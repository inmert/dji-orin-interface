//! Telemetry: a serializable snapshot built from the DUML the aircraft streams
//! up through the controller, broadcast to the web UI over WebSocket.
//!
//! Field decoding is deliberately conservative. The stream/activity table is
//! exact (it only reports what frames arrived); per-feature decodes live with
//! their feature (e.g. [`gimbal::attitude`]) and are
//! marked `provisional` until calibrated against the hardware.

use std::collections::BTreeMap;

use crate::duml::{DevName, Frame};
use crate::gimbal;

/// Count and last size of one distinct DUML stream (sender→receiver set/id).
#[derive(Clone, Default, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
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
#[derive(Clone, Default, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Telemetry {
    /// True while frames are arriving (cleared when the link drops).
    pub link_up: bool,
    /// Total CRC-valid frames decoded.
    pub frames: u64,
    /// Latest attitude per gimbal index.
    pub gimbals: BTreeMap<u8, gimbal::Attitude>,
    /// Distinct streams seen, keyed `set/id sender>receiver` (hex), with
    /// running counts.
    pub streams: BTreeMap<String, StreamStat>,
}

impl Telemetry {
    /// Fold one decoded, CRC-valid frame into the snapshot.
    pub fn update(&mut self, f: &Frame) {
        self.link_up = true;
        self.frames += 1;

        // Key on the route as well as set/id: the same report from two
        // gimbals (or two senders) is two streams, not one.
        let key = format!(
            "{:#04x}/{:#04x} {:02x}>{:02x}",
            f.cmd_set, f.cmd_id, f.sender, f.receiver
        );
        let stat = self.streams.entry(key).or_insert_with(|| StreamStat {
            from: DevName(f.sender).to_string(),
            to: DevName(f.receiver).to_string(),
            cmd_set: f.cmd_set,
            cmd_id: f.cmd_id,
            ..Default::default()
        });
        stat.count += 1;
        stat.last_len = f.payload.len();

        if let Some(a) = gimbal::Attitude::decode(f) {
            self.gimbals.insert(a.index, a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duml::{self, Device};

    fn attitude_frame(index: u8) -> Vec<u8> {
        let header = duml::Header {
            sender: duml::addr(Device::Gimbal, index),
            receiver: duml::addr(Device::App, 0),
            seq: 1,
            cmd_type: 0x00,
            cmd_set: gimbal::CMD_SET,
            cmd_id: gimbal::CMD_ID_ATTITUDE,
        };
        // pitch=10, roll=0, yaw=-20 in the first three LE i16.
        duml::build(&header, &[0x0a, 0x00, 0x00, 0x00, 0xec, 0xff]).unwrap()
    }

    #[test]
    fn counts_streams_and_decodes_gimbal_attitude() {
        let frame = attitude_frame(2);
        let (f, _) = duml::parse(&frame).unwrap();

        let mut t = Telemetry::default();
        t.update(&f);

        assert!(t.link_up);
        assert_eq!(t.frames, 1);
        let g = t.gimbals.get(&2).expect("gimbal 2 decoded");
        assert_eq!((g.pitch, g.roll, g.yaw), (10, 0, -20));
        assert!(t.streams.contains_key("0x04/0x05 44>02"));
    }

    #[test]
    fn same_ids_from_two_senders_are_two_streams() {
        let mut t = Telemetry::default();
        for index in [0, 2] {
            let frame = attitude_frame(index);
            let (f, _) = duml::parse(&frame).unwrap();
            t.update(&f);
        }
        assert_eq!(t.streams.len(), 2);
        assert_eq!(t.gimbals.len(), 2);
    }
}
