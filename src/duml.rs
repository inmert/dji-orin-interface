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
    Battery = 0x0A,
    Esc = 0x0B,
}

/// DJI header checksum: CRC-8, reflected poly 0x31, init 0x77.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0x77;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8C } else { crc >> 1 };
        }
    }
    crc
}

/// DJI frame checksum: CRC-16/CCITT (reflected poly 0x8408), init 0x3692.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x3692;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8408 } else { crc >> 1 };
        }
    }
    crc
}

/// Human-readable name for a sender/receiver byte, e.g. `"App[0]"`.
pub fn dev_name(id: u8) -> String {
    const NAMES: [&str; 12] = [
        "Any", "Camera", "App", "FlightCtrl", "Gimbal", "CenterBoard", "Remote", "WiFi", "DM36x",
        "HDLink", "Battery", "ESC",
    ];
    let ty = (id & 0x1F) as usize;
    let idx = id >> 5;
    if ty < NAMES.len() {
        format!("{}[{}]", NAMES[ty], idx)
    } else {
        format!("type{:#04x}[{}]", id & 0x1F, idx)
    }
}

/// A parsed DUML frame. `payload` borrows from the buffer passed to [`parse`].
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

impl Frame<'_> {
    /// One-line human description.
    pub fn describe(&self) -> String {
        format!(
            "{:<14} -> {:<14} seq={:<5} set={:#04x} id={:#04x} len={}{}{}",
            dev_name(self.sender),
            dev_name(self.receiver),
            self.seq,
            self.cmd_set,
            self.cmd_id,
            self.payload.len(),
            if self.hdr_crc_ok { "" } else { " [BAD HDR CRC]" },
            if self.crc_ok { "" } else { " [BAD CRC]" },
        )
    }
}

/// Why [`parse`] could not return a complete frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// The buffer holds a valid-looking but truncated frame; read more bytes.
    NeedMore,
    /// `buf[0]` is not a plausible frame start, or the length field is bogus.
    Invalid,
}

/// Parse one frame at the start of `buf`. On success returns the frame and the
/// number of bytes it consumed (its length).
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
            hdr_crc_ok: crc8(&buf[0..3]) == buf[3],
            crc_ok: crc16(&buf[0..length - 2]) == want,
        },
        length,
    ))
}

/// Scan a buffer for frames (resynchronising on `0x55`), calling `cb` per frame.
/// Returns the number of frames reported.
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
                i += n;
            }
            Err(ParseError::NeedMore) => break, // truncated frame at buffer end
            Err(ParseError::Invalid) => i += 1, // false SOF, resync
        }
    }
    found
}

/// Build a complete frame (fills length/version header and both checksums).
/// Returns an empty vector if the payload is too large for the 10-bit length.
pub fn build(
    sender: u8,
    receiver: u8,
    seq: u16,
    cmd_type: u8,
    cmd_set: u8,
    cmd_id: u8,
    payload: &[u8],
) -> Vec<u8> {
    let length = OVERHEAD + payload.len();
    if length > MAX_LEN {
        return Vec::new();
    }
    let mut out = vec![0u8; length];
    out[0] = SOF;
    out[1] = (length & 0xFF) as u8;
    out[2] = (((length >> 8) & 0x03) | (1 << 2)) as u8; // version 1
    out[3] = crc8(&out[0..3]);
    out[4] = sender;
    out[5] = receiver;
    out[6] = (seq & 0xFF) as u8;
    out[7] = (seq >> 8) as u8;
    out[8] = cmd_type;
    out[9] = cmd_set;
    out[10] = cmd_id;
    out[11..11 + payload.len()].copy_from_slice(payload);
    let crc = crc16(&out[0..length - 2]);
    out[length - 2] = (crc & 0xFF) as u8;
    out[length - 1] = (crc >> 8) as u8;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Frames captured 2026-10-26 from DJI Pilot (app -> modules), full with CRCs.
    const F1: [u8; 13] = [0x55, 0x0d, 0x04, 0x33, 0x02, 0x2d, 0x07, 0x00, 0x40, 0x00, 0xff, 0x34, 0x7a];
    const F2: [u8; 13] = [0x55, 0x0d, 0x04, 0x33, 0x02, 0x0e, 0x08, 0x00, 0x40, 0x00, 0x01, 0x24, 0x87];
    const F3: [u8; 13] = [0x55, 0x0d, 0x04, 0x33, 0x02, 0x00, 0x0b, 0x00, 0x40, 0x00, 0x01, 0x4a, 0xa3];

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
        assert_eq!(parse(&[0x11, 0x22, 0x33]).map(|_| ()), Err(ParseError::Invalid));
        assert_eq!(parse(&F2[0..6]).map(|_| ()), Err(ParseError::NeedMore));
    }

    #[test]
    fn build_round_trips_captured_frame() {
        assert_eq!(build(0x02, 0x0e, 0x0008, 0x40, 0x00, 0x01, &[]), F2.to_vec());
    }
}
