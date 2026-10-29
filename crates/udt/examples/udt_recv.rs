//! Receive a UDT stream for a few seconds and save it: a quick check that the
//! aircraft serves a camera feed, independent of the server.
//!
//! Usage: `cargo run -p dji-udt --example udt_recv -- [ip:port] [seconds] [out]`
//! (defaults: the FPV camera, `192.168.42.2:40003`, 5 s, `udt_recv.h264`).
//!
//! The client sends only UDT's own handshake and ACKs; no aircraft commands.

use std::io::Write;
use std::net::SocketAddrV4;
use std::time::{Duration, Instant};

use dji_udt::Stream;

fn main() {
    let mut args = std::env::args().skip(1);
    let addr: SocketAddrV4 = args
        .next()
        .unwrap_or_else(|| "192.168.42.2:40003".into())
        .parse()
        .expect("ip:port");
    let secs: u64 = args.next().map_or(5, |s| s.parse().expect("seconds"));
    let out = args.next().unwrap_or_else(|| "udt_recv.h264".into());

    let mut stream = Stream::connect(addr, Duration::from_millis(200))
        .unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    eprintln!("connected to {addr}");
    let mut file = std::fs::File::create(&out).expect("create output file");
    let mut buf = vec![0u8; 128 * 1024];
    let (mut total, mut reads, mut start_codes) = (0usize, 0u64, 0u64);
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        match stream.recv(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                file.write_all(&buf[..n]).expect("write");
                total += n;
                reads += 1;
                start_codes += buf[..n].windows(4).filter(|w| *w == [0, 0, 0, 1]).count() as u64;
            }
            Err(e) => {
                eprintln!("recv: {e}");
                break;
            }
        }
    }
    eprintln!("{total} bytes in {reads} reads, {start_codes} H.264 start codes, saved to {out}");
    if total == 0 {
        std::process::exit(3);
    }
}
