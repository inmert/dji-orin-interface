//! Telemetry: a serializable snapshot built from the DUML the aircraft streams
//! up through the controller, plus the flight-data broadcast from its OSDK
//! UART ([`FlightLink`]) and the DUML on its USB API port ([`UsbLink`]),
//! broadcast to the web UI over WebSocket.
//!
//! Field decoding is deliberately conservative. The stream/activity table is
//! exact (it only reports what frames arrived); per-feature decodes live with
//! their feature (e.g. [`gimbal::attitude`]) and are
//! marked `provisional` until calibrated against the hardware.

use std::collections::BTreeMap;

use crate::duml::{DevName, Frame};
use crate::flyc;
use crate::gimbal;
use crate::osdk::{self, broadcast};

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
    /// Flight data from the OSDK UART, independent of the DUML link above.
    pub flight: FlightLink,
    /// DUML from the aircraft's USB API port (`ttyACM0`), also independent.
    pub usb: UsbLink,
}

/// The aircraft's USB API port (receive-only): the reports it pushes unasked.
#[derive(Clone, Default, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct UsbLink {
    /// Serial port being read; empty when the reader is disabled.
    pub port: String,
    /// The port is open and being read.
    pub open: bool,
    /// CRC-valid DUML frames received.
    pub frames: u64,
    /// Latest flight controller general-data report.
    pub osd: Option<flyc::Osd>,
    /// Latest attitude per gimbal index (wire units 0.1°).
    pub gimbals: BTreeMap<u8, gimbal::Attitude>,
}

impl UsbLink {
    /// Fold one CRC-valid DUML frame in.
    pub fn update(&mut self, f: &Frame) {
        self.frames += 1;
        if let Some(o) = flyc::Osd::decode(f) {
            self.osd = Some(o);
        } else if let Some(a) = gimbal::Attitude::decode(f) {
            self.gimbals.insert(a.index, a);
        }
    }
}

/// The aircraft's OSDK UART flight-data broadcast (receive-only).
#[derive(Clone, Default, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FlightLink {
    /// Serial port being read; empty when the reader is disabled.
    pub port: String,
    /// The port is open and being read.
    pub open: bool,
    /// Broadcast frames decoded.
    pub frames: u64,
    /// Broadcast frames that matched no known layout, or were encrypted.
    pub rejected: u64,
    pub data: broadcast::FlightData,
}

impl FlightLink {
    /// Fold one CRC-valid OpenProtocol frame in; other commands are ignored.
    pub fn update(&mut self, f: &osdk::Frame) {
        if (f.cmd_set, f.cmd_id) != (broadcast::CMD_SET, broadcast::CMD_ID) {
            return;
        }
        if f.enc == 0 && self.data.apply(f.payload).is_ok() {
            self.frames += 1;
        } else {
            self.rejected += 1;
        }
    }
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
    fn truncated_report_counts_as_traffic_but_preserves_last_attitude() {
        let mut t = Telemetry::default();
        let bytes = attitude_frame(2);
        let (valid, _) = duml::parse(&bytes).unwrap();
        t.update(&valid);
        let mut truncated = valid.clone();
        truncated.payload = &valid.payload[..1];
        t.update(&truncated);
        assert_eq!(t.frames, 2);
        let attitude = &t.gimbals[&2];
        assert_eq!((attitude.pitch, attitude.roll, attitude.yaw), (10, 0, -20));
    }

    #[test]
    fn flight_link_counts_broadcasts_and_rejects() {
        let mut l = FlightLink::default();
        let mut time_only = vec![1, 0];
        time_only.extend_from_slice(&42u32.to_le_bytes());
        time_only.extend_from_slice(&[0; 11]);
        let frame = |set, payload, enc| osdk::Frame {
            session: 0,
            ack: false,
            enc,
            seq: 0,
            cmd_set: set,
            cmd_id: 0,
            payload,
        };
        l.update(&frame(broadcast::CMD_SET, &time_only, 0));
        l.update(&frame(broadcast::CMD_SET, &time_only, 1)); // encrypted
        l.update(&frame(broadcast::CMD_SET, &time_only[..5], 0)); // bad layout
        l.update(&frame(0x00, &time_only, 0)); // not a broadcast
        assert_eq!((l.frames, l.rejected), (1, 2));
        assert_eq!(l.data.time_ms, Some(42));
    }

    #[test]
    fn usb_link_keeps_latest_fc_and_gimbal_reports() {
        let mut u = UsbLink::default();
        let gimbal = attitude_frame(0);
        u.update(&duml::parse(&gimbal).unwrap().0);
        let header = duml::Header {
            sender: duml::addr(Device::FlightCtrl, 0),
            receiver: duml::addr(Device::Pc, 0),
            seq: 2,
            cmd_type: 0x00,
            cmd_set: flyc::CMD_SET,
            cmd_id: flyc::CMD_ID_OSD,
        };
        let mut p = vec![0u8; 55];
        p[28..30].copy_from_slice(&1430i16.to_le_bytes());
        p[30] = 6;
        let osd = duml::build(&header, &p).unwrap();
        u.update(&duml::parse(&osd).unwrap().0);
        assert_eq!(u.frames, 2);
        assert_eq!(u.gimbals[&0].yaw, -20);
        let o = u.osd.as_ref().unwrap();
        assert_eq!((o.yaw, o.mode_name), (143.0, "P-GPS"));
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
