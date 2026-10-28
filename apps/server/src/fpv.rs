//! FPV camera video: the aircraft's raw H.264 over UDT, decoded to JPEG frames
//! for the browser (MJPEG at `/fpv.mjpg`, see [`crate::http`]).
//!
//! The UDT connection is made only while at least one viewer is watching and
//! dropped [`IDLE_GRACE`] after the last one leaves, so nothing is sent to the
//! aircraft when nobody looks. Decoding runs in a `gst-launch-1.0` child
//! (`avdec_h264`): the stream has no IDR frames (intra refresh), which the
//! decoder handles from the first SPS on, and the Orin's FFmpeg has no software
//! H.264 decoder. JPEG suits the browser: an `<img>` shows MJPEG over plain
//! HTTP, where WebCodecs would need a secure context.
//!
//! ```text
//!   aircraft 192.168.42.2:40003 ─UDT─▶ pump ─stdin─▶ gst (H.264→JPEG) ─stdout─▶ splitter ─▶ viewers
//! ```

use std::io::{Read, Write};
use std::net::SocketAddrV4;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{JoinHandle, sleep};
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_common::logln;
use dji_udt::Stream;

/// How long the stream stays up after the last viewer leaves (covers a reload).
const IDLE_GRACE: Duration = Duration::from_secs(3);
/// Wait between connection attempts while a viewer is waiting.
const RETRY: Duration = Duration::from_secs(3);
/// Upper bound on one UDT receive, so the pump notices viewers leaving.
const RECV_TIMEOUT: Duration = Duration::from_millis(200);
/// A connected stream that delivers nothing for this long is dropped and retried.
const STALL: Duration = Duration::from_secs(5);
/// Decoder pipeline: H.264 byte stream on stdin, concatenated JPEGs on stdout.
const GST_PIPELINE: &str = "-q fdsrc fd=0 ! h264parse ! avdec_h264 max-threads=1 ! \
    videoconvert ! jpegenc quality=75 ! fdsink fd=1 sync=false";

/// Stream state for the UI.
#[derive(Clone, Debug, Serialize)]
pub struct FpvStatus {
    /// Aircraft address, e.g. `192.168.42.2:40003`.
    pub addr: String,
    /// `idle` (no viewers), `connecting`, `streaming` or `error`.
    pub state: &'static str,
    pub viewers: usize,
    /// JPEG frames decoded since the server started.
    pub frames: u64,
    /// H.264 bytes received since the server started.
    pub bytes: u64,
    /// Last connection or decoder error, cleared once streaming.
    pub error: Option<String>,
}

struct Inner {
    status: FpvStatus,
    running: bool,
    /// Latest JPEG and its sequence number (`frames`).
    frame: Option<Arc<Vec<u8>>>,
}

/// The shared FPV stream: viewers subscribe, one worker feeds them.
pub struct Fpv {
    addr: SocketAddrV4,
    inner: Mutex<Inner>,
    new_frame: Condvar,
}

impl Fpv {
    pub fn new(addr: SocketAddrV4) -> Arc<Self> {
        Arc::new(Self {
            addr,
            inner: Mutex::new(Inner {
                status: FpvStatus {
                    addr: addr.to_string(),
                    state: "idle",
                    viewers: 0,
                    frames: 0,
                    bytes: 0,
                    error: None,
                },
                running: false,
                frame: None,
            }),
            new_frame: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    pub fn status(&self) -> FpvStatus {
        self.lock().status.clone()
    }

    fn set_state(&self, state: &'static str, error: Option<String>) {
        let mut inner = self.lock();
        inner.status.state = state;
        inner.status.error = error;
    }

    /// Register a viewer, starting the worker if it isn't running.
    pub fn subscribe(self: &Arc<Self>) -> Viewer {
        let mut inner = self.lock();
        inner.status.viewers += 1;
        if !inner.running {
            inner.running = true;
            let fpv = Arc::clone(self);
            std::thread::spawn(move || fpv.worker());
        }
        Viewer {
            fpv: Arc::clone(self),
            seen: inner.status.frames,
        }
    }

    fn viewers(&self) -> usize {
        self.lock().status.viewers
    }

    fn publish(&self, jpeg: Vec<u8>) {
        let mut inner = self.lock();
        inner.status.frames += 1;
        inner.frame = Some(Arc::new(jpeg));
        drop(inner);
        self.new_frame.notify_all();
    }

    /// Connect, decode and publish while anyone watches; retry on failure;
    /// exit after [`IDLE_GRACE`] without viewers.
    fn worker(self: Arc<Self>) {
        let mut last_viewer = Instant::now();
        loop {
            if self.viewers() > 0 {
                last_viewer = Instant::now();
            } else if last_viewer.elapsed() >= IDLE_GRACE {
                let mut inner = self.lock();
                // A viewer may have arrived since the check; keep going if so.
                if inner.status.viewers == 0 {
                    inner.running = false;
                    inner.status.state = "idle";
                    logln!("fpv: no viewers; stream closed");
                    return;
                }
                continue;
            }
            self.set_state("connecting", self.status().error);
            logln!("fpv: connecting to {}", self.addr);
            let result = Stream::connect(self.addr, RECV_TIMEOUT)
                .and_then(|udt| self.stream(udt, &mut last_viewer));
            match result {
                Ok(()) => self.set_state("idle", None),
                Err(e) => {
                    logln!("fpv: {e}");
                    self.set_state("error", Some(e));
                    sleep(RETRY);
                }
            }
        }
    }

    /// Pump one UDT connection through the decoder until it fails, stalls or
    /// the viewers have been gone for [`IDLE_GRACE`].
    fn stream(self: &Arc<Self>, mut udt: Stream, last_viewer: &mut Instant) -> Result<(), String> {
        logln!("fpv: connected; decoding");
        let mut gst = Decoder::spawn(Arc::clone(self))?;
        self.set_state("streaming", None);
        let mut buf = vec![0u8; 128 * 1024];
        let mut last_data = Instant::now();
        loop {
            if self.viewers() > 0 {
                *last_viewer = Instant::now();
            } else if last_viewer.elapsed() >= IDLE_GRACE {
                return Ok(());
            }
            let n = udt.recv(&mut buf)?;
            if n == 0 {
                if last_data.elapsed() >= STALL {
                    return Err(format!("no video for {} s", STALL.as_secs()));
                }
                continue;
            }
            last_data = Instant::now();
            self.lock().status.bytes += n as u64;
            gst.write(&buf[..n])?;
        }
    }
}

/// The `gst-launch-1.0` decoder child and the thread splitting its output.
struct Decoder {
    child: Child,
    splitter: Option<JoinHandle<()>>,
}

impl Decoder {
    fn spawn(fpv: Arc<Fpv>) -> Result<Self, String> {
        let mut child = Command::new("gst-launch-1.0")
            .args(GST_PIPELINE.split_whitespace())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start gst-launch-1.0: {e}"))?;
        let mut out = child.stdout.take().expect("piped stdout");
        let splitter = std::thread::spawn(move || {
            let mut split = JpegSplitter::default();
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n @ 1..) = out.read(&mut buf) {
                for jpeg in split.push(&buf[..n]) {
                    fpv.publish(jpeg);
                }
            }
        });
        Ok(Self {
            child,
            splitter: Some(splitter),
        })
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let stdin = self.child.stdin.as_mut().expect("piped stdin");
        stdin
            .write_all(data)
            .map_err(|e| format!("decoder stopped: {e}"))
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(h) = self.splitter.take() {
            let _ = h.join();
        }
    }
}

/// One subscribed viewer; unsubscribes on drop.
pub struct Viewer {
    fpv: Arc<Fpv>,
    seen: u64,
}

impl Viewer {
    /// The next frame newer than the last one returned, or `None` if none
    /// arrives within `timeout`.
    pub fn next_frame(&mut self, timeout: Duration) -> Option<Arc<Vec<u8>>> {
        let inner = self.fpv.lock();
        let (inner, _) = self
            .fpv
            .new_frame
            .wait_timeout_while(inner, timeout, |i| i.status.frames == self.seen)
            .unwrap();
        if inner.status.frames == self.seen {
            return None;
        }
        self.seen = inner.status.frames;
        inner.frame.clone()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.fpv.lock().status.viewers -= 1;
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
    use super::JpegSplitter;

    const A: &[u8] = &[0xff, 0xd8, 1, 2, 0xff, 0x00, 3, 0xff, 0xd9];
    const B: &[u8] = &[0xff, 0xd8, 9, 0xff, 0xd0, 8, 0xff, 0xd9];

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
}
