//! The aircraft's own serial ports: decode what it pushes there into the
//! shared [`Telemetry`], independent of (and alongside) the RC link.
//!
//! * [`Osdk`] — the OSDK broadcast on the SDK UART (a CP210x adapter or
//!   `ttyTHS1`; the server picks the port).
//! * [`Usb`] — the DUML reports on the USB API port (a `ttyACM`).
//!
//! Receive-only by construction: ports are opened `O_RDONLY` ([`RxPort`]), so
//! nothing here can send anything to the aircraft. ACKs read here (OpenProtocol
//! on the SDK UART, the FC's USB-flight ACK on the USB port) go to
//! [`crate::motors`], which sends its requests on handles of its own. A missing
//! or unplugged port is retried every few seconds, choosing the port again each
//! time.

use std::io::ErrorKind;
use std::thread::sleep;
use std::time::Duration;

use dji_common::logln;
use dji_protocol::{Telemetry, duml, osdk};
use dji_transport::serial::RxPort;

use crate::motors::Motors;
use crate::{Tele, Vehicle};

const RETRY: Duration = Duration::from_secs(5);

/// Shown as the port while an automatic choice finds none.
pub const NO_PORT: &str = "no port found";

/// A protocol decoder for one port, rebuilt each time the port is opened.
pub trait Link: Default {
    const NAME: &'static str;
    /// The link's `(port, open)` fields in the snapshot.
    fn state(t: &mut Telemetry) -> (&mut String, &mut bool);
    /// Decode freshly read bytes into the snapshot; ACKs go to `motors`.
    fn feed(&mut self, bytes: &[u8], t: &mut Telemetry, motors: &Motors);
}

#[derive(Default)]
pub struct Osdk(osdk::Decoder);

impl Link for Osdk {
    const NAME: &'static str = "flight";
    fn state(t: &mut Telemetry) -> (&mut String, &mut bool) {
        (&mut t.flight.port, &mut t.flight.open)
    }
    fn feed(&mut self, bytes: &[u8], t: &mut Telemetry, motors: &Motors) {
        self.0.push(bytes);
        self.0.drain(|f| {
            motors.deliver_ack(f);
            t.flight.update(f);
        });
    }
}

#[derive(Default)]
pub struct Usb(duml::Decoder);

impl Link for Usb {
    const NAME: &'static str = "usb";
    fn state(t: &mut Telemetry) -> (&mut String, &mut bool) {
        (&mut t.usb.port, &mut t.usb.open)
    }
    fn feed(&mut self, bytes: &[u8], t: &mut Telemetry, motors: &Motors) {
        self.0.push(bytes);
        self.0.drain(|f| {
            motors.deliver_duml_ack(f);
            if f.crc_ok {
                t.usb.update(f);
            }
        });
    }
}

fn set_open<L: Link>(tele: &Tele, open: bool) {
    *L::state(&mut tele.lock().unwrap()).1 = open;
}

/// Read the port `pick` names, choosing again on every (re)open; `None`
/// means there is none yet.
pub fn run<L: Link>(pick: impl Fn() -> Option<String>, baud: u32, vehicle: &Vehicle) {
    let tele = &vehicle.telemetry;
    let mut last_err = String::new();
    loop {
        let Some(port) = pick() else {
            L::state(&mut tele.lock().unwrap())
                .0
                .replace_range(.., NO_PORT);
            if last_err != NO_PORT {
                logln!("{}: {NO_PORT}; retrying", L::NAME);
                last_err = NO_PORT.into();
            }
            sleep(RETRY);
            continue;
        };
        L::state(&mut tele.lock().unwrap()).0.clone_from(&port);
        match RxPort::open(&port, baud) {
            Ok(rx) => {
                logln!("{}: reading {port} (receive-only)", L::NAME);
                last_err.clear();
                set_open::<L>(tele, true);
                read::<L>(rx, vehicle);
                set_open::<L>(tele, false);
            }
            Err(e) => {
                // Log each distinct failure once, not every retry.
                let msg = e.to_string();
                if msg != last_err {
                    logln!("{}: cannot open {port}: {msg}; retrying", L::NAME);
                    last_err = msg;
                }
            }
        }
        sleep(RETRY);
    }
}

/// Read and decode until the port fails (e.g. the cable is unplugged).
fn read<L: Link>(mut rx: RxPort, vehicle: &Vehicle) {
    let mut link = L::default();
    let mut buf = vec![0u8; 4096];
    loop {
        match rx.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => link.feed(
                &buf[..n],
                &mut vehicle.telemetry.lock().unwrap(),
                &vehicle.motors,
            ),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                logln!("{}: read error: {e}; reopening", L::NAME);
                return;
            }
        }
    }
}
