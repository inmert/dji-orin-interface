//! Decode the OSDK flight-data broadcast from a raw UART capture file, offline.
//!
//! Usage: `cargo run -p dji-protocol --example osdk_broadcast -- <capture.bin>`
//!
//! Prints frame counts and the final decoded state. Position fields are left
//! out on purpose; captures can hold the aircraft's real location.

use dji_protocol::osdk::{self, broadcast};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: osdk_broadcast <capture.bin>");
    let bytes = std::fs::read(&path).expect("read capture");
    let mut decoder = osdk::Decoder::new();
    decoder.push(&bytes);
    let mut data = broadcast::FlightData::default();
    let (mut ok, mut other, mut bad) = (0, 0, 0);
    decoder.drain(|f| {
        if (f.cmd_set, f.cmd_id, f.enc) != (broadcast::CMD_SET, broadcast::CMD_ID, 0) {
            other += 1;
        } else if data.apply(f.payload).is_ok() {
            ok += 1;
        } else {
            bad += 1;
        }
    });
    println!("{path}: {ok} broadcasts decoded, {bad} rejected, {other} other frames");
    data.position = None;
    if let Some(g) = data.gps.as_mut() {
        (g.latitude, g.longitude) = (0.0, 0.0);
    }
    println!("{data:#?}");
}
