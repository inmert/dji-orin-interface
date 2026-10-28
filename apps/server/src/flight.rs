//! Flight-data reader: decodes the OSDK broadcast the aircraft pushes on its
//! SDK UART into the shared [`Telemetry`](dji_protocol::Telemetry).
//!
//! Receive-only by construction: the port is opened `O_RDONLY`
//! ([`RxPort`]), so this can never send anything to the aircraft. Runs
//! alongside (and independent of) the RC USB link; a missing or unplugged
//! port is retried every few seconds.

use std::io::ErrorKind;
use std::thread::sleep;
use std::time::Duration;

use dji_common::logln;
use dji_protocol::osdk;
use dji_transport::serial::RxPort;

use crate::reader::Tele;

const RETRY: Duration = Duration::from_secs(5);

pub fn run(port: String, baud: u32, tele: Tele) {
    tele.lock().unwrap().flight.port.clone_from(&port);
    let mut last_err = String::new();
    loop {
        match RxPort::open(&port, baud) {
            Ok(rx) => {
                logln!("flight: reading {port} at {baud} baud (receive-only)");
                last_err.clear();
                tele.lock().unwrap().flight.open = true;
                read(rx, &tele);
                tele.lock().unwrap().flight.open = false;
            }
            Err(e) => {
                // Log each distinct failure once, not every retry.
                let msg = e.to_string();
                if msg != last_err {
                    logln!("flight: cannot open {port}: {msg}; retrying");
                    last_err = msg;
                }
            }
        }
        sleep(RETRY);
    }
}

/// Read and decode until the port fails (e.g. a USB adapter is unplugged).
fn read(mut rx: RxPort, tele: &Tele) {
    let mut decoder = osdk::Decoder::new();
    let mut buf = vec![0u8; 4096];
    loop {
        match rx.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                decoder.push(&buf[..n]);
                let mut t = tele.lock().unwrap();
                decoder.drain(|f| t.flight.update(f));
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                logln!("flight: read error: {e}; reopening");
                return;
            }
        }
    }
}
