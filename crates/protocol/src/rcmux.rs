//! The RC's USB packet mux: how the controller interleaves DUML and the
//! camera video on the bulk stream it sends the app (ep1 on our gadget).
//!
//! Two layers. The bulk stream is a chain of **outer frames**, each an 8-byte
//! header plus `len` payload bytes, with no padding:
//!
//! ```text
//!   55 cc | type | 57 | len (u16 LE) | 00 00 | payload…
//! ```
//!
//! * type `0x4a`: a slice of the **mux stream** below. Concatenated, these
//!   payloads are the mux packets; a packet may span outer frames.
//! * type `0x49`: DUML frames directly (most camera reports and ACKs travel
//!   here, not on the mux's DUML channel). Reported as [`DUML_DIRECT`].
//!
//! Each **mux packet** is a 12-byte header, then `len` payload bytes, then
//! zero padding so the packet ends on an 8-byte boundary. `check` is the XOR
//! of the ten header bytes before it (verified on every packet of a capture):
//!
//! ```text
//!   00 00 01 ff | len (u16 LE) | ff 00 4c | channel | check | 00 | payload…
//! ```
//!
//! Channels seen on the M210 V1 with a Cendence RC: [`DUML`] (`0x40`) and
//! three plain H.264 byte streams (Annex B): two 1280×720 gimbal-camera
//! feeds and the 608×448 FPV camera (see `docs/protocol.md`). The RC also
//! puts a bare 8-byte H.264 access-unit delimiter in the mux stream after
//! some packets, outside any packet; it is skipped (it carries no picture).
//!
//! In a 460 MB capture the outer frames chain over every byte with no
//! resync, and all but those delimiters parse as mux packets. Both layers
//! resynchronise after a loss. Payloads are not checksummed, so consumers
//! must tolerate garbage after a loss.

/// Channel carrying the DUML frames.
pub const DUML: u8 = 0x40;
/// Pseudo-channel for the payloads of type-`0x49` outer frames: DUML sent
/// outside the mux. A separate byte stream from [`DUML`]; decode them apart.
pub const DUML_DIRECT: u8 = 0x49;
/// Video channel of one 1280×720 gimbal camera (H.264 Baseline).
pub const VIDEO_A: u8 = 0x11;
/// Video channel of the FPV camera (H.264 Main, 608×448).
pub const VIDEO_FPV: u8 = 0x13;
/// Video channel of the other 1280×720 gimbal camera (H.264 High).
pub const VIDEO_B: u8 = 0x15;

const OUTER_HEADER: usize = 8;
const OUTER_MUX: u8 = 0x4a;
/// Longest outer frame payload accepted when resynchronising (8192 seen).
const OUTER_MAX: usize = 16 * 1024;

/// An outer frame header (`55 cc <type> 57 <len> 00 00`) of a known type,
/// returning its type and payload length.
fn outer_header(b: &[u8]) -> Option<(u8, usize)> {
    let b = b.get(..OUTER_HEADER)?;
    let len = usize::from(u16::from_le_bytes([b[4], b[5]]));
    (b[0] == 0x55
        && b[1] == 0xcc
        && b[3] == 0x57
        && b[6..8] == [0, 0]
        && matches!(b[2], OUTER_MUX | DUML_DIRECT)
        && len <= OUTER_MAX)
        .then_some((b[2], len))
}

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
    /// A partial outer frame carried over to the next read.
    outer: Vec<u8>,
    packets: Packets,
}

impl Demux {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `data` and call `packet(channel, payload)` for every complete
    /// mux packet, and with [`DUML_DIRECT`] for every type-`0x49` outer frame.
    /// Bytes outside any frame (garbage after a loss) are skipped; partial
    /// frames are kept for the next call.
    pub fn push(&mut self, data: &[u8], mut packet: impl FnMut(u8, &[u8])) {
        self.outer.extend_from_slice(data);
        let buf = &self.outer;
        let mut pos = 0;
        while pos < buf.len() {
            let Some((kind, len)) = outer_header(&buf[pos..]) else {
                if buf.len() - pos < OUTER_HEADER {
                    break; // maybe the start of a header: wait for more
                }
                // Lost sync: skip to the next plausible header.
                pos = (pos + 1..=buf.len() - OUTER_HEADER)
                    .find(|&i| outer_header(&buf[i..]).is_some())
                    .unwrap_or(buf.len() - (OUTER_HEADER - 1));
                continue;
            };
            let end = pos + OUTER_HEADER + len;
            if end > buf.len() {
                break;
            }
            let payload = &buf[pos + OUTER_HEADER..end];
            if kind == OUTER_MUX {
                self.packets.push(payload, &mut packet);
            } else {
                packet(DUML_DIRECT, payload);
            }
            pos = end;
        }
        self.outer.drain(..pos);
    }
}

/// The mux layer: whole packets out of the concatenated type-`0x4a` payloads.
#[derive(Default)]
struct Packets {
    buf: Vec<u8>,
}

impl Packets {
    fn push(&mut self, data: &[u8], packet: &mut impl FnMut(u8, &[u8])) {
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

    /// One outer frame around `payload`.
    fn outer(kind: u8, payload: &[u8]) -> Vec<u8> {
        let len = u16::try_from(payload.len()).unwrap().to_le_bytes();
        [&[0x55, 0xcc, kind, 0x57, len[0], len[1], 0, 0][..], payload].concat()
    }

    /// The mux stream `mux` cut into type-`0x4a` outer frames of `size` bytes.
    fn sliced(mux: &[u8], size: usize) -> Vec<u8> {
        mux.chunks(size).flat_map(|c| outer(OUTER_MUX, c)).collect()
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
        let mux = [
            &packet(VIDEO_A, &video)[..],
            &packet(DUML, &duml),
            &packet(VIDEO_B, &[]),
        ]
        .concat();
        let stream = [
            &[0x13, 0x37][..], // garbage before the first header
            &outer(OUTER_MUX, &mux),
            &outer(DUML_DIRECT, &duml),
        ]
        .concat();
        let want = vec![
            (VIDEO_A, video.to_vec()),
            (DUML, duml.to_vec()),
            (VIDEO_B, vec![]),
            (DUML_DIRECT, duml.to_vec()),
        ];
        for cut in 0..=stream.len() {
            assert_eq!(collect(&stream, cut), want, "cut at {cut}");
        }
    }

    #[test]
    fn reassembles_packets_that_span_outer_frames() {
        let video: Vec<u8> = (0..100).collect();
        let mux = [packet(VIDEO_A, &video), packet(VIDEO_B, &[7; 9])].concat();
        for size in [1, 7, 8, 13, 64] {
            let got = collect(&sliced(&mux, size), 0);
            assert_eq!(
                got,
                vec![(VIDEO_A, video.clone()), (VIDEO_B, vec![7; 9])],
                "{size}"
            );
        }
    }

    #[test]
    fn skips_the_bare_access_unit_delimiter_between_packets() {
        // As captured: an AUD packet, then the same delimiter with no header.
        let aud = [0, 0, 0, 1, 0x09, 0x10];
        let stream = [
            outer(OUTER_MUX, &packet(VIDEO_FPV, &aud)),
            outer(OUTER_MUX, &[0, 0, 0, 0, 0, 1, 0x09, 0x10]),
            outer(OUTER_MUX, &packet(DUML, &[1, 2])),
        ]
        .concat();
        assert_eq!(
            collect(&stream, 0),
            vec![(VIDEO_FPV, aud.to_vec()), (DUML, vec![1, 2])]
        );
    }

    #[test]
    fn resyncs_on_the_outer_frames_after_garbage() {
        let stream = [
            outer(DUML_DIRECT, &[1]),
            vec![0x55, 0xcc, 0x4a, 0x99, 1, 2, 3], // not a header: wrong marker
            outer(DUML_DIRECT, &[2]),
        ]
        .concat();
        assert_eq!(
            collect(&stream, 0),
            vec![(DUML_DIRECT, vec![1]), (DUML_DIRECT, vec![2])]
        );
    }

    #[test]
    fn resyncs_after_a_truncated_packet() {
        let mut broken = packet(VIDEO_A, &[9; 40]);
        broken.truncate(20); // header claims 40 bytes; the rest was lost
        let stream = outer(OUTER_MUX, &[broken, packet(VIDEO_B, &[7; 8])].concat());
        // The broken packet swallows the next header as payload until its
        // claimed length runs out, so only what follows is guaranteed: here,
        // nothing complete arrives until more data pushes past it.
        let mut d = Demux::new();
        let mut out = Vec::new();
        d.push(&stream, |c, p| out.push((c, p.len())));
        d.push(&outer(OUTER_MUX, &packet(DUML, &[1, 2, 3])), |c, p| {
            out.push((c, p.len()))
        });
        d.push(&outer(OUTER_MUX, &packet(DUML, &[4])), |c, p| {
            out.push((c, p.len()))
        });
        assert_eq!(out.last(), Some(&(DUML, 1)));
    }
}
