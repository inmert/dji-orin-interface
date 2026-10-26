//! Decode a stream of raw bytes into DUML frames, the way the capture tools do.
//!
//! Run with: `cargo run --example decode_frames`

use dji_orin_interface::duml::Decoder;

fn main() {
    // A couple of real captured frames, with a byte of line noise spliced in
    // between to show that the decoder resynchronises on the next SOF.
    let frame1 = [
        0x55, 0x0d, 0x04, 0x33, 0x02, 0x2d, 0x07, 0x00, 0x40, 0x00, 0xff, 0x34, 0x7a,
    ];
    let frame2 = [
        0x55, 0x0d, 0x04, 0x33, 0x02, 0x0e, 0x08, 0x00, 0x40, 0x00, 0x01, 0x24, 0x87,
    ];

    let mut stream = Vec::new();
    stream.extend_from_slice(&frame1);
    stream.push(0x55); // a false SOF (bad header CRC) — ignored
    stream.extend_from_slice(&frame2);

    // Feed the stream in small chunks, as USB reads would arrive.
    let mut decoder = Decoder::new();
    let mut total = 0;
    for chunk in stream.chunks(5) {
        decoder.push(chunk);
        total += decoder.drain(|frame| {
            let tag = if frame.crc_ok { "ok " } else { "BAD" };
            println!("[{tag}] {frame}");
        });
    }
    println!(
        "decoded {total} CRC-checked frame(s); {} byte(s) buffered",
        decoder.buffered()
    );
}
