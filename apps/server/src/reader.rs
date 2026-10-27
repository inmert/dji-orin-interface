//! Telemetry reader: reassembles the DUML on ep1 (OUT, RC → Orin) and folds it
//! into the shared [`Telemetry`].

use std::io::ErrorKind;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use dji_common::logln;
use dji_protocol::Telemetry;
use dji_protocol::duml;
use dji_transport::usb;

pub type Tele = Arc<Mutex<Telemetry>>;

/// Read until the endpoint is gone (`ENODEV`). `ESHUTDOWN` (the RC disabled the
/// gadget, e.g. it was power-cycled) is waited out, so the reader resumes on
/// the next ENABLE without being respawned.
pub fn run(ep1: OwnedFd, tele: Tele) {
    let mut decoder = duml::Decoder::new();
    let mut buf = vec![0u8; 16384];
    loop {
        match usb::read(ep1.as_fd(), &mut buf) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
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
                }
                sleep(Duration::from_millis(20));
            }
        }
    }
}
