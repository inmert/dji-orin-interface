//! The Zenmuse XT2's thermal core: its measurement report and the settings
//! the UI sends, decoded from DJI Pilot traffic captured through the bridge
//! (2026-10-28, see `docs/protocol.md`).
//!
//! The XT2 on gimbal 0 has its thermal core at `Camera[1]`. It pushes
//! `cmd_set 0x02, cmd_id 0xF2` ("Tau param", ~6 Hz) to the app, and takes
//! settings as `0x02/0xF1` with a one-byte parameter id:
//!
//! * `0x87` + `f32 x, f32 y` — spot-meter position, normalised 0..1 over the
//!   thermal image. Accepted only in spot measurement (`0x8E 01`); otherwise
//!   the camera ACKs `ff`.
//! * `0x8B` / `0x8C` / `0x8D` + `i16` — isotherm lower / middle / upper, °C.
//!   The camera rejects values that break lower ≤ middle ≤ upper.
//! * `0x8E` + `u8` — measurement type ([`Measure`]).
//! * `0x97` + `u16 left, top, right, bottom` — area-measurement rectangle in
//!   1/10000 of the image.
//! * `0x98` + `00, mode` — display mode ([`DisplayMode`]).
//!
//! The camera ACKs each with `00` and echoes the new value in its next report.
//! Pilot sends them with command type `0x40` (ACK requested) from `App[0]`.

use crate::duml::{self, Device, Frame, Header};

pub const CMD_SET: u8 = 0x02;
/// Settings command (parameter id + value).
pub const CMD_ID_SET: u8 = 0xf1;
/// Measurement report pushed by the thermal core.
pub const CMD_ID_REPORT: u8 = 0xf2;
/// The thermal core's camera index (`Camera[1]`).
pub const CAMERA_INDEX: u8 = 1;

const PARAM_SPOT: u8 = 0x87;
const PARAM_MEASURE: u8 = 0x8e;
const PARAM_AREA: u8 = 0x97;
const PARAM_DISPLAY_MODE: u8 = 0x98;
/// Area coordinates are in 1/10000 of the image.
const AREA_SCALE: f32 = 10000.0;

/// What the thermal camera's video channel shows. Values as Pilot sends them;
/// only `Visual` (0) and `Thermal` (3) were seen to change the picture as
/// named. `Pip` (2) showed the visual picture in the capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DisplayMode {
    Visual = 0,
    Pip = 2,
    Thermal = 3,
}

impl DisplayMode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Visual),
            2 => Some(Self::Pip),
            3 => Some(Self::Thermal),
            _ => None,
        }
    }
}

/// What the camera measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Measure {
    Off = 0,
    /// One point ([`spot_frame`]); the report's spot fields.
    Spot = 1,
    /// A rectangle ([`area_frame`]); the report's area statistics.
    Area = 2,
}

/// One of the three isotherm bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Isotherm {
    Lower = 0x8b,
    Middle = 0x8c,
    Upper = 0x8d,
}

fn settings_frame(payload: &[u8], seq: u16) -> Vec<u8> {
    let header = Header {
        sender: duml::addr(Device::App, 0),
        receiver: duml::addr(Device::Camera, CAMERA_INDEX),
        seq,
        cmd_type: 0x40,
        cmd_set: CMD_SET,
        cmd_id: CMD_ID_SET,
    };
    duml::build(&header, payload).expect("thermal settings frame within length limit")
}

/// Move the spot meter to `(x, y)`, each clamped to 0..=1 of the thermal image.
pub fn spot_frame(x: f32, y: f32, seq: u16) -> Vec<u8> {
    let mut payload = vec![PARAM_SPOT];
    payload.extend_from_slice(&x.clamp(0.0, 1.0).to_le_bytes());
    payload.extend_from_slice(&y.clamp(0.0, 1.0).to_le_bytes());
    settings_frame(&payload, seq)
}

/// Set an isotherm bound, in °C.
pub fn isotherm_frame(bound: Isotherm, celsius: i16, seq: u16) -> Vec<u8> {
    let [lo, hi] = celsius.to_le_bytes();
    settings_frame(&[bound as u8, lo, hi], seq)
}

/// Choose what the camera measures.
pub fn measure_frame(measure: Measure, seq: u16) -> Vec<u8> {
    settings_frame(&[PARAM_MEASURE, measure as u8], seq)
}

/// Set the area-measurement rectangle, each edge clamped to 0..=1 of the image.
pub fn area_frame(left: f32, top: f32, right: f32, bottom: f32, seq: u16) -> Vec<u8> {
    let mut payload = vec![PARAM_AREA];
    for v in [left, top, right, bottom] {
        // In range after the clamp, so the cast cannot truncate.
        let units = (v.clamp(0.0, 1.0) * AREA_SCALE).round() as u16;
        payload.extend_from_slice(&units.to_le_bytes());
    }
    settings_frame(&payload, seq)
}

/// Switch what the thermal camera's video shows.
pub fn display_mode_frame(mode: DisplayMode, seq: u16) -> Vec<u8> {
    settings_frame(&[PARAM_DISPLAY_MODE, 0x00, mode as u8], seq)
}

/// The camera's reply to one of our settings: the `seq` it answers and the
/// status byte (`00` accepted, `ff` rejected). `None` for any other frame.
pub fn ack(f: &Frame) -> Option<(u16, u8)> {
    let is_ack = f.cmd_type & 0x80 != 0;
    if !is_ack
        || (f.cmd_set, f.cmd_id) != (CMD_SET, CMD_ID_SET)
        || f.sender != duml::addr(Device::Camera, CAMERA_INDEX)
        || f.receiver != duml::addr(Device::App, 0)
    {
        return None;
    }
    Some((f.seq, *f.payload.first()?))
}

/// The fields of the `0xF2` report the UI uses. Offsets follow the
/// dji-firmware-tools "Camera Tau Param" layout, which matched live data.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Report {
    /// Spot-meter position, normalised 0..1 over the thermal image.
    pub spot_x: f32,
    pub spot_y: f32,
    /// Temperature at the spot, °C.
    pub spot_temp: f32,
    /// The spot reading is valid (byte 48, bit 0x80); briefly clear while the
    /// camera moves the spot.
    pub spot_valid: bool,
    /// Display mode as reported (byte 94); see [`DisplayMode`].
    pub display_mode: Option<u8>,
    /// Measurement type (byte 39); see [`Measure`].
    pub measure: u8,
    /// Isotherm enabled (byte 31, bit 0) and its bounds in °C (bytes 33/35/37).
    pub isotherm_enabled: bool,
    pub isotherm_lower: i16,
    pub isotherm_middle: i16,
    pub isotherm_upper: i16,
    /// Area statistics, present in area measurement.
    pub area: Option<AreaStats>,
}

/// Area measurement results; positions normalised 0..1 over the thermal image.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AreaStats {
    /// The measured rectangle: left, top, right, bottom.
    pub rect: [f32; 4],
    pub avg: f32,
    pub min: f32,
    pub max: f32,
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl Report {
    /// Decode a report frame from the thermal core, or `None` for any other frame.
    pub fn decode(f: &Frame) -> Option<Self> {
        if (f.cmd_set, f.cmd_id) != (CMD_SET, CMD_ID_REPORT)
            || f.sender != duml::addr(Device::Camera, CAMERA_INDEX)
        {
            return None;
        }
        let p = f.payload;
        let f32_at = |o: usize| {
            p.get(o..o + 4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        };
        let u16_at = |o: usize| p.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let i16_at = |o: usize| p.get(o..o + 2).map(|b| i16::from_le_bytes([b[0], b[1]]));
        let unit = |o: usize| u16_at(o).map(|v| f32::from(v) / AREA_SCALE);
        let spot_x = f32_at(17)?;
        let spot_y = f32_at(21)?;
        let spot_temp = f32_at(25)?;
        let flags = *p.get(48)?;
        let measure = *p.get(39)?;
        // JSON has no NaN or infinity (serde_json writes `null`), so a
        // non-finite reading is dropped here rather than breaking the UI's
        // snapshot: the spot reads as invalid, the area as absent.
        let spot_ok = [spot_x, spot_y, spot_temp].iter().all(|v| v.is_finite());
        let area = if measure == Measure::Area as u8 {
            Some(AreaStats {
                rect: [unit(66)?, unit(68)?, unit(70)?, unit(72)?],
                avg: f32_at(74)?,
                min: f32_at(78)?,
                max: f32_at(82)?,
                min_x: unit(86)?,
                min_y: unit(88)?,
                max_x: unit(90)?,
                max_y: unit(92)?,
            })
            .filter(|a| [a.avg, a.min, a.max].iter().all(|v| v.is_finite()))
        } else {
            None
        };
        let (spot_x, spot_y, spot_temp) = if spot_ok {
            (spot_x, spot_y, spot_temp)
        } else {
            (0.0, 0.0, 0.0)
        };
        Some(Self {
            spot_x,
            spot_y,
            spot_temp,
            spot_valid: spot_ok && flags & 0x80 != 0,
            display_mode: p.get(94).copied(),
            measure,
            isotherm_enabled: p.get(31)? & 1 != 0,
            isotherm_lower: i16_at(33)?,
            isotherm_middle: i16_at(35)?,
            isotherm_upper: i16_at(37)?,
            area,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_frame_matches_pilot_byte_for_byte() {
        // Captured from DJI Pilot (seq 0x507f): spot at 0.4977, 0.5534.
        let pilot = [
            0x55, 0x16, 0x04, 0xfc, 0x02, 0x21, 0x7f, 0x50, 0x40, 0x02, 0xf1, 0x87, 0xf3, 0xcc,
            0xfe, 0x3e, 0x64, 0xa8, 0x0d, 0x3f, 0x85, 0xc0,
        ];
        let x = f32::from_le_bytes([0xf3, 0xcc, 0xfe, 0x3e]);
        let y = f32::from_le_bytes([0x64, 0xa8, 0x0d, 0x3f]);
        assert_eq!(spot_frame(x, y, 0x507f), pilot);
    }

    #[test]
    fn display_mode_frame_matches_pilot_byte_for_byte() {
        // Captured from DJI Pilot (seq 0x4843): display mode 3 (thermal).
        let pilot = [
            0x55, 0x10, 0x04, 0x56, 0x02, 0x21, 0x43, 0x48, 0x40, 0x02, 0xf1, 0x98, 0x00, 0x03,
            0x91, 0x47,
        ];
        assert_eq!(display_mode_frame(DisplayMode::Thermal, 0x4843), pilot);
    }

    #[test]
    fn isotherm_measure_and_area_frames_match_pilot_byte_for_byte() {
        fn seq(frame: &[u8]) -> u16 {
            u16::from_le_bytes([frame[6], frame[7]])
        }
        type Build<'a> = &'a dyn Fn(u16) -> Vec<u8>;
        let cases: [(Vec<u8>, Build); 6] = [
            (
                hex("55 10 04 56 02 21 8b c7 40 02 f1 8b 1b 00 84 72"),
                &|s| isotherm_frame(Isotherm::Lower, 27, s),
            ),
            (
                hex("55 10 04 56 02 21 38 e2 40 02 f1 8c 95 00 c7 d7"),
                &|s| isotherm_frame(Isotherm::Middle, 149, s),
            ),
            (
                hex("55 10 04 56 02 21 ca c7 40 02 f1 8d 1a 02 d9 7a"),
                &|s| isotherm_frame(Isotherm::Upper, 538, s),
            ),
            (hex("55 0f 04 a2 02 21 5a fe 40 02 f1 8e 01 fc e3"), &|s| {
                measure_frame(Measure::Spot, s)
            }),
            (hex("55 0f 04 a2 02 21 bf fe 40 02 f1 8e 02 5c 03"), &|s| {
                measure_frame(Measure::Area, s)
            }),
            (
                hex("55 16 04 fc 02 21 d1 fe 40 02 f1 97 7f 11 fc 10 90 15 13 16 20 5e"),
                &|s| area_frame(0.4479, 0.4348, 0.552, 0.5651, s),
            ),
        ];
        for (pilot, build) in cases {
            assert_eq!(build(seq(&pilot)), pilot);
        }
        // Negative bounds are signed: Pilot's earlier lower bound read 65496 = -40.
        assert_eq!(
            &isotherm_frame(Isotherm::Lower, -40, 0)[12..14],
            [0xd8, 0xff]
        );
    }

    #[test]
    fn decodes_a_captured_area_report() {
        // An area-mode report: isotherm 27/177/695, area avg/min/max 18.8/17.2/20.7.
        let payload = hex(concat!(
            "01 00 02 01 64 00 18 03 14 00 03 00 0f 00 0f 00 10 00 00 00 ",
            "3f 00 00 00 3f 66 66 96 41 00 00 01 01 1b 00 b1 00 b7 02 02 ",
            "00 00 00 04 01 00 02 01 81 00 10 27 10 27 98 08 98 08 10 27 ",
            "98 08 00 00 98 08 7f 11 fc 10 10 20 2f 1e 66 66 96 41 9a 99 ",
            "89 41 9a 99 a5 41 a7 13 ef 10 59 13 22 1e 03 0a 0f 00 00 00 ",
        ));
        let r = report(&payload);
        assert_eq!(r.measure, Measure::Area as u8);
        assert!(r.isotherm_enabled);
        assert_eq!(
            (r.isotherm_lower, r.isotherm_middle, r.isotherm_upper),
            (27, 177, 695)
        );
        let a = r.area.unwrap();
        assert_eq!(a.rect, [0.4479, 0.4348, 0.8208, 0.7727]);
        assert!(
            (a.avg - 18.8).abs() < 0.01
                && (a.min - 17.2).abs() < 0.01
                && (a.max - 20.7).abs() < 0.01
        );
        assert_eq!(
            (a.min_x, a.min_y, a.max_x, a.max_y),
            (0.5031, 0.4335, 0.4953, 0.7714)
        );
    }

    fn report(payload: &[u8]) -> Report {
        let frame = duml::build(
            &Header {
                sender: duml::addr(Device::Camera, CAMERA_INDEX),
                receiver: duml::addr(Device::App, 0),
                seq: 1,
                cmd_type: 0,
                cmd_set: CMD_SET,
                cmd_id: CMD_ID_REPORT,
            },
            payload,
        )
        .unwrap();
        let (f, _) = duml::parse(&frame).unwrap();
        Report::decode(&f).unwrap()
    }

    #[test]
    fn decodes_a_captured_report() {
        // Payload of a live 0xF2 report (100 bytes): spot 0.5/0.5, 21.1 °C,
        // flags 0x81, display-mode byte 0.
        let payload = hex(concat!(
            "01 00 02 01 64 00 18 03 14 00 03 00 0f 00 0f 00 10 00 00 00 ",
            "3f 00 00 00 3f cd cc a8 41 00 00 01 01 ff ff 20 00 26 00 01 ",
            "00 02 00 04 01 00 02 01 81 00 10 27 10 27 98 08 98 08 10 27 ",
            "98 08 00 00 98 08 88 13 88 13 88 13 88 13 cd cc a8 41 00 00 ",
            "00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01 0f 00 00 00 ",
        ));
        let frame = duml::build(
            &Header {
                sender: duml::addr(Device::Camera, CAMERA_INDEX),
                receiver: duml::addr(Device::App, 0),
                seq: 1,
                cmd_type: 0,
                cmd_set: CMD_SET,
                cmd_id: CMD_ID_REPORT,
            },
            &payload,
        )
        .unwrap();
        let (f, _) = duml::parse(&frame).unwrap();
        let r = Report::decode(&f).unwrap();
        assert_eq!((r.spot_x, r.spot_y), (0.5, 0.5));
        assert!((r.spot_temp - 21.1).abs() < 0.01);
        assert!(r.spot_valid);
        assert_eq!(r.display_mode, Some(0));
    }

    #[test]
    fn acks_are_matched_to_our_settings_only() {
        let build = |sender, receiver, cmd_type, cmd_id, payload: &[u8]| {
            let header = Header {
                sender,
                receiver,
                seq: 0x1234,
                cmd_type,
                cmd_set: CMD_SET,
                cmd_id,
            };
            duml::build(&header, payload).unwrap()
        };
        let cam = duml::addr(Device::Camera, CAMERA_INDEX);
        let app = duml::addr(Device::App, 0);
        let decode = |bytes: Vec<u8>| ack(&duml::parse(&bytes).unwrap().0);
        assert_eq!(
            decode(build(cam, app, 0xc0, CMD_ID_SET, &[0x00])),
            Some((0x1234, 0x00))
        );
        assert_eq!(
            decode(build(cam, app, 0xc0, CMD_ID_SET, &[0xff])),
            Some((0x1234, 0xff))
        );
        assert_eq!(decode(build(app, cam, 0x40, CMD_ID_SET, &[0x87])), None); // our request
        assert_eq!(decode(build(cam, app, 0x00, CMD_ID_REPORT, &[0x00])), None); // a report
        assert_eq!(decode(build(cam, app, 0xc0, CMD_ID_SET, &[])), None);
    }

    #[test]
    fn non_finite_readings_are_dropped() {
        let mut payload = hex(concat!(
            "01 00 02 01 64 00 18 03 14 00 03 00 0f 00 0f 00 10 00 00 00 ",
            "3f 00 00 00 3f 66 66 96 41 00 00 01 01 1b 00 b1 00 b7 02 02 ",
            "00 00 00 04 01 00 02 01 81 00 10 27 10 27 98 08 98 08 10 27 ",
            "98 08 00 00 98 08 7f 11 fc 10 10 20 2f 1e 66 66 96 41 9a 99 ",
            "89 41 9a 99 a5 41 a7 13 ef 10 59 13 22 1e 03 0a 0f 00 00 00 ",
        ));
        payload[25..29].copy_from_slice(&f32::NAN.to_le_bytes()); // spot temp
        payload[82..86].copy_from_slice(&f32::INFINITY.to_le_bytes()); // area max
        let r = report(&payload);
        assert!(!r.spot_valid && r.spot_temp == 0.0);
        assert_eq!(r.area, None);
        assert_eq!(r.isotherm_middle, 177); // the rest of the report survives
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
