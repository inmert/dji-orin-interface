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
use dji_protocol::{duml, flyc, rcmux, thermal};
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
/// [`rcmux`] splits the stream: DUML arrives on two separate byte streams
/// (the mux's DUML channel, and outer frames sent outside the mux), each with
/// its own decoder so a frame split across packets is reassembled; `feeds`
/// get the payloads of their RC channels.
pub fn run(ep1: OwnedFd, tele: Tele, feeds: Vec<Arc<Feed>>) {
    // One decoder per DUML byte stream: [mux channel, direct].
    let mut decoders = [duml::Decoder::new(), duml::Decoder::new()];
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
                demux.push(&buf[..n], |ch, payload| match ch {
                    rcmux::DUML => decoders[0].push(payload),
                    rcmux::DUML_DIRECT => decoders[1].push(payload),
                    _ => {
                        if let Some(f) = feeds.iter().find(|f| f.rc_channel() == Some(ch)) {
                            f.offer(payload);
                        }
                    }
                });
                let mut t = tele.lock().unwrap();
                let mut each = |f: &duml::Frame| {
                    if f.crc_ok {
                        t.update(f);
                        if f.cmd_set == flyc::CMD_SET && f.cmd_id == flyc::CMD_ID_FUNCTION_CONTROL {
                            logln!(
                                "flyc: 0x03/0x2a {} seq {} type {:#04x} payload {:02x?}",
                                duml::DevName(f.sender),
                                f.seq,
                                f.cmd_type,
                                f.payload
                            );
                        }
                        if let Some((seq, status)) = thermal::ack(f) {
                            match status {
                                0x00 => logln!("thermal: camera accepted seq {seq}"),
                                _ => logln!(
                                    "thermal: camera REJECTED seq {seq} (status {status:#04x})"
                                ),
                            }
                        }
                    }
                };
                for d in &mut decoders {
                    d.drain(&mut each);
                }
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
                    decoders = [duml::Decoder::new(), duml::Decoder::new()];
                    demux = rcmux::Demux::new();
                }
                sleep(Duration::from_millis(20));
            }
        }
    }
}
