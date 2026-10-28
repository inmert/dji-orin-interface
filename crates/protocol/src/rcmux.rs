//! The RC's USB packet mux: how the controller interleaves DUML and the
//! camera video on the bulk stream it sends the app (ep1 on our gadget).
//!
//! Every packet is a 12-byte header, then `len` payload bytes, then zero
//! padding so the packet ends on an 8-byte boundary. `check` is the XOR of
//! the ten header bytes before it (verified on every packet of a capture):
//!
//! ```text
//!   00 00 01 ff | len (u16 LE) | ff 00 4c | channel | check | 00 | payload…
//! ```
//!
//! Channels seen on the M210 V1 with a Cendence RC: [`DUML`] (`0x40`) and
//! three plain H.264 byte streams (Annex B): two 1280×720 gimbal-camera
//! feeds and the 608×448 FPV camera (see `docs/protocol.md`). The demuxer
//! resynchronises on the header magic plus `check`, so a lost byte costs at
//! most one packet. Payloads are not checksummed: the RC occasionally sends
//! stray bytes inside a video packet, so consumers must tolerate garbage.

/// Channel carrying the DUML frames.
pub const DUML: u8 = 0x40;
/// Video channel of one 1280×720 gimbal camera (H.264 Baseline).
pub const VIDEO_A: u8 = 0x11;
/// Video channel of the FPV camera (H.264 Main, 608×448).
pub const VIDEO_FPV: u8 = 0x13;
/// Video channel of the other 1280×720 gimbal camera (H.264 High).
pub const VIDEO_B: u8 = 0x15;

const HEADER: usize = 12;
const MAGIC_HEAD: [u8; 4] = [0x00, 0x00, 0x01, 0xff];
const MAGIC_TAIL: [u8; 3] = [0xff, 0x00, 0x4c];

fn is_header(b: &[u8]) -> bool {
    b.len() >= HEADER
        && b[..4] == MAGIC_HEAD
        && b[6..9] == MAGIC_TAIL
        && b[..10].iter().fold(0, |x, &v| x ^ v) == b[10]
}

/// Streaming demuxer: feed it reads of any size, get whole packets back.
#[derive(Default)]
pub struct Demux {
    buf: Vec<u8>,
}

impl Demux {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `data` and call `packet(channel, payload)` for every complete
    /// packet. Bytes before a header (padding, or garbage after a loss) are
    /// skipped; a partial packet is kept for the next call.
    pub fn push(&mut self, data: &[u8], mut packet: impl FnMut(u8, &[u8])) {
        self.buf.extend_from_slice(data);
        let mut pos = 0;
        loop {
            let Some(start) = (pos..self.buf.len().saturating_sub(HEADER - 1))
                .find(|&i| is_header(&self.buf[i..]))
            else {
                // Keep a tail that may hold the start of the next header.
                pos = self.buf.len().saturating_sub(HEADER - 1).max(pos);
                break;
            };
            let len = usize::from(u16::from_le_bytes([
                self.buf[start + 4],
                self.buf[start + 5],
            ]));
            let end = start + HEADER + len;
            if end > self.buf.len() {
                pos = start;
                break;
            }
            packet(self.buf[start + 9], &self.buf[start + HEADER..end]);
            pos = end;
        }
        self.buf.drain(..pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(channel: u8, payload: &[u8]) -> Vec<u8> {
        let len = u16::try_from(payload.len()).unwrap().to_le_bytes();
        let mut p = vec![
            0x00, 0x00, 0x01, 0xff, len[0], len[1], 0xff, 0x00, 0x4c, channel,
        ];
        p.push(p.iter().fold(0, |x, &v| x ^ v));
        p.push(0x00);
        p.extend_from_slice(payload);
        p.resize(p.len().div_ceil(8) * 8, 0);
        p
    }

    #[test]
    fn check_byte_matches_a_captured_header() {
        // Channel 0x11, 2036 bytes, from a real capture: check 0xaf.
        let h = [
            0x00, 0x00, 0x01, 0xff, 0xf4, 0x07, 0xff, 0x00, 0x4c, 0x11, 0xaf, 0x00,
        ];
        assert!(is_header(&h));
        let mut bad = h;
        bad[10] ^= 1;
        assert!(!is_header(&bad));
    }

    fn collect(stream: &[u8], cut: usize) -> Vec<(u8, Vec<u8>)> {
        let mut d = Demux::new();
        let mut out = Vec::new();
        d.push(&stream[..cut], |c, p| out.push((c, p.to_vec())));
        d.push(&stream[cut..], |c, p| out.push((c, p.to_vec())));
        out
    }

    #[test]
    fn splits_channels_across_every_read_boundary() {
        let video = [0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, 0, 0, 1];
        let duml = [0x55, 0x0d, 0x04, 0x33, 0x0a, 0x02];
        let stream = [
            &[0x13, 0x37][..], // garbage before the first header
            &packet(VIDEO_A, &video),
            &packet(DUML, &duml),
            &packet(VIDEO_B, &[]),
        ]
        .concat();
        let want = vec![
            (VIDEO_A, video.to_vec()),
            (DUML, duml.to_vec()),
            (VIDEO_B, vec![]),
        ];
        for cut in 0..=stream.len() {
            assert_eq!(collect(&stream, cut), want, "cut at {cut}");
        }
    }

    #[test]
    fn resyncs_after_a_truncated_packet() {
        let mut broken = packet(VIDEO_A, &[9; 40]);
        broken.truncate(20); // header claims 40 bytes; the rest was lost
        let stream = [broken, packet(VIDEO_B, &[7; 8])].concat();
        // The broken packet swallows the next header as payload until its
        // claimed length runs out, so only what follows is guaranteed: here,
        // nothing complete arrives until more data pushes past it.
        let mut d = Demux::new();
        let mut out = Vec::new();
        d.push(&stream, |c, p| out.push((c, p.len())));
        d.push(&packet(DUML, &[1, 2, 3]), |c, p| out.push((c, p.len())));
        d.push(&packet(DUML, &[4]), |c, p| out.push((c, p.len())));
        assert_eq!(out.last(), Some(&(DUML, 1)));
    }
}
