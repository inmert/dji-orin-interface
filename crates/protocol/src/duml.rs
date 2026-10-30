//! DJI DUML ("0x55") protocol framing.
//!
//! Frame layout (little-endian multi-byte fields):
//!
//! ```text
//! off  size  field
//! 0    1     SOF, always 0x55
//! 1    1     length low 8 bits
//! 2    1     bits[1:0] = length high bits (10-bit length), bits[7:2] = version
//! 3    1     CRC-8 over bytes [0..2]            (header check)
//! 4    1     sender    (low 5 bits = device type, high 3 bits = index)
//! 5    1     receiver  (same encoding)
//! 6    2     sequence number
//! 8    1     command type / flags
//! 9    1     command set
//! 10   1     command id
//! 11   N     payload (length - 13 bytes)
//! ..   2     CRC-16 over bytes [0 .. length-3]  (whole-frame check)
//! ```

use std::fmt;

pub const SOF: u8 = 0x55;
pub const HEADER_LEN: usize = 4; // SOF + len(2) + hdr CRC-8
pub const OVERHEAD: usize = 13; // header(4) + routing/cmd(7) + CRC-16(2)
pub const MAX_LEN: usize = 0x3FF;

/// Device type (low 5 bits of the sender/receiver byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Device {
    Any = 0x00,
    Camera = 0x01,
    App = 0x02,
    FlightCtrl = 0x03,
    Gimbal = 0x04,
    CenterBoard = 0x05,
    Remote = 0x06,
    WiFi = 0x07,
    Dm36x = 0x08,
    HdLink = 0x09,
    Pc = 0x0A,
    Battery = 0x0B,
    Esc = 0x0C,
    Dm36xGnd = 0x0D,
    HdLinkGnd = 0x0E,
    UsbAir = 0x0F,
    UsbGnd = 0x10,
    Monocular = 0x11,
    /// Binocular vision (stereo obstacle sensing). Pushes the `0x0a` vision
    /// command set to the app on the RC link.
    Binocular = 0x12,
}

impl Device {
    /// Display name of a device type.
    pub fn name(self) -> &'static str {
        DEVICE_NAMES[self as usize]
    }
}

/// Names after dji-firmware-tools' dissector, shortened; `Device::name` indexes
/// this by the enum value, so it stays contiguous from 0x00.
const DEVICE_NAMES: [&str; 19] = [
    "Any",
    "Camera",
    "App",
    "FlightCtrl",
    "Gimbal",
    "CenterBoard",
    "Remote",
    "WiFi",
    "DM36x",
    "HDLink",
    "PC",
    "Battery",
    "ESC",
    "DM36xGnd",
    "HDLinkGnd",
    "USBAir",
    "USBGnd",
    "Monocular",
    "Binocular",
];

impl TryFrom<u8> for Device {
    type Error = u8;

    /// Interpret the device-type field (low 5 bits only); the index bits are
    /// ignored. Returns the unknown value as the error.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value & 0x1F {
            0x00 => Device::Any,
            0x01 => Device::Camera,
            0x02 => Device::App,
            0x03 => Device::FlightCtrl,
            0x04 => Device::Gimbal,
            0x05 => Device::CenterBoard,
            0x06 => Device::Remote,
            0x07 => Device::WiFi,
            0x08 => Device::Dm36x,
            0x09 => Device::HdLink,
            0x0A => Device::Pc,
            0x0B => Device::Battery,
            0x0C => Device::Esc,
            0x0D => Device::Dm36xGnd,
            0x0E => Device::HdLinkGnd,
            0x0F => Device::UsbAir,
            0x10 => Device::UsbGnd,
            0x11 => Device::Monocular,
            0x12 => Device::Binocular,
            other => return Err(other),
        })
    }
}

/// Display wrapper for a sender/receiver byte, e.g. `App[0]` or `type0x1f[0]`.
#[derive(Debug, Clone, Copy)]
pub struct DevName(pub u8);

impl fmt::Display for DevName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ty = (self.0 & 0x1F) as usize;
        let idx = self.0 >> 5;
        if ty < DEVICE_NAMES.len() {
            write!(f, "{}[{idx}]", DEVICE_NAMES[ty])
        } else {
            write!(f, "type{:#04x}[{idx}]", self.0 & 0x1F)
        }
    }
}

/// Encode a sender/receiver byte from a device type and index (the inverse of
/// the decode in [`DevName`]): low 5 bits type, high 3 bits index.
pub fn addr(device: Device, index: u8) -> u8 {
    (device as u8 & 0x1F) | (index << 5)
}

/// DJI header checksum: CRC-8, reflected poly 0x31, init 0x77.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0x77;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8C
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// DJI frame checksum: CRC-16/CCITT (reflected poly 0x8408), init 0x3692.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x3692;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// A parsed DUML frame. `payload` borrows from the buffer passed to [`parse`].
///
/// [`parse`] only returns a frame whose header CRC is valid, so `hdr_crc_ok` is
/// always `true` here; the field is kept for callers that persist frames.
#[derive(Debug, Clone)]
pub struct Frame<'a> {
    pub length: u16,
    pub version: u8,
    pub sender: u8,
    pub receiver: u8,
    pub seq: u16,
    pub cmd_type: u8,
    pub cmd_set: u8,
    pub cmd_id: u8,
    pub payload: &'a [u8],
    pub hdr_crc_ok: bool,
    pub crc_ok: bool,
}

impl fmt::Display for Frame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<14} -> {:<14} seq={:<5} set={:#04x} id={:#04x} len={}{}{}",
            DevName(self.sender).to_string(),
            DevName(self.receiver).to_string(),
            self.seq,
            self.cmd_set,
            self.cmd_id,
            self.payload.len(),
            if self.hdr_crc_ok {
                ""
            } else {
                " [BAD HDR CRC]"
            },
            if self.crc_ok { "" } else { " [BAD CRC]" },
        )
    }
}

/// Why [`parse`] could not return a complete frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// The buffer holds a valid-looking but truncated frame; read more bytes.
    NeedMore,
    /// `buf[0]` is not a plausible frame start: not a SOF, a failed header CRC,
    /// or a bogus length field.
    Invalid,
}

/// Parse one frame at the start of `buf`. On success returns the frame and the
/// number of bytes it consumed (its length).
///
/// The header CRC-8 is checked *before* the length field is trusted, so a stray
/// `0x55` in video/encrypted data is rejected as [`ParseError::Invalid`] (one
/// byte to resync) instead of skipping up to `MAX_LEN` bytes of real frames.
pub fn parse(buf: &[u8]) -> Result<(Frame<'_>, usize), ParseError> {
    if buf.is_empty() {
        return Err(ParseError::NeedMore);
    }
    if buf[0] != SOF {
        return Err(ParseError::Invalid);
    }
    if buf.len() < HEADER_LEN {
        return Err(ParseError::NeedMore);
    }
    // Validate the header CRC before believing the length field.
    if crc8(&buf[0..3]) != buf[3] {
        return Err(ParseError::Invalid);
    }
    let length = (buf[1] as usize) | (((buf[2] & 0x03) as usize) << 8);
    let version = buf[2] >> 2;
    if !(OVERHEAD..=MAX_LEN).contains(&length) {
        return Err(ParseError::Invalid);
    }
    if buf.len() < length {
        return Err(ParseError::NeedMore);
    }
    let plen = length - OVERHEAD;
    let want = u16::from_le_bytes([buf[length - 2], buf[length - 1]]);
    Ok((
        Frame {
            length: length as u16,
            version,
            sender: buf[4],
            receiver: buf[5],
            seq: u16::from_le_bytes([buf[6], buf[7]]),
            cmd_type: buf[8],
            cmd_set: buf[9],
            cmd_id: buf[10],
            payload: &buf[11..11 + plen],
            hdr_crc_ok: true,
            crc_ok: crc16(&buf[0..length - 2]) == want,
        },
        length,
    ))
}

/// How far to advance past a parsed frame: its whole length when the frame
/// CRC holds, else one byte. A stray `0x55` passes the header CRC-8 one time
/// in 256, and trusting its length would skip up to [`MAX_LEN`] bytes of real
/// frames; the bad frame is still reported, so loggers can show it.
fn step(f: &Frame, n: usize) -> usize {
    if f.crc_ok { n } else { 1 }
}

/// Scan a buffer for frames (resynchronising on `0x55`), calling `cb` per frame.
/// Returns the number of frames reported. For a live stream split across reads,
/// use [`Decoder`] instead, which reassembles frames across buffer boundaries.
pub fn scan(buf: &[u8], mut cb: impl FnMut(&Frame)) -> usize {
    let mut i = 0;
    let mut found = 0;
    while i < buf.len() {
        if buf[i] != SOF {
            i += 1;
            continue;
        }
        match parse(&buf[i..]) {
            Ok((f, n)) => {
                cb(&f);
                found += 1;
                i += step(&f, n);
            }
            Err(ParseError::NeedMore) => break, // truncated frame at buffer end
            Err(ParseError::Invalid) => i += 1, // false SOF, resync
        }
    }
    found
}

/// A streaming DUML reassembler: feed it whatever bytes each USB read yields,
/// and it emits complete frames even when one frame spans several reads.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append freshly-read bytes to the internal buffer.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Emit every complete frame currently buffered, calling `cb` for each, and
    /// discard the bytes they (and any resync junk) consumed. A trailing
    /// incomplete frame is retained for the next [`push`](Self::push); because
    /// [`parse`] validates the header CRC first, that retained tail is at most
    /// `MAX_LEN` bytes. Returns the number of frames emitted.
    pub fn drain(&mut self, mut cb: impl FnMut(&Frame)) -> usize {
        let mut i = 0;
        let mut found = 0;
        while i < self.buf.len() {
            if self.buf[i] != SOF {
                i += 1;
                continue;
            }
            match parse(&self.buf[i..]) {
                Ok((f, n)) => {
                    cb(&f);
                    found += 1;
                    i += step(&f, n);
                }
                Err(ParseError::NeedMore) => break,
                Err(ParseError::Invalid) => i += 1,
            }
        }
        self.buf.drain(0..i);
        found
    }

    /// Bytes currently buffered (a partial frame awaiting more data).
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

/// Routing and command header for [`build`].
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub sender: u8,
    pub receiver: u8,
    pub seq: u16,
    pub cmd_type: u8,
    pub cmd_set: u8,
    pub cmd_id: u8,
}

/// Why [`build`] could not produce a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// Payload too large for the 10-bit length field.
    PayloadTooLong,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::PayloadTooLong => f.write_str("payload too long for 10-bit DUML length"),
        }
    }
}

impl std::error::Error for BuildError {}

/// Build a complete frame (fills length/version header and both checksums,
/// protocol version 1).
pub fn build(h: &Header, payload: &[u8]) -> Result<Vec<u8>, BuildError> {
    let length = OVERHEAD + payload.len();
    if length > MAX_LEN {
        return Err(BuildError::PayloadTooLong);
    }
    let mut out = vec![0u8; length];
    out[0] = SOF;
    out[1] = (length & 0xFF) as u8;
    out[2] = (((length >> 8) & 0x03) | (1 << 2)) as u8; // version 1
    out[3] = crc8(&out[0..3]);
    out[4] = h.sender;
    out[5] = h.receiver;
    out[6] = (h.seq & 0xFF) as u8;
    out[7] = (h.seq >> 8) as u8;
    out[8] = h.cmd_type;
    out[9] = h.cmd_set;
    out[10] = h.cmd_id;
    out[11..11 + payload.len()].copy_from_slice(payload);
    let crc = crc16(&out[0..length - 2]);
    out[length - 2] = (crc & 0xFF) as u8;
    out[length - 1] = (crc >> 8) as u8;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Frames captured 2026-10-26 from DJI Pilot (app -> modules), full with CRCs.
    const F1: [u8; 13] = [
        0x55, 0x0d, 0x04, 0x33, 0x02, 0x2d, 0x07, 0x00, 0x40, 0x00, 0xff, 0x34, 0x7a,
    ];
    const F2: [u8; 13] = [
        0x55, 0x0d, 0x04, 0x33, 0x02, 0x0e, 0x08, 0x00, 0x40, 0x00, 0x01, 0x24, 0x87,
    ];
    const F3: [u8; 13] = [
        0x55, 0x0d, 0x04, 0x33, 0x02, 0x00, 0x0b, 0x00, 0x40, 0x00, 0x01, 0x4a, 0xa3,
    ];

    fn trailer(f: &[u8]) -> u16 {
        u16::from_le_bytes([f[f.len() - 2], f[f.len() - 1]])
    }

    #[test]
    fn crc8_matches_captured_headers() {
        assert_eq!(crc8(&F1[0..3]), F1[3]);
        assert_eq!(crc8(&F3[0..3]), F3[3]);
    }

    #[test]
    fn crc16_matches_captured_frames() {
        assert_eq!(crc16(&F1[0..F1.len() - 2]), trailer(&F1));
        assert_eq!(crc16(&F2[0..F2.len() - 2]), trailer(&F2));
        assert_eq!(crc16(&F3[0..F3.len() - 2]), trailer(&F3));
    }

    #[test]
    fn parses_a_real_frame() {
        let (f, n) = parse(&F2).expect("parse");
        assert_eq!(n, F2.len());
        assert!(f.hdr_crc_ok && f.crc_ok);
        assert_eq!(f.sender, Device::App as u8);
        assert_eq!((f.cmd_set, f.cmd_id), (0x00, 0x01)); // Get Version
        assert!(f.payload.is_empty());
    }

    #[test]
    fn parse_status_codes() {
        assert_eq!(
            parse(&[0x11, 0x22, 0x33]).map(|_| ()),
            Err(ParseError::Invalid)
        );
        assert_eq!(parse(&F2[0..6]).map(|_| ()), Err(ParseError::NeedMore));
    }

    #[test]
    fn rejects_bad_header_crc() {
        // A stray 0x55 with a plausible length but a wrong header CRC.
        let junk = [0x55, 0x40, 0x04, 0x00, 0xAA, 0xBB];
        assert_eq!(parse(&junk).map(|_| ()), Err(ParseError::Invalid));
    }

    #[test]
    fn scan_skips_false_sof_and_still_finds_the_real_frame() {
        // A bad-header-CRC 0x55 immediately before a valid frame: the old
        // length-trusting parser skipped the real frame; now it is found.
        let mut buf = vec![0x55, 0x40, 0x04, 0x00, 0xAA, 0xBB];
        buf.extend_from_slice(&F2);
        let mut valid = 0;
        scan(&buf, |f| {
            if f.hdr_crc_ok && f.crc_ok {
                valid += 1;
            }
        });
        assert_eq!(valid, 1);
    }

    #[test]
    fn bad_frame_crc_does_not_swallow_the_frame_behind_it() {
        // A stray SOF whose header CRC-8 happens to hold claims 40 bytes; the
        // real frame starts 20 bytes in and must still be found.
        let mut junk = vec![0x55, 40, 0x04, 0];
        junk[3] = crc8(&junk[0..3]);
        junk.resize(20, 0xAA);
        let stream = [&junk[..], &F2[..], &[0; 10]].concat();
        let mut valid = 0;
        let mut bad = 0;
        scan(&stream, |f| if f.crc_ok { valid += 1 } else { bad += 1 });
        assert_eq!((valid, bad), (1, 1));
        let mut d = Decoder::new();
        d.push(&stream);
        let mut valid = 0;
        d.drain(|f| valid += usize::from(f.crc_ok));
        assert_eq!(valid, 1);
        assert_eq!(d.buffered(), 0);
    }

    #[test]
    fn decoder_reassembles_a_split_frame() {
        let (a, b) = F2.split_at(7);
        let mut d = Decoder::new();
        let mut found = 0;
        d.push(a);
        found += d.drain(|_| {});
        assert_eq!(found, 0);
        assert_eq!(d.buffered(), a.len());
        d.push(b);
        found += d.drain(|f| assert!(f.crc_ok));
        assert_eq!(found, 1);
        assert_eq!(d.buffered(), 0);
    }

    /// Junk, a stray SOF whose header CRC holds (claiming 20 bytes), a frame
    /// with a broken CRC-16, then three good frames, the last with a payload.
    fn messy_stream() -> (Vec<u8>, usize) {
        let mut stray = vec![SOF, 20, 0x04, 0];
        stray[3] = crc8(&stray[0..3]);
        stray.resize(20, 0xAA);
        let mut broken = F1.to_vec();
        broken[11] ^= 1;
        let h = Header {
            sender: 0x03,
            receiver: 0x02,
            seq: 9,
            cmd_type: 0x80,
            cmd_set: 0x03,
            cmd_id: 0x2a,
        };
        let with_payload = build(&h, &[0x00, SOF, 0x0d]).unwrap();
        let stream = [
            &[0x13, 0x37, SOF][..],
            &stray,
            &broken,
            &F2,
            &F3,
            &with_payload,
        ]
        .concat();
        (stream, 3)
    }

    fn good_frames(d: &mut Decoder) -> usize {
        let mut good = 0;
        d.drain(|f| good += usize::from(f.crc_ok));
        good
    }

    #[test]
    fn decoder_finds_every_frame_fed_a_byte_at_a_time_or_cut_anywhere() {
        let (stream, want) = messy_stream();
        let mut d = Decoder::new();
        let mut good = 0;
        for b in &stream {
            d.push(std::slice::from_ref(b));
            good += good_frames(&mut d);
        }
        assert_eq!((good, d.buffered()), (want, 0), "byte at a time");
        for cut in 0..=stream.len() {
            let mut d = Decoder::new();
            d.push(&stream[..cut]);
            let mut good = good_frames(&mut d);
            d.push(&stream[cut..]);
            good += good_frames(&mut d);
            assert_eq!((good, d.buffered()), (want, 0), "cut at {cut}");
        }
    }

    #[test]
    fn every_truncation_asks_for_more() {
        for n in 0..F2.len() {
            assert_eq!(
                parse(&F2[..n]).map(|_| ()),
                Err(ParseError::NeedMore),
                "{n}"
            );
        }
        // A length field below the fixed overhead is not a frame.
        let mut short = vec![SOF, (OVERHEAD - 1) as u8, 0x04, 0];
        short[3] = crc8(&short[0..3]);
        assert_eq!(parse(&short).map(|_| ()), Err(ParseError::Invalid));
    }

    #[test]
    fn the_largest_frame_builds_and_parses() {
        let h = Header {
            sender: 0x02,
            receiver: 0x03,
            seq: 0xFFFF,
            cmd_type: 0x40,
            cmd_set: 0x03,
            cmd_id: 0x2a,
        };
        let payload = vec![SOF; MAX_LEN - OVERHEAD];
        let frame = build(&h, &payload).unwrap();
        assert_eq!(frame.len(), MAX_LEN);
        let (f, n) = parse(&frame).unwrap();
        assert_eq!(
            (n, f.crc_ok, f.seq, f.payload.len()),
            (MAX_LEN, true, 0xFFFF, payload.len())
        );
        assert_eq!(
            build(&h, &[0; MAX_LEN - OVERHEAD + 1]),
            Err(BuildError::PayloadTooLong)
        );
    }

    #[test]
    fn build_round_trips_captured_frame() {
        let h = Header {
            sender: 0x02,
            receiver: 0x0e,
            seq: 0x0008,
            cmd_type: 0x40,
            cmd_set: 0x00,
            cmd_id: 0x01,
        };
        assert_eq!(build(&h, &[]).unwrap(), F2.to_vec());
    }

    #[test]
    fn build_rejects_oversized_payload() {
        let h = Header {
            sender: 0,
            receiver: 0,
            seq: 0,
            cmd_type: 0,
            cmd_set: 0,
            cmd_id: 0,
        };
        assert_eq!(build(&h, &[0u8; MAX_LEN]), Err(BuildError::PayloadTooLong));
    }

    #[test]
    fn device_try_from_round_trips() {
        assert_eq!(Device::try_from(0x43), Ok(Device::FlightCtrl)); // index bits ignored
        assert_eq!(Device::try_from(0x1F), Err(0x1F));
        // 0x0A is the PC on the aircraft's USB API port (the FC acks it there).
        assert_eq!(Device::try_from(0x0A), Ok(Device::Pc));
        assert_eq!(Device::try_from(0x0C), Ok(Device::Esc));
        assert_eq!(DevName(0x0A).to_string(), "PC[0]");
        // 0x12 pushes the vision command set (0x0a) to App[0] on the RC link.
        assert_eq!(Device::try_from(0x12), Ok(Device::Binocular));
        assert_eq!(DevName(0x12).to_string(), "Binocular[0]");
        assert_eq!(DevName(0x13).to_string(), "type0x13[0]");
        for v in 0..DEVICE_NAMES.len() as u8 {
            assert_eq!(Device::try_from(v).map(|d| d as u8), Ok(v));
        }
    }
}
