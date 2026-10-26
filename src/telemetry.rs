//! Telemetry: a serializable snapshot built from the DUML the aircraft streams
//! up through the controller, broadcast to the web UI over WebSocket.
//!
//! Field decoding is deliberately conservative. The stream/activity table is
//! exact (it only reports what frames arrived), while per-sensor decodes we are
//! not yet sure of — currently the gimbal attitude — are marked `provisional`
//! and carry the raw payload hex so the UI can show them honestly and we can
//! calibrate them against the physical hardware.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::duml::{DevName, Frame};

/// Gimbal command set / attitude-report id (the report the gimbals push up).
const CMD_SET_GIMBAL: u8 = 0x04;
const CMD_ID_GIMBAL_ATTITUDE: u8 = 0x05;

/// A best-effort decode of a gimbal attitude report. The angle fields are
/// provisional (byte offsets not yet verified against the hardware).
#[derive(Serialize, Clone, Default)]
pub struct GimbalAttitude {
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

/// Count and last size of one distinct DUML stream (a sender→receiver set/id).
#[derive(Serialize, Clone, Default)]
pub struct StreamStat {
    pub from: String,
    pub to: String,
    pub cmd_set: u8,
    pub cmd_id: u8,
    pub count: u64,
    pub last_len: usize,
}

/// Snapshot of everything we currently know from the link, serialized to the
/// UI. Cheap to clone for each broadcast.
#[derive(Serialize, Clone, Default)]
pub struct Telemetry {
    /// True once any valid frame has arrived.
    pub link_up: bool,
    /// Total CRC-valid frames decoded.
    pub frames: u64,
    /// Latest attitude per gimbal index.
    pub gimbals: BTreeMap<u8, GimbalAttitude>,
    /// Distinct streams seen, keyed `set/id` (hex), with running counts.
    pub streams: BTreeMap<String, StreamStat>,
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn le_i16(b: &[u8], off: usize) -> i16 {
    if off + 2 <= b.len() {
        i16::from_le_bytes([b[off], b[off + 1]])
    } else {
        0
    }
}

impl Telemetry {
    /// Fold one decoded, CRC-valid frame into the snapshot.
    pub fn update(&mut self, f: &Frame) {
        self.link_up = true;
        self.frames += 1;

        let key = format!("{:#04x}/{:#04x}", f.cmd_set, f.cmd_id);
        let stat = self.streams.entry(key).or_insert_with(|| StreamStat {
            from: DevName(f.sender).to_string(),
            to: DevName(f.receiver).to_string(),
            cmd_set: f.cmd_set,
            cmd_id: f.cmd_id,
            ..Default::default()
        });
        stat.count += 1;
        stat.last_len = f.payload.len();

        if f.cmd_set == CMD_SET_GIMBAL && f.cmd_id == CMD_ID_GIMBAL_ATTITUDE {
            let index = f.sender >> 5;
            self.gimbals.insert(
                index,
                GimbalAttitude {
                    index,
                    pitch: le_i16(f.payload, 0),
                    roll: le_i16(f.payload, 2),
                    yaw: le_i16(f.payload, 4),
                    raw: hex(f.payload),
                    provisional: true,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{self, Camera, GimbalMove};
    use crate::duml;

    #[test]
    fn counts_streams_and_decodes_gimbal_attitude() {
        // A synthetic gimbal attitude report from camera 2 (index 2).
        let header = duml::Header {
            sender: control::addr(duml::Device::Gimbal, 2),
            receiver: control::addr(duml::Device::App, 0),
            seq: 1,
            cmd_type: 0x00,
            cmd_set: CMD_SET_GIMBAL,
            cmd_id: CMD_ID_GIMBAL_ATTITUDE,
        };
        // pitch=10, roll=0, yaw=-20 in the first three LE i16.
        let payload = [0x0a, 0x00, 0x00, 0x00, 0xec, 0xff];
        let frame = duml::build(&header, &payload).unwrap();
        let (f, _) = duml::parse(&frame).unwrap();

        let mut t = Telemetry::default();
        t.update(&f);
        // keep GimbalMove referenced so the test imports stay meaningful
        let _ = GimbalMove::default();
        let _ = Camera::Two;

        assert!(t.link_up);
        assert_eq!(t.frames, 1);
        let g = t.gimbals.get(&2).expect("gimbal 2 decoded");
        assert_eq!((g.pitch, g.roll, g.yaw), (10, 0, -20));
        assert!(t.streams.contains_key("0x04/0x05"));
    }
}
