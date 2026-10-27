//! Gimbal attitude reports: the `0x04/0x05` frames each gimbal pushes up.
//!
//! The angle decode is provisional (byte offsets not yet verified against the
//! hardware), so every [`Attitude`] carries the raw payload hex and a
//! `provisional` flag for the UI to show honestly.

use serde::Serialize;

use super::{CMD_ID_ATTITUDE, CMD_SET};
use crate::duml::{Device, Frame};
use crate::log::hex;

/// A best-effort decode of a gimbal attitude report.
#[derive(Serialize, Clone, Default, Debug)]
pub struct Attitude {
    /// Gimbal device index (0 = camera 1, 2 = camera 2).
    pub index: u8,
    /// Provisional pitch/roll/yaw, first three little-endian i16 of the report.
    pub pitch: i16,
    pub roll: i16,
    pub yaw: i16,
    /// Raw payload hex, so the UI can show ground truth while angles are
    /// unverified.
    pub raw: String,
    pub provisional: bool,
}

fn le_i16(b: &[u8], off: usize) -> i16 {
    b.get(off..off + 2)
        .map_or(0, |s| i16::from_le_bytes([s[0], s[1]]))
}

impl Attitude {
    /// Decode `f` if it is an attitude report sent by a gimbal; `None` for any
    /// other frame.
    pub fn decode(f: &Frame) -> Option<Attitude> {
        if f.cmd_set != CMD_SET
            || f.cmd_id != CMD_ID_ATTITUDE
            || Device::try_from(f.sender) != Ok(Device::Gimbal)
        {
            return None;
        }
        Some(Attitude {
            index: f.sender >> 5,
            pitch: le_i16(f.payload, 0),
            roll: le_i16(f.payload, 2),
            yaw: le_i16(f.payload, 4),
            raw: hex(f.payload),
            provisional: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duml::{self, Header};

    fn frame(sender: u8, payload: &[u8]) -> Vec<u8> {
        let header = Header {
            sender,
            receiver: duml::addr(Device::App, 0),
            seq: 1,
            cmd_type: 0x00,
            cmd_set: CMD_SET,
            cmd_id: CMD_ID_ATTITUDE,
        };
        duml::build(&header, payload).unwrap()
    }

    #[test]
    fn decodes_a_gimbal_report() {
        // pitch=10, roll=0, yaw=-20 in the first three LE i16.
        let bytes = frame(duml::addr(Device::Gimbal, 2), &[0x0a, 0, 0, 0, 0xec, 0xff]);
        let (f, _) = duml::parse(&bytes).unwrap();
        let a = Attitude::decode(&f).expect("decoded");
        assert_eq!(a.index, 2);
        assert_eq!((a.pitch, a.roll, a.yaw), (10, 0, -20));
        assert_eq!(a.raw, "0a 00 00 00 ec ff");
    }

    #[test]
    fn ignores_the_same_ids_from_a_non_gimbal() {
        let bytes = frame(duml::addr(Device::App, 0), &[0; 6]);
        let (f, _) = duml::parse(&bytes).unwrap();
        assert!(Attitude::decode(&f).is_none());
    }

    #[test]
    fn short_payload_reads_zero() {
        let bytes = frame(duml::addr(Device::Gimbal, 0), &[0x05]);
        let (f, _) = duml::parse(&bytes).unwrap();
        let a = Attitude::decode(&f).unwrap();
        assert_eq!((a.pitch, a.roll, a.yaw), (0, 0, 0));
    }
}
