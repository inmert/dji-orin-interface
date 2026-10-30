//! The flight controller's `0x03/0x43` general-data report.
//!
//! Offsets were matched against the OSDK UART broadcast in a simultaneous
//! capture (2026-10-28, aircraft on the ground indoors). Attitude, flight mode
//! and satellite count matched; position, height and velocity read zero there
//! (no fix, at rest), so those offsets are not verified yet and are reported
//! separately as [`Osd::unverified`].

use super::{CMD_ID_OSD, CMD_SET};
use crate::duml::{Device, Frame};
use crate::osdk::broadcast::mode_name;

/// Shortest payload holding every field we decode (satellites at byte 36).
const MIN_LEN: usize = 37;

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Osd {
    /// Degrees (wire unit 0.1°). Verified.
    pub pitch: f32,
    pub roll: f32,
    pub yaw: f32,
    /// Flight mode, same numbering as the OSDK display mode. Verified.
    pub mode: u8,
    pub mode_name: &'static str,
    /// GPS satellites in use. Verified at 0 only.
    pub satellites: u8,
    pub unverified: Unverified,
}

/// Fields whose offsets have only been seen at zero.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Unverified {
    /// Degrees (wire: f64 radians).
    pub latitude: f64,
    pub longitude: f64,
    /// m (wire unit 0.1 m).
    pub height: f32,
    /// m/s (wire unit 0.1 m/s).
    pub velocity: [f32; 3],
}

fn i16_at(p: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([p[off], p[off + 1]])
}

fn f64_at(p: &[u8], off: usize) -> f64 {
    f64::from_le_bytes(p[off..off + 8].try_into().unwrap())
}

/// Degrees from the wire's f64 radians; 0 (no fix) for a value that is not
/// a number, which unverified offsets can decode to and JSON cannot carry.
fn degrees(radians: f64) -> f64 {
    let d = radians.to_degrees();
    if d.is_finite() { d } else { 0.0 }
}

fn tenths(v: i16) -> f32 {
    f32::from(v) / 10.0
}

impl Osd {
    /// Decode `f` if it is a general-data report from the flight controller;
    /// `None` for any other frame or a truncated report.
    pub fn decode(f: &Frame) -> Option<Osd> {
        let p = f.payload;
        if f.cmd_set != CMD_SET
            || f.cmd_id != CMD_ID_OSD
            || Device::try_from(f.sender) != Ok(Device::FlightCtrl)
            || p.len() < MIN_LEN
        {
            return None;
        }
        let mode = p[30];
        Some(Osd {
            pitch: tenths(i16_at(p, 24)),
            roll: tenths(i16_at(p, 26)),
            yaw: tenths(i16_at(p, 28)),
            mode,
            mode_name: mode_name(mode),
            satellites: p[36],
            unverified: Unverified {
                longitude: degrees(f64_at(p, 0)),
                latitude: degrees(f64_at(p, 8)),
                height: tenths(i16_at(p, 16)),
                velocity: [18, 20, 22].map(|o| tenths(i16_at(p, o))),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duml::{self, Header};

    /// The first 0x03/0x43 payload from the 2026-10-28 ttyACM0 capture
    /// (no GPS fix, so position is zero).
    const CAPTURED: &str = "000000000000000000000000000000000000000000000000 \
                            0d00170096050600f01180000000000000010000000f8a1f1c0000c900ffff";

    fn payload() -> Vec<u8> {
        let hex: String = CAPTURED.split_whitespace().collect();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn frame(sender: u8, payload: &[u8]) -> Vec<u8> {
        let header = Header {
            sender,
            receiver: duml::addr(Device::Pc, 0),
            seq: 1,
            cmd_type: 0x00,
            cmd_set: CMD_SET,
            cmd_id: CMD_ID_OSD,
        };
        duml::build(&header, payload).unwrap()
    }

    #[test]
    fn decodes_the_captured_report() {
        let p = payload();
        assert_eq!(p.len(), 55);
        let bytes = frame(duml::addr(Device::FlightCtrl, 0), &p);
        let (f, _) = duml::parse(&bytes).unwrap();
        let o = Osd::decode(&f).expect("decoded");
        assert_eq!((o.pitch, o.roll, o.yaw), (1.3, 2.3, 143.0));
        assert_eq!((o.mode, o.mode_name, o.satellites), (6, "P-GPS", 0));
        assert_eq!(o.unverified, Unverified::default());
    }

    #[test]
    fn unrepresentable_positions_read_as_no_fix() {
        let mut p = payload();
        p[0..8].copy_from_slice(&f64::NAN.to_le_bytes());
        p[8..16].copy_from_slice(&f64::INFINITY.to_le_bytes());
        let bytes = frame(duml::addr(Device::FlightCtrl, 0), &p);
        let o = Osd::decode(&duml::parse(&bytes).unwrap().0).unwrap();
        assert_eq!((o.unverified.latitude, o.unverified.longitude), (0.0, 0.0));
        assert_eq!(o.yaw, 143.0, "the verified fields are untouched");
    }

    #[test]
    fn ignores_other_senders_and_truncated_reports() {
        let p = payload();
        let bytes = frame(duml::addr(Device::Gimbal, 0), &p);
        assert!(Osd::decode(&duml::parse(&bytes).unwrap().0).is_none());
        let bytes = frame(duml::addr(Device::FlightCtrl, 0), &p[..MIN_LEN - 1]);
        assert!(Osd::decode(&duml::parse(&bytes).unwrap().0).is_none());
    }
}
