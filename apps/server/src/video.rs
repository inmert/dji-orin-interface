//! Live camera video: raw H.264 from the aircraft, decoded on the Orin's
//! hardware decoder to JPEG frames for the browser (MJPEG at
//! `/video/<feed>.mjpg`, see [`crate::web::http`]).
//!
//! Two sources, both plain H.264 byte streams (Annex B):
//! * [`Source::Udt`] — the FPV camera over UDT on the aircraft's USB network
//!   (`192.168.42.2:40003`). The connection is made only while someone
//!   watches, so nothing is sent to the aircraft otherwise.
//! * [`Source::Rc`] — a video channel of the RC's USB mux
//!   ([`dji_protocol::rcmux`]): the gimbal cameras. The reader already
//!   receives these bytes; it hands them over via [`Feed::offer`], which
//!   costs nothing while nobody watches. Receive-only.
//!
//! A feed decodes only while it has viewers and stops [`IDLE_GRACE`] after
//! the last one leaves. Decoding runs in a `gst-launch-1.0` child on NVIDIA's
//! `nvv4l2decoder` ([`Decoder`]). The cameras use intra refresh (few or no IDR
//! frames), which that decoder only accepts with `is-gdr-stream=true`. JPEG
//! frames travel over HTTP; each viewer gets the newest frame, never a backlog.
//!
//! ```text
//!   UDT / RC channel ─▶ pump ─▶ NalFilter ─stdin─▶ gst (NVDEC → JPEG) ─fd 3─▶ splitter ─▶ viewers
//!                                                      └─stdout/stderr─▶ diagnostics (logged, never video)
//! ```

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddrV4;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{JoinHandle, sleep};
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_common::logln;
use dji_udt::Stream;

/// How long a feed stays up after the last viewer leaves (covers a reload).
const IDLE_GRACE: Duration = Duration::from_secs(3);
/// Wait between UDT connection attempts while a viewer is waiting.
const RETRY: Duration = Duration::from_secs(3);
/// Wait before restarting a failed decoder on an RC feed (no connection to redo).
const RC_RETRY: Duration = Duration::from_millis(200);
/// Upper bound on one wait for input, so the pump notices viewers leaving.
const RECV_TIMEOUT: Duration = Duration::from_millis(200);
/// A source that delivers nothing for this long is reported as stalled.
const STALL: Duration = Duration::from_secs(5);
/// RC payloads queued for the decoder; more are dropped (counted) rather than
/// letting a slow decoder stall the RC reader.
const RC_QUEUE: usize = 512;
/// Diagnostic lines from the decoder logged per run; the rest are counted.
/// NVIDIA's GDR mode prints a "Frame N" line for every frame.
const DIAG_LOG_LINES: usize = 8;
/// Reject malformed units before they can consume unbounded memory/CPU.
const MAX_NAL: usize = 2 * 1024 * 1024;

pub struct Frame {
    pub jpeg: Vec<u8>,
    pub sequence: u64,
    /// Monotonic decode time, relative to this feed's creation (not wall clock).
    pub timestamp_ms: u64,
}

/// Where a feed's H.264 comes from.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// UDT stream from the aircraft's USB network.
    Udt(SocketAddrV4),
    /// A video channel of the RC's USB mux.
    Rc(u8),
}

impl Source {
    fn describe(self) -> String {
        match self {
            Source::Udt(addr) => format!("UDT {addr}"),
            Source::Rc(ch) => format!("RC channel 0x{ch:02x}"),
        }
    }
}

/// Feed state for the UI.
#[derive(Clone, Debug, Serialize)]
pub struct VideoStatus {
    /// Where the video comes from, e.g. `UDT 192.168.42.2:40003`.
    pub source: String,
    /// `idle` (no viewers), `connecting`, `streaming` or `error`.
    pub state: &'static str,
    pub viewers: usize,
    /// JPEG frames decoded since the server started.
    pub frames: u64,
    /// H.264 bytes received since the server started.
    pub bytes: u64,
    /// RC payloads dropped because the decoder fell behind.
    pub dropped: u64,
    /// Last connection, stall or decoder error, cleared once streaming.
    pub error: Option<String>,
}

struct Inner {
    status: VideoStatus,
    running: bool,
    /// The newest decoded frame.
    latest: Option<Arc<Frame>>,
}

/// One video feed: viewers subscribe, one worker decodes for them.
pub struct Feed {
    name: &'static str,
    epoch: Instant,
    source: Source,
    /// Output size for the JPEGs, or `None` to keep the stream's own.
    size: Option<(u32, u32)>,
    inner: Mutex<Inner>,
    new_frame: Condvar,
    /// Where [`Feed::offer`] sends RC payloads while the feed is decoding.
    rc_tx: Mutex<Option<SyncSender<Vec<u8>>>>,
}

impl Feed {
    pub fn new(name: &'static str, source: Source, size: Option<(u32, u32)>) -> Arc<Self> {
        Arc::new(Self {
            name,
            epoch: Instant::now(),
            source,
            size,
            inner: Mutex::new(Inner {
                status: VideoStatus {
                    source: source.describe(),
                    state: "idle",
                    viewers: 0,
                    frames: 0,
                    bytes: 0,
                    dropped: 0,
                    error: None,
                },
                running: false,
                latest: None,
            }),
            new_frame: Condvar::new(),
            rc_tx: Mutex::new(None),
        })
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The RC channel this feed takes, if it is an RC feed.
    pub fn rc_channel(&self) -> Option<u8> {
        match self.source {
            Source::Rc(ch) => Some(ch),
            Source::Udt(_) => None,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    pub fn status(&self) -> VideoStatus {
        self.lock().status.clone()
    }

    fn set_state(&self, state: &'static str, error: Option<String>) {
        let mut inner = self.lock();
        inner.status.state = state;
        inner.status.error = error;
    }

    /// Hand over a payload of this feed's RC channel. Dropped unless the feed
    /// is decoding; called from the RC reader, so it never blocks.
    pub fn offer(&self, payload: &[u8]) {
        let tx = self.rc_tx.lock().unwrap();
        if let Some(tx) = tx.as_ref()
            && let Err(TrySendError::Full(_)) = tx.try_send(payload.to_vec())
        {
            self.lock().status.dropped += 1;
        }
    }

    /// Register a viewer, starting the worker if it isn't running.
    pub fn subscribe(self: &Arc<Self>) -> Viewer {
        let mut inner = self.lock();
        inner.status.viewers += 1;
        if !inner.running {
            inner.running = true;
            let feed = Arc::clone(self);
            std::thread::spawn(move || feed.worker());
        }
        Viewer {
            feed: Arc::clone(self),
            seen: inner.status.frames,
        }
    }

    fn viewers(&self) -> usize {
        self.lock().status.viewers
    }

    fn publish(&self, jpeg: Vec<u8>) {
        let mut inner = self.lock();
        inner.status.frames += 1;
        let sequence = inner.status.frames;
        inner.latest = Some(Arc::new(Frame {
            jpeg,
            sequence,
            timestamp_ms: self.epoch.elapsed().as_millis() as u64,
        }));
        inner.status.state = "streaming";
        inner.status.error = None;
        drop(inner);
        self.new_frame.notify_all();
    }

    /// True once the viewers have been gone for [`IDLE_GRACE`].
    fn idle(&self, last_viewer: &mut Instant) -> bool {
        if self.viewers() > 0 {
            *last_viewer = Instant::now();
        }
        last_viewer.elapsed() >= IDLE_GRACE
    }

    /// Open the source, decode and publish while anyone watches; retry on
    /// failure; exit after [`IDLE_GRACE`] without viewers.
    fn worker(self: Arc<Self>) {
        let name = self.name;
        let mut last_viewer = Instant::now();
        loop {
            if self.idle(&mut last_viewer) {
                let mut inner = self.lock();
                // A viewer may have arrived since the check; keep going if so.
                if inner.status.viewers == 0 {
                    inner.running = false;
                    inner.status.state = "idle";
                    logln!("video {name}: no viewers; stopped");
                    return;
                }
                continue;
            }
            self.set_state("connecting", self.status().error);
            logln!("video {name}: opening {}", self.source.describe());
            let result = self
                .open()
                .and_then(|input| self.stream(input, &mut last_viewer));
            *self.rc_tx.lock().unwrap() = None;
            match result {
                Ok(()) => self.set_state("idle", None),
                Err(e) => {
                    logln!("video {name}: {e}");
                    self.set_state("error", Some(e));
                    sleep(match self.source {
                        Source::Udt(_) => RETRY,
                        Source::Rc(_) => RC_RETRY,
                    });
                }
            }
        }
    }

    fn open(&self) -> Result<Input, String> {
        match self.source {
            Source::Udt(addr) => Stream::connect(addr, RECV_TIMEOUT).map(Input::Udt),
            Source::Rc(_) => {
                let (tx, rx) = sync_channel(RC_QUEUE);
                *self.rc_tx.lock().unwrap() = Some(tx);
                Ok(Input::Rc(rx))
            }
        }
    }

    /// Pump one input through the decoder until it fails, stalls or the
    /// viewers have been gone for [`IDLE_GRACE`]. A stalled RC channel keeps
    /// waiting (the RC may just be unplugged); a stalled UDT stream reconnects.
    fn stream(self: &Arc<Self>, mut input: Input, last_viewer: &mut Instant) -> Result<(), String> {
        let mut gst = Decoder::spawn(Arc::clone(self))?;
        let mut nals = NalFilter::default();
        let mut last_data = Instant::now();
        let mut flowing = false;
        loop {
            if self.idle(last_viewer) {
                return Ok(());
            }
            let Some(chunk) = input.next()? else {
                if flowing && last_data.elapsed() >= STALL {
                    let msg = format!("no video for {} s", STALL.as_secs());
                    if matches!(input, Input::Udt(_)) {
                        return Err(msg);
                    }
                    self.set_state("error", Some(msg));
                    flowing = false;
                }
                continue;
            };
            if !flowing {
                logln!("video {}: receiving; decoding", self.name);
                flowing = true;
            }
            last_data = Instant::now();
            self.lock().status.bytes += chunk.len() as u64;
            gst.write(&nals.push(&chunk))?;
        }
    }
}

/// An open source of H.264 bytes.
enum Input {
    Udt(Stream),
    Rc(Receiver<Vec<u8>>),
}

impl Input {
    /// The next chunk, `None` after [`RECV_TIMEOUT`] without data.
    fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        match self {
            Input::Udt(udt) => {
                let mut buf = vec![0u8; 128 * 1024];
                let n = udt.recv(&mut buf)?;
                buf.truncate(n);
                Ok((n > 0).then_some(buf))
            }
            Input::Rc(rx) => match rx.recv_timeout(RECV_TIMEOUT) {
                Ok(chunk) => Ok(Some(chunk)),
                Err(RecvTimeoutError::Timeout) => Ok(None),
                Err(RecvTimeoutError::Disconnected) => Err("RC feed closed".into()),
            },
        }
    }
}

/// The server's feeds by name, as the UI and the HTTP routes know them.
pub type Feeds = BTreeMap<&'static str, Arc<Feed>>;

/// The feeds `[video]` configures (see `config.toml`). Nothing is opened
/// until a viewer subscribes.
pub fn feeds(cfg: &crate::config::Video) -> Result<Feeds, String> {
    let mut feeds = Feeds::new();
    if let Some(addr) = cfg.fpv()? {
        // 608×448 already: keep the stream's own size.
        feeds.insert("fpv", Feed::new("fpv", Source::Udt(addr), None));
    }
    for (name, &ch) in &cfg.rc {
        let name: &'static str = match name.as_str() {
            "cam1" => "cam1",
            "cam2" => "cam2",
            "fpv" => "fpv",
            other => return Err(format!("video.rc: no feed {other:?} (cam1, cam2, fpv)")),
        };
        // The gimbal cameras' 1280×720 streams are scaled down to keep the
        // MJPEG light; the FPV's 608×448 keeps its size. An RC FPV feed
        // replaces the UDT one.
        let scale = (name != "fpv").then_some((640, 360));
        feeds.insert(name, Feed::new(name, Source::Rc(ch), scale));
    }
    Ok(feeds)
}

/// Status of every feed, for the WebSocket snapshot.
pub fn statuses(feeds: &Feeds) -> BTreeMap<&'static str, VideoStatus> {
    feeds.iter().map(|(name, f)| (*name, f.status())).collect()
}

/// The `gst-launch-1.0` pipeline for a feed: H.264 on stdin, JPEGs on fd 3.
fn gst_pipeline(size: Option<(u32, u32)>) -> String {
    let caps = match size {
        Some((w, h)) => format!("video/x-raw(memory:NVMM),width={w},height={h},format=I420"),
        None => "video/x-raw(memory:NVMM),format=I420".into(),
    };
    format!(
        "fdsrc fd=0 ! h264parse ! video/x-h264,stream-format=byte-stream,alignment=au ! \
         nvv4l2decoder is-gdr-stream=true ! \
         nvvidconv ! '{caps}' ! nvjpegenc quality=70 ! \
         fdsink fd=3 sync=false"
    )
}

/// The decoder child: `gst-launch-1.0` with NVIDIA's hardware H.264 decoder.
///
/// NVIDIA's elements print to stdout ("Opening in BLOCKING MODE", a "Frame N"
/// line per GDR frame) even with `-q`, so the video must not travel on
/// stdout. The child is started as `sh -c 'exec gst-launch-1.0 … 3>&1 1>&2'`:
/// fd 3 takes over our stdout pipe for the JPEGs (`fdsink fd=3`), and the
/// child's own stdout joins stderr, our separate diagnostics pipe. `exec`
/// makes the shell become gst, so killing the child kills the decoder. The
/// parent holds only the read ends (plus stdin's write end); both readers run
/// on their own threads until EOF, so a full pipe can never stall the child,
/// and dropping the decoder closes stdin, kills, reaps and joins them.
struct Decoder {
    child: Child,
    input: Option<UnixStream>,
    threads: Vec<JoinHandle<()>>,
}

impl Decoder {
    fn spawn(feed: Arc<Feed>) -> Result<Self, String> {
        let script = format!(
            "exec gst-launch-1.0 -q {} 3>&1 1>&2",
            gst_pipeline(feed.size)
        );
        // A socket supplies bounded writes; ChildStdin's pipe can block forever.
        let (input, child_input) = UnixStream::pair().map_err(|e| e.to_string())?;
        input.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut child = Command::new("sh")
            .args(["-c", &script])
            .stdin(Stdio::from(OwnedFd::from(child_input)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start gst-launch-1.0: {e}"))?;
        let mut video = child.stdout.take().expect("piped stdout");
        let diag = child.stderr.take().expect("piped stderr");
        let name = feed.name;
        let splitter = std::thread::spawn(move || {
            let mut split = JpegSplitter::default();
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n @ 1..) = video.read(&mut buf) {
                for jpeg in split.push(&buf[..n]) {
                    feed.publish(jpeg);
                }
            }
        });
        let diagnostics = std::thread::spawn(move || drain_diagnostics(name, diag));
        Ok(Self {
            child,
            input: Some(input),
            threads: vec![splitter, diagnostics],
        })
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        write_decoder(self.input.as_mut().expect("decoder input"), data, STALL)
            .map_err(|e| format!("decoder stopped: {e}"))
    }
}

/// Bound the whole write, including a decoder that makes only partial progress.
fn write_decoder(
    input: &mut UnixStream,
    mut data: &[u8],
    timeout: Duration,
) -> std::io::Result<()> {
    let start = Instant::now();
    while !data.is_empty() {
        if start.elapsed() >= timeout {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "decoder input stalled",
            ));
        }
        match input.write(data) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl Drop for Decoder {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }
}

/// Read the decoder's stdout+stderr to EOF: log the first few lines, count
/// the rest. Errors and warnings past that limit are still logged.
fn drain_diagnostics(name: &str, diag: impl Read) {
    let mut lines = 0usize;
    for line in BufReader::new(diag).lines() {
        let Ok(line) = line else { break };
        lines += 1;
        let line = line.trim();
        let notable = line.contains("ERROR") || line.contains("WARN");
        if !line.is_empty() && (lines <= DIAG_LOG_LINES || notable) {
            logln!("video {name}: gst: {line}");
        }
    }
    if lines > DIAG_LOG_LINES {
        logln!("video {name}: gst: {lines} diagnostic lines in total");
    }
}

/// One subscribed viewer; unsubscribes on drop.
pub struct Viewer {
    feed: Arc<Feed>,
    seen: u64,
}

impl Viewer {
    /// The newest frame, once it is newer than the last one returned (frames
    /// published in between are skipped), or `None` if none arrives within
    /// `timeout`.
    pub fn next_frame(&mut self, timeout: Duration) -> Option<Arc<Frame>> {
        let inner = self.feed.lock();
        let (inner, _) = self
            .feed
            .new_frame
            .wait_timeout_while(inner, timeout, |i| i.status.frames == self.seen)
            .unwrap();
        if inner.status.frames == self.seen {
            return None;
        }
        let frame = inner.latest.clone()?;
        self.seen = frame.sequence;
        Some(frame)
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.feed.lock().status.viewers -= 1;
    }
}

/// Passes an H.264 Annex B byte stream through whole NAL units at a time,
/// dropping any unit whose forbidden bit is set. Such units came from the
/// RC demuxer splicing outer-frame headers into video packets (fixed in
/// [`dji_protocol::rcmux`]); one that looked like `00 00 01 a7…` reached the
/// parser as a bogus SPS and killed the decoder. Kept as a guard against
/// lost or damaged USB data. A unit is released when the next start code
/// arrives (about one frame of delay).
#[derive(Default)]
struct NalFilter {
    buf: Vec<u8>,
}

impl NalFilter {
    fn push(&mut self, data: &[u8]) -> Vec<u8> {
        if data.len() > MAX_NAL {
            return data
                .chunks(MAX_NAL)
                .flat_map(|chunk| self.push(chunk))
                .collect();
        }
        self.buf.extend_from_slice(data);
        let mut out = Vec::with_capacity(self.buf.len());
        // Units start at a 3-byte start code; a 4-byte one's leading zero
        // ends the previous unit, which the decoder ignores.
        let starts: Vec<usize> = self
            .buf
            .windows(3)
            .enumerate()
            .filter(|(_, w)| *w == [0, 0, 1])
            .map(|(i, _)| i)
            .collect();
        let Some(&last) = starts.last() else {
            // No start code yet: keep a possible partial one, drop the rest.
            let keep = self.buf.len().min(2);
            self.buf.drain(..self.buf.len() - keep);
            return out;
        };
        for pair in starts.windows(2) {
            let unit = &self.buf[pair[0]..pair[1]];
            if unit.len() <= MAX_NAL && unit.get(3).is_some_and(|h| h & 0x80 == 0) {
                out.extend_from_slice(unit);
            }
        }
        self.buf.drain(..last);
        if self.buf.len() > MAX_NAL {
            // Discard the oversized unit, retaining only a possible split start code.
            self.buf.drain(..self.buf.len() - 2);
        }
        out
    }
}

/// Splits a byte stream of back-to-back baseline JPEGs at their SOI/EOI
/// markers. Inside the entropy-coded data a 0xFF byte is always stuffed or a
/// restart marker, so `FF D9` only ever ends an image.
#[derive(Default)]
struct JpegSplitter {
    buf: Vec<u8>,
}

impl JpegSplitter {
    fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let Some(start) = find(&self.buf, &[0xff, 0xd8], 0) else {
                // Keep a trailing 0xFF: it may be half of the next SOI.
                let keep = usize::from(self.buf.last() == Some(&0xff));
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            let Some(end) = find(&self.buf, &[0xff, 0xd9], start + 2) else {
                self.buf.drain(..start);
                break;
            };
            out.push(self.buf[start..end + 2].to_vec());
            self.buf.drain(..end + 2);
        }
        out
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &[u8] = &[0xff, 0xd8, 1, 2, 0xff, 0x00, 3, 0xff, 0xd9];
    const B: &[u8] = &[0xff, 0xd8, 9, 0xff, 0xd0, 8, 0xff, 0xd9];

    #[test]
    fn stalled_decoder_input_times_out() {
        let (mut writer, _unread) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        let start = Instant::now();
        let error = write_decoder(
            &mut writer,
            &vec![0; 4 * 1024 * 1024],
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn nal_filter_bounds_corrupt_units_and_recovers() {
        let mut f = NalFilter::default();
        f.push(&[0, 0, 1, 0x41]);
        let garbage = vec![0x55; 16384];
        for _ in 0..(MAX_NAL / garbage.len() * 3) {
            assert!(f.push(&garbage).is_empty());
            assert!(f.buf.len() <= MAX_NAL);
        }
        assert_eq!(
            f.push(&[0, 0, 1, 0x67, 42, 0, 0, 1, 0x09]),
            [0, 0, 1, 0x67, 42]
        );
    }

    #[test]
    fn splits_back_to_back_images() {
        let mut s = JpegSplitter::default();
        let stream = [A, B].concat();
        assert_eq!(s.push(&stream), vec![A.to_vec(), B.to_vec()]);
    }

    #[test]
    fn reassembles_across_every_split_point() {
        let stream = [&[0x00, 0x42][..], A, B].concat();
        for cut in 0..=stream.len() {
            let mut s = JpegSplitter::default();
            let mut got = s.push(&stream[..cut]);
            got.extend(s.push(&stream[cut..]));
            assert_eq!(got, vec![A.to_vec(), B.to_vec()], "cut at {cut}");
        }
    }

    #[test]
    fn nal_filter_drops_forbidden_units_and_keeps_order() {
        let good1 = [0, 0, 0, 1, 0x67, 0x42, 0x00];
        let bad = [0, 0, 1, 0xa7, 0xb2, 0x55];
        let good2 = [0, 0, 1, 0x41, 0x9a, 0x01];
        let tail = [0, 0, 1, 0x09, 0x10];
        let stream = [&good1[..], &bad, &good2, &tail].concat();
        for cut in 0..=stream.len() {
            let mut f = NalFilter::default();
            let mut out = f.push(&stream[..cut]);
            out.extend(f.push(&stream[cut..]));
            // Units start at the 3-byte start code, so the 4-byte code's
            // leading zero goes; `tail` waits for the next start code.
            assert_eq!(out, [&good1[1..], &good2].concat(), "cut at {cut}");
        }
    }

    #[test]
    fn byte_at_a_time_input_gives_the_same_units_and_images() {
        let stream = [
            &[0, 0, 0, 1, 0x67, 0x42, 0, 0, 1][..], // 4-byte code; the next code split across lines
            &[0x68, 0xce, 0, 0, 3, 1],              // emulation prevention: not a start code
            &[0, 0, 1, 0xe5, 0x00],                 // forbidden bit: dropped
            &[0, 0, 0, 1, 0x65, 0x88, 0x84],
            &[0, 0, 1, 0x09, 0x10], // releases the unit before it
        ]
        .concat();
        let mut whole = NalFilter::default();
        let want = whole.push(&stream);
        assert_eq!(
            want,
            [
                &[0, 0, 1, 0x67, 0x42][..],
                &[0, 0, 1, 0x68, 0xce, 0, 0, 3, 1],
                &[0, 0, 1, 0x65, 0x88, 0x84]
            ]
            .concat()
        );
        let mut f = NalFilter::default();
        let got: Vec<u8> = stream.iter().flat_map(|b| f.push(&[*b])).collect();
        assert_eq!(got, want);

        let images = [A, B].concat();
        let mut s = JpegSplitter::default();
        let got: Vec<Vec<u8>> = images.iter().flat_map(|b| s.push(&[*b])).collect();
        assert_eq!(got, vec![A.to_vec(), B.to_vec()]);
    }

    #[test]
    fn pipeline_scales_only_when_asked_and_writes_video_to_fd_3() {
        let scaled = gst_pipeline(Some((640, 360)));
        assert!(scaled.contains("width=640,height=360,format=I420"));
        assert!(scaled.contains("is-gdr-stream=true"));
        assert!(scaled.ends_with("fdsink fd=3 sync=false"));
        assert!(gst_pipeline(None).contains("video/x-raw(memory:NVMM),format=I420"));
    }

    #[test]
    fn viewer_gets_the_newest_frame_and_never_a_backlog() {
        let feed = Feed::new("test", Source::Rc(0), None);
        // Construct a viewer without starting a worker or opening any device.
        feed.lock().status.viewers = 1;
        let mut viewer = Viewer {
            feed: Arc::clone(&feed),
            seen: 0,
        };
        assert!(viewer.next_frame(Duration::ZERO).is_none());
        feed.publish(vec![0]);
        assert_eq!(viewer.next_frame(Duration::ZERO).unwrap().jpeg, vec![0]);
        assert!(
            viewer.next_frame(Duration::ZERO).is_none(),
            "each frame sent once"
        );
        for n in 1..20 {
            feed.publish(vec![n]);
        }
        // A slow viewer skips straight to the newest frame.
        let frame = viewer.next_frame(Duration::ZERO).unwrap();
        assert_eq!((frame.jpeg.clone(), frame.sequence), (vec![19], 20));
        assert!(viewer.next_frame(Duration::ZERO).is_none());
        assert_eq!(feed.status().state, "streaming");
    }

    /// Runs the real decoder on a captured stream, exactly as the server does
    /// (same pipeline, fd 3 for video, NalFilter in front):
    /// `VIDEO_TEST_H264=<file.h264> cargo test -p dji-server -- --ignored`.
    #[test]
    #[ignore = "needs a capture file and the NVIDIA GStreamer decoder"]
    fn hardware_decoder_produces_clean_jpegs_from_a_capture() {
        let path = std::env::var("VIDEO_TEST_H264").expect("set VIDEO_TEST_H264");
        let data = std::fs::read(&path).expect("read capture");
        let feed = Feed::new("test", Source::Rc(0), Some((640, 360)));
        let mut gst = Decoder::spawn(Arc::clone(&feed)).expect("start decoder");
        let mut nals = NalFilter::default();
        for chunk in data.chunks(16384) {
            gst.write(&nals.push(chunk)).expect("feed decoder");
        }
        // EOF on stdin lets gst flush its last frames before it exits.
        drop(gst.input.take());
        let _ = gst.child.wait();
        drop(gst);
        let frames = feed.status().frames;
        let frame = feed.lock().latest.clone().expect("at least one frame");
        let jpeg = &frame.jpeg;
        println!("{path}: {frames} JPEG frames, last {} bytes", jpeg.len());
        assert!(frames > 0);
        assert!(jpeg.starts_with(&[0xff, 0xd8]) && jpeg.ends_with(&[0xff, 0xd9]));
        assert!(!jpeg.windows(8).any(|w| w == b"BLOCKING"));
    }
}
