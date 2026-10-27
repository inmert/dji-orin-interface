//! Black-box integration tests for the public DUML API: they use the crate
//! exactly as the binaries do, exercising the framing surface end to end.

use dji_protocol::duml::{self, Decoder, Device, Header};

/// A real App->module "get version" frame captured from DJI Pilot.
const FRAME: [u8; 13] = [
    0x55, 0x0d, 0x04, 0x33, 0x02, 0x0e, 0x08, 0x00, 0x40, 0x00, 0x01, 0x24, 0x87,
];

#[test]
fn build_then_parse_round_trips() {
    let h = Header {
        sender: Device::App as u8,
        receiver: 0x0e,
        seq: 0x0008,
        cmd_type: 0x40,
        cmd_set: 0x00,
        cmd_id: 0x01,
    };
    let bytes = duml::build(&h, &[]).expect("build");
    assert_eq!(bytes, FRAME);

    let (frame, consumed) = duml::parse(&bytes).expect("parse");
    assert_eq!(consumed, bytes.len());
    assert!(frame.hdr_crc_ok && frame.crc_ok);
    assert_eq!(frame.sender, Device::App as u8);
    assert_eq!((frame.cmd_set, frame.cmd_id), (0x00, 0x01));
}

#[test]
fn scan_rejects_video_noise_but_keeps_real_frames() {
    // A stray 0x55 with a bad header CRC (as encrypted/video bytes produce),
    // immediately followed by two valid frames.
    let mut buf = vec![0x55, 0x40, 0x04, 0x00, 0xAA, 0xBB];
    buf.extend_from_slice(&FRAME);
    buf.extend_from_slice(&FRAME);

    let mut valid = 0;
    duml::scan(&buf, |f| {
        if f.hdr_crc_ok && f.crc_ok {
            valid += 1;
        }
    });
    assert_eq!(valid, 2);
}

#[test]
fn decoder_reassembles_frames_split_across_reads() {
    // Feed the two frames one byte at a time; the decoder must still emit both.
    let mut stream = Vec::new();
    stream.extend_from_slice(&FRAME);
    stream.extend_from_slice(&FRAME);

    let mut decoder = Decoder::new();
    let mut found = 0;
    for byte in stream {
        decoder.push(&[byte]);
        found += decoder.drain(|f| assert!(f.crc_ok));
    }
    assert_eq!(found, 2);
    assert_eq!(decoder.buffered(), 0);
}

#[test]
fn device_try_from_ignores_index_bits() {
    assert_eq!(Device::try_from(0x63), Ok(Device::FlightCtrl)); // index 3, type 3
    assert_eq!(Device::try_from(0x1F), Err(0x1F));
}
