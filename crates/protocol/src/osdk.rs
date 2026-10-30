//! DJI Onboard SDK "OpenProtocol" (0xAA) framing, as spoken on the aircraft's
//! OSDK UART. A different wire format from DUML: own SOF, CRCs and command
//! numbering. Mostly we receive on it; [`command`] builds the few commands the
//! server sends (activation, control authority, motor start/stop).
//!
//! Frame layout (little-endian multi-byte fields):
//!
//! ```text
//! off  size  field
//! 0    1     SOF, always 0xAA
//! 1    2     bits[9:0] = total length, bits[15:10] = version
//! 3    1     bits[4:0] = session id, bit 5 = ack
//! 4    1     bits[2:0] = encryption (0 = none)
//! 5    3     reserved
//! 8    2     sequence number
//! 10   2     CRC-16 over bytes [0..10]          (header check)
//! 12   1     command set
//! 13   1     command id
//! 14   N     payload (length - 18 bytes)
//! ..   4     CRC-32 over bytes [0 .. length-4]  (whole-frame check)
//! ```
//!
//! An ACK (bit 5 of byte 3 set) has no command set/id: its data starts at
//! byte 12, and it answers the request with the same session and sequence.
//!
//! Both CRCs are reflected and seeded `0x3AA3`: CRC-16 poly `0xA001`, CRC-32
//! poly `0xEDB88320` (values from the Onboard SDK source).
//!
//! * [`broadcast`] — the flight-data push (`0x02/0x00`) the FC sends unasked.
//! * [`command`] — requests we send, and their ACK codes.

pub mod broadcast;
pub mod command;

pub const SOF: u8 = 0xAA;
pub const HEADER_LEN: usize = 12;
/// Header + set/id + CRC-32.
pub const OVERHEAD: usize = HEADER_LEN + 2 + 4;
pub const MAX_LEN: usize = 0x3FF;

const CRC_INIT: u32 = 0x3AA3;

pub fn crc16(data: &[u8]) -> u16 {
    let mut c = CRC_INIT as u16;
    for &b in data {
        c ^= u16::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xA001
            } else {
                c >> 1
            };
        }
    }
    c
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = CRC_INIT;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    c
}

/// A CRC-valid OpenProtocol frame, borrowing its payload. For an ACK,
/// `cmd_set`/`cmd_id` are 0 and `payload` is the ACK data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub session: u8,
    pub ack: bool,
    /// Encryption type; payloads with a non-zero value are ciphertext.
    pub enc: u8,
    pub seq: u16,
    pub cmd_set: u8,
    pub cmd_id: u8,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Not enough bytes yet for the header or the whole frame.
    Incomplete,
    BadSof,
    BadHeaderCrc,
    /// Declared length is shorter than the fixed overhead.
    BadLength,
    BadCrc,
}

/// Parse one frame at the start of `buf`; returns it and its total length.
/// The header CRC is checked before the length is trusted, so a stray `0xAA`
/// is rejected without waiting for up to a kilobyte of bytes.
pub fn parse(buf: &[u8]) -> Result<(Frame<'_>, usize), ParseError> {
    if buf.len() < HEADER_LEN {
        return Err(ParseError::Incomplete);
    }
    if buf[0] != SOF {
        return Err(ParseError::BadSof);
    }
    if crc16(&buf[..10]) != u16::from_le_bytes([buf[10], buf[11]]) {
        return Err(ParseError::BadHeaderCrc);
    }
    let len = usize::from(u16::from_le_bytes([buf[1], buf[2]]) & 0x3FF);
    let ack = buf[3] & 0x20 != 0;
    if len < if ack { HEADER_LEN + 4 } else { OVERHEAD } {
        return Err(ParseError::BadLength);
    }
    let Some(frame) = buf.get(..len) else {
        return Err(ParseError::Incomplete);
    };
    let (body, crc) = frame.split_at(len - 4);
    if crc32(body) != u32::from_le_bytes([crc[0], crc[1], crc[2], crc[3]]) {
        return Err(ParseError::BadCrc);
    }
    let (cmd_set, cmd_id, payload) = if ack {
        (0, 0, &body[HEADER_LEN..])
    } else {
        (buf[12], buf[13], &body[14..])
    };
    Ok((
        Frame {
            session: buf[3] & 0x1F,
            ack,
            enc: buf[4] & 0x07,
            seq: u16::from_le_bytes([buf[8], buf[9]]),
            cmd_set,
            cmd_id,
            payload,
        },
        len,
    ))
}

/// Streaming reassembler for a serial byte stream: push reads, drain frames.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Emit every complete, CRC-valid frame buffered so far and drop the bytes
    /// they (and any line noise) used; a trailing partial frame is kept.
    /// Returns the number of frames emitted.
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
                    i += n;
                }
                Err(ParseError::Incomplete) => break,
                Err(_) => i += 1,
            }
        }
        self.buf.drain(..i);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a frame the way the FC does.
    pub(crate) fn frame(cmd_set: u8, cmd_id: u8, payload: &[u8]) -> Vec<u8> {
        let len = OVERHEAD + payload.len();
        let mut f = vec![SOF, len as u8, (len >> 8) as u8 | 0x04, 0, 0, 0, 0, 0, 7, 0];
        f.extend_from_slice(&crc16(&f).to_le_bytes());
        f.extend_from_slice(&[cmd_set, cmd_id]);
        f.extend_from_slice(payload);
        f.extend_from_slice(&crc32(&f).to_le_bytes());
        f
    }

    #[test]
    fn parses_a_built_frame() {
        let bytes = frame(0x02, 0x00, &[1, 0, 9, 9]);
        let (f, n) = parse(&bytes).unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!((f.cmd_set, f.cmd_id, f.seq, f.enc), (0x02, 0x00, 7, 0));
        assert_eq!(f.payload, &[1, 0, 9, 9]);
    }

    #[test]
    fn parses_an_ack_without_set_or_id() {
        // An ACK of one code byte: header, data, CRC-32.
        let len = HEADER_LEN + 1 + 4;
        let mut f = vec![SOF, len as u8, 0, 0x22, 0, 0, 0, 0, 0x34, 0x12];
        f.extend_from_slice(&crc16(&f).to_le_bytes());
        f.push(0x02);
        f.extend_from_slice(&crc32(&f).to_le_bytes());
        let (a, n) = parse(&f).unwrap();
        assert_eq!(n, len);
        assert!(a.ack);
        assert_eq!((a.session, a.seq, a.cmd_set, a.cmd_id), (2, 0x1234, 0, 0));
        assert_eq!(a.payload, &[0x02]);
    }

    #[test]
    fn rejects_corruption() {
        let mut bytes = frame(0x02, 0x00, &[1, 0]);
        let last = bytes.len() - 5;
        bytes[last] ^= 1;
        assert_eq!(parse(&bytes), Err(ParseError::BadCrc));
        bytes[1] ^= 1;
        assert_eq!(parse(&bytes), Err(ParseError::BadHeaderCrc));
    }

    /// An ACK of `data` with session 2 and sequence `seq`.
    fn ack(seq: u16, data: &[u8]) -> Vec<u8> {
        let len = HEADER_LEN + data.len() + 4;
        let [s0, s1] = seq.to_le_bytes();
        let mut f = vec![SOF, len as u8, 0, 0x22, 0, 0, 0, 0, s0, s1];
        f.extend_from_slice(&crc16(&f).to_le_bytes());
        f.extend_from_slice(data);
        f.extend_from_slice(&crc32(&f).to_le_bytes());
        f
    }

    /// Line noise, a frame with a broken CRC-32, then a broadcast, an ACK and
    /// a second broadcast: the three good frames, as (ack, seq).
    fn noisy_stream() -> (Vec<u8>, Vec<(bool, u16)>) {
        let mut broken = frame(0x02, 0x00, &[1, 0, 1, 2]);
        let last = broken.len() - 1;
        broken[last] ^= 0x80;
        let stream = [
            &[0x00, SOF, 0x13, SOF][..],
            &broken,
            &frame(0x02, 0x00, &[1, 0, 9]),
            &ack(0x1234, &[2, 0]),
            &frame(0x02, 0x00, &[1, 0, SOF, SOF]),
        ]
        .concat();
        (stream, vec![(false, 7), (true, 0x1234), (false, 7)])
    }

    #[test]
    fn decoder_finds_every_frame_fed_a_byte_at_a_time_or_cut_anywhere() {
        let (stream, want) = noisy_stream();
        let mut d = Decoder::new();
        let mut got = Vec::new();
        for b in &stream {
            d.push(std::slice::from_ref(b));
            d.drain(|f| got.push((f.ack, f.seq)));
        }
        assert_eq!(got, want, "byte at a time");
        for cut in 0..=stream.len() {
            let mut d = Decoder::new();
            let mut got = Vec::new();
            d.push(&stream[..cut]);
            d.drain(|f| got.push((f.ack, f.seq)));
            d.push(&stream[cut..]);
            d.drain(|f| got.push((f.ack, f.seq)));
            assert_eq!(got, want, "cut at {cut}");
        }
    }

    #[test]
    fn truncated_and_undersized_frames() {
        let f = frame(0x02, 0x00, &[1, 0]);
        for n in 0..f.len() {
            assert!(
                matches!(parse(&f[..n]), Err(ParseError::Incomplete)),
                "{n} bytes"
            );
        }
        // A request whose length leaves no room for set/id and CRC.
        let mut short = vec![SOF, (OVERHEAD - 1) as u8, 0, 0, 0, 0, 0, 0, 0, 0];
        short.extend_from_slice(&crc16(&short).to_le_bytes());
        short.resize(OVERHEAD, 0);
        assert_eq!(parse(&short), Err(ParseError::BadLength));
        // An empty ACK is still an ACK (no code).
        let empty = ack(1, &[]);
        let (a, _) = parse(&empty).unwrap();
        assert!(a.ack && a.payload.is_empty());
    }

    #[test]
    fn decoder_resyncs_and_keeps_partial_tail() {
        let a = frame(0x02, 0x00, &[1, 0]);
        let b = frame(0x02, 0x00, &[1, 0, 5]);
        let mut d = Decoder::new();
        d.push(&[0xAA, 0x13, 0x55]); // junk with a false SOF
        d.push(&a);
        d.push(&b[..5]);
        let mut seen = 0;
        assert_eq!(d.drain(|_| seen += 1), 1);
        d.push(&b[5..]);
        assert_eq!(d.drain(|f| assert_eq!(f.payload, &[1, 0, 5])), 1);
        assert_eq!(seen, 1);
    }
}
