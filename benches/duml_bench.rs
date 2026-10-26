//! Micro-benchmark for the DUML hot path (CRC-16 and `scan`), with no external
//! bench framework — `harness = false` in Cargo.toml makes this a plain binary.
//!
//! Run with: `cargo bench`

use std::hint::black_box;
use std::time::Instant;

use dji_orin_interface::duml::{self, Header};

/// Time `f` over `iters` runs and print MiB/s against `bytes_per_iter`.
fn bench(name: &str, iters: u32, bytes_per_iter: usize, mut f: impl FnMut()) {
    // Warm up, then measure.
    for _ in 0..iters / 10 + 1 {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    let elapsed = start.elapsed();
    let total_bytes = bytes_per_iter as f64 * f64::from(iters);
    let mib_s = total_bytes / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    let ns_per_iter = elapsed.as_nanos() as f64 / f64::from(iters);
    println!("{name:<20} {ns_per_iter:>10.1} ns/iter  {mib_s:>8.1} MiB/s");
}

fn main() {
    // A ~1 KiB buffer of back-to-back real frames, as the bulk reader sees.
    let frame = duml::build(
        &Header {
            sender: 0x03,
            receiver: 0x02,
            seq: 1,
            cmd_type: 0x40,
            cmd_set: 0x03,
            cmd_id: 0x43,
        },
        &[0u8; 32],
    )
    .expect("build");
    let mut buf = Vec::new();
    while buf.len() < 1024 {
        buf.extend_from_slice(&frame);
    }

    bench("crc16/1KiB", 200_000, buf.len(), || {
        black_box(duml::crc16(black_box(&buf)));
    });

    bench("scan/1KiB", 200_000, buf.len(), || {
        let mut n = 0usize;
        duml::scan(black_box(&buf), |f| n += usize::from(f.crc_ok));
        black_box(n);
    });
}
