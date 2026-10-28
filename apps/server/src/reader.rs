//! Telemetry reader: reassembles the DUML on ep1 (OUT, RC → Orin) and folds it
//! into the shared [`Telemetry`]. It also demuxes the RC's USB packet mux
//! ([`rcmux`]) and hands the camera video channels to their [`Feed`]s.
//!
//! `RC_DUMP=<file>` also appends every raw ep1 byte to `<file>` (up to
//! [`DUMP_CAP`]) for offline analysis, e.g. of the non-DUML video downlink.

use std::fs::File;
use std::io::{ErrorKind, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use dji_common::logln;
use dji_protocol::Telemetry;
use dji_protocol::{duml, rcmux};
use dji_transport::usb;

use crate::video::Feed;

pub type Tele = Arc<Mutex<Telemetry>>;

/// Size limit for an `RC_DUMP` file, so a forgotten dump can't fill the disk.
const DUMP_CAP: u64 = 512 * 1024 * 1024;

/// Raw ep1 bytes appended to a file, stopping at [`DUMP_CAP`].
struct Dump {
    file: File,
    written: u64,
}

impl Dump {
    fn from_env() -> Option<Self> {
        let path = std::env::var("RC_DUMP").ok()?;
        match File::options().create(true).append(true).open(&path) {
            Ok(file) => {
                logln!("telemetry: dumping raw ep1 to {path}");
                Some(Self { file, written: 0 })
            }
            Err(e) => {
                logln!("telemetry: cannot open RC_DUMP {path}: {e}");
                None
            }
        }
    }

    /// Append `data`; `false` once the dump is finished (cap reached or error).
    fn write(&mut self, data: &[u8]) -> bool {
        if self.written + data.len() as u64 > DUMP_CAP {
            logln!("telemetry: RC_DUMP reached {DUMP_CAP} bytes; stopped");
            return false;
        }
        self.written += data.len() as u64;
        self.file
            .write_all(data)
            .map_err(|e| logln!("telemetry: RC_DUMP write failed: {e}"))
            .is_ok()
    }
}

/// Read until the endpoint is gone (`ENODEV`). `ESHUTDOWN` (the RC disabled the
/// gadget, e.g. it was power-cycled) is waited out, so the reader resumes on
/// the next ENABLE without being respawned.
///
/// The DUML decoder scans the raw bytes (it skips the mux headers and video by
/// CRC), so telemetry does not depend on the mux layout; `feeds` get the
/// payloads of their RC channels.
pub fn run(ep1: OwnedFd, tele: Tele, feeds: Vec<Arc<Feed>>) {
    let mut decoder = duml::Decoder::new();
    let mut demux = rcmux::Demux::new();
    let mut buf = vec![0u8; 16384];
    let mut dump = Dump::from_env();
    loop {
        match usb::read(ep1.as_fd(), &mut buf) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
                if dump.as_mut().is_some_and(|d| !d.write(&buf[..n])) {
                    dump = None;
                }
                if !feeds.is_empty() {
                    demux.push(&buf[..n], |ch, payload| {
                        if let Some(f) = feeds.iter().find(|f| f.rc_channel() == Some(ch)) {
                            f.offer(payload);
                        }
                    });
                }
                decoder.push(&buf[..n]);
                let mut t = tele.lock().unwrap();
                decoder.drain(|f| {
                    if f.crc_ok {
                        t.update(f);
                    }
                });
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == usb::errno::ENODEV {
                    logln!("telemetry: ep1 gone; reader stopping");
                    return;
                }
                if code == usb::errno::ESHUTDOWN {
                    // Any partial frame belongs to the old session.
                    decoder = duml::Decoder::new();
                    demux = rcmux::Demux::new();
                }
                sleep(Duration::from_millis(20));
            }
        }
    }
}
