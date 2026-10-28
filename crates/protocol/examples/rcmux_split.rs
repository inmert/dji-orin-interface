//! Split a raw RC→app USB capture (e.g. from the server's `RC_DUMP`) into one
//! file per mux channel, offline: `<out_prefix>-<channel>.bin`. The video
//! channels are plain H.264 byte streams, playable with e.g.
//! `gst-play-1.0` or `ffplay -f h264`.
//!
//! Usage: `cargo run -p dji-protocol --example rcmux_split -- <ep1.bin> <out_prefix>`

use std::collections::BTreeMap;

use dji_protocol::rcmux::Demux;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(prefix)) = (args.next(), args.next()) else {
        eprintln!("usage: rcmux_split <ep1.bin> <out_prefix>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&input).expect("read capture");
    let mut channels: BTreeMap<u8, (usize, Vec<u8>)> = BTreeMap::new();
    let mut demux = Demux::new();
    // Feed in USB-sized reads, as the server does.
    for chunk in bytes.chunks(16384) {
        demux.push(chunk, |ch, payload| {
            let entry = channels.entry(ch).or_default();
            entry.0 += 1;
            entry.1.extend_from_slice(payload);
        });
    }
    let total: usize = channels.values().map(|(_, b)| b.len()).sum();
    println!("{input}: {} bytes, {total} payload bytes", bytes.len());
    for (ch, (packets, data)) in &channels {
        let out = format!("{prefix}-{ch:02x}.bin");
        std::fs::write(&out, data).expect("write channel");
        println!(
            "channel 0x{ch:02x}: {packets} packets, {} bytes -> {out}",
            data.len()
        );
    }
}
