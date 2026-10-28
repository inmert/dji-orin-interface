//! The flight-data broadcast (`cmd_set 0x02`, `cmd_id 0x00`) the flight
//! controller pushes on the OSDK UART without activation or acks.
//!
//! Payload = `u16` flag word, then each flagged section in a fixed order
//! (Onboard SDK `DataBroadcast::unpackData`; structs are packed,
//! little-endian). See [`SECTIONS`] for sizes.
//!
//! Verified on our M210 V1. On 2026-10-27 it sent a 17-byte time-only frame
//! (~50 Hz) and a 267-byte frame (~10 Hz) whose flag word read **0** but whose
//! body was exactly sections `0x0001..=0x0800`. On 2026-10-28 it sent real
//! flag words instead: `0x00FF` and `0x2FFF` (adds status, battery, SDK info).
//! A zero flag is resolved by matching the body length against the section
//! order ([`resolve_flags`]); a body that matches no layout is rejected.

use std::f32::consts::PI;

/// Broadcast command set/id.
pub const CMD_SET: u8 = 0x02;
pub const CMD_ID: u8 = 0x00;

pub const TIME: u16 = 0x0001;
pub const QUATERNION: u16 = 0x0002;
pub const ACCELERATION: u16 = 0x0004;
pub const VELOCITY: u16 = 0x0008;
pub const ANGULAR_RATE: u16 = 0x0010;
pub const POSITION: u16 = 0x0020;
pub const GPS: u16 = 0x0040;
pub const RTK: u16 = 0x0080;
pub const MAG: u16 = 0x0100;
pub const RC: u16 = 0x0200;
pub const GIMBAL: u16 = 0x0400;
pub const STATUS: u16 = 0x0800;
pub const BATTERY: u16 = 0x1000;
pub const DEVICE: u16 = 0x2000;
pub const COMPASS: u16 = 0x4000;

/// Section flag and byte size, in wire order.
pub const SECTIONS: [(u16, usize); 15] = [
    (TIME, 8 + 7),       // TimeStamp + SyncStamp
    (QUATERNION, 16),    // f32 w, x, y, z
    (ACCELERATION, 12),  // f32 x, y, z
    (VELOCITY, 12 + 1),  // f32 x, y, z + VelocityInfo
    (ANGULAR_RATE, 12),  // f32 x, y, z
    (POSITION, 25 + 25), // GlobalPosition + RelativePosition
    (GPS, 68),           // GPSInfo
    (RTK, 44),           // RTK
    (MAG, 6),            // i16 x, y, z
    (RC, 12),            // i16 × 6
    (GIMBAL, 13),        // f32 roll, pitch, yaw + limit bits
    (STATUS, 4),         // flight, mode, gear, error
    (BATTERY, 13),       // capacity, voltage, current, percent
    (DEVICE, 2),         // SDKInfo
    (COMPASS, 1),        // compass status
];

/// Byte count of the sections selected by `flags`.
pub fn body_len(flags: u16) -> usize {
    SECTIONS
        .iter()
        .filter(|(f, _)| flags & f != 0)
        .map(|(_, n)| n)
        .sum()
}

/// The flags that describe a body of `len` bytes. A non-zero flag word is
/// trusted only if its sections add up to `len`; a zero word (what the M210 V1
/// sends) is matched against each prefix of the section order.
pub fn resolve_flags(word: u16, len: usize) -> Option<u16> {
    if word != 0 {
        return (body_len(word) == len).then_some(word);
    }
    let mut flags = 0;
    let mut total = 0;
    for (f, n) in SECTIONS {
        if total == len {
            break;
        }
        flags |= f;
        total += n;
    }
    (total == len && len > 0).then_some(flags)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Euler {
    /// Degrees.
    pub roll: f32,
    pub pitch: f32,
    pub yaw: f32,
}

impl Euler {
    /// Roll/pitch/yaw (ZYX) from the FC's attitude quaternion (w, x, y, z),
    /// the conversion the Onboard SDK samples use.
    pub fn from_quaternion([w, x, y, z]: [f32; 4]) -> Euler {
        let deg = 180.0 / PI;
        Euler {
            roll: (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y)) * deg,
            pitch: (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin() * deg,
            yaw: (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)) * deg,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Velocity {
    /// m/s, frame as configured in DJI Assistant 2 (ground NEU by default).
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// The FC is using GPS for this estimate.
    pub gps: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Position {
    /// Fused position, degrees (0/0 without a fix).
    pub latitude: f64,
    pub longitude: f64,
    /// Barometric altitude, m.
    pub altitude: f32,
    /// Height above the takeoff point / ground, m.
    pub height: f32,
    /// GPS health 0–5 (above 3 is strong).
    pub health: u8,
}

/// Obstacle distances from the vision/ultrasonic sensors, m.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Obstacles {
    pub down: f32,
    pub front: f32,
    pub right: f32,
    pub back: f32,
    pub left: f32,
    pub up: f32,
    /// Bit per sensor (down, front, right, back, left, up): 1 = working.
    pub health: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Gps {
    /// yyyymmdd and hhmmss (observed as UTC on the M210 V1).
    pub date: u32,
    pub time: u32,
    /// Degrees. Hold the last value seen without a fix, so check `satellites`.
    pub latitude: f64,
    pub longitude: f64,
    /// Height above mean sea level, m.
    pub hfsl: f32,
    /// NED velocity, m/s.
    pub velocity_ned: [f32; 3],
    pub hdop: f32,
    pub pdop: f32,
    pub fix: f32,
    /// Satellites used: GPS + GLONASS.
    pub satellites: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Rc {
    /// Each ±10000.
    pub roll: i16,
    pub pitch: i16,
    pub yaw: i16,
    pub throttle: i16,
    pub mode: i16,
    pub gear: i16,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Gimbal {
    /// Degrees.
    pub roll: f32,
    pub pitch: f32,
    pub yaw: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Status {
    pub flight: u8,
    pub flight_name: &'static str,
    /// Display (flight) mode.
    pub mode: u8,
    pub mode_name: &'static str,
    pub gear: u8,
    pub gear_name: &'static str,
    pub error: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Battery {
    /// mAh, mV, mA, %.
    pub capacity: u32,
    pub voltage: i32,
    pub current: i32,
    pub percent: u8,
}

/// Latest value of every broadcast section seen so far. Sections absent from a
/// frame keep their previous value (the time-only frames carry nothing else).
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FlightData {
    /// FC clock, ms.
    pub time_ms: Option<u32>,
    pub attitude: Option<Euler>,
    pub quaternion: Option<[f32; 4]>,
    pub acceleration: Option<[f32; 3]>,
    pub velocity: Option<Velocity>,
    /// rad/s.
    pub angular_rate: Option<[f32; 3]>,
    pub position: Option<Position>,
    pub obstacles: Option<Obstacles>,
    pub gps: Option<Gps>,
    pub mag: Option<[i16; 3]>,
    pub rc: Option<Rc>,
    pub gimbal: Option<Gimbal>,
    pub status: Option<Status>,
    pub battery: Option<Battery>,
    /// 0 normal, 1 needs calibration, 2 needs direction fix, 3 needs restart.
    pub compass: Option<u8>,
    /// Which sections the last frame carried.
    pub last_flags: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Shorter than the flag word.
    Short,
    /// The body length matches no section layout.
    Layout { word: u16, len: usize },
}

/// Little-endian cursor over a section body (lengths are checked up front).
struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        head.try_into().unwrap()
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn i16(&mut self) -> i16 {
        i16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.take())
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
    fn f64(&mut self) -> f64 {
        f64::from_le_bytes(self.take())
    }
    fn f32x3(&mut self) -> [f32; 3] {
        [self.f32(), self.f32(), self.f32()]
    }
    fn skip(&mut self, n: usize) {
        self.0 = &self.0[n..];
    }
}

pub fn flight_name(v: u8) -> &'static str {
    match v {
        0 => "Stopped",
        1 => "On ground",
        2 => "In air",
        _ => "Unknown",
    }
}

pub fn mode_name(v: u8) -> &'static str {
    match v {
        0 => "Manual",
        1 => "Attitude",
        6 => "P-GPS",
        9 => "Hotpoint",
        10 => "Assisted takeoff",
        11 => "Auto takeoff",
        12 => "Auto landing",
        15 => "Go home",
        17 => "SDK control",
        33 => "Forced landing",
        40 => "Search",
        41 => "Engine start",
        _ => "Unknown",
    }
}

pub fn gear_name(v: u8) -> &'static str {
    match v {
        0 => "Undefined",
        1 => "Down",
        2 => "Lowering",
        3 => "Up",
        4 => "Raising",
        5 => "Hold",
        6 => "Packed",
        7 => "Packing",
        8 => "Unpacking",
        _ => "Unknown",
    }
}

impl FlightData {
    /// Fold one broadcast payload in. On error nothing is changed.
    pub fn apply(&mut self, payload: &[u8]) -> Result<u16, Error> {
        let (word, body) = match payload {
            [a, b, body @ ..] => (u16::from_le_bytes([*a, *b]), body),
            _ => return Err(Error::Short),
        };
        let flags = resolve_flags(word, body.len()).ok_or(Error::Layout {
            word,
            len: body.len(),
        })?;
        let mut r = Rd(body);
        for (flag, size) in SECTIONS {
            if flags & flag == 0 {
                continue;
            }
            match flag {
                TIME => {
                    self.time_ms = Some(r.u32());
                    r.skip(size - 4);
                }
                QUATERNION => {
                    let q = [r.f32(), r.f32(), r.f32(), r.f32()];
                    self.quaternion = Some(q);
                    self.attitude = Some(Euler::from_quaternion(q));
                }
                ACCELERATION => self.acceleration = Some(r.f32x3()),
                VELOCITY => {
                    let [x, y, z] = r.f32x3();
                    let gps = r.u8() & 1 != 0;
                    self.velocity = Some(Velocity { x, y, z, gps });
                }
                ANGULAR_RATE => self.angular_rate = Some(r.f32x3()),
                POSITION => {
                    let (lat, lon) = (r.f64(), r.f64());
                    self.position = Some(Position {
                        latitude: lat.to_degrees(),
                        longitude: lon.to_degrees(),
                        altitude: r.f32(),
                        height: r.f32(),
                        health: r.u8(),
                    });
                    self.obstacles = Some(Obstacles {
                        down: r.f32(),
                        front: r.f32(),
                        right: r.f32(),
                        back: r.f32(),
                        left: r.f32(),
                        up: r.f32(),
                        health: r.u8() & 0x3F,
                    });
                }
                GPS => {
                    let (date, time) = (r.u32(), r.u32());
                    let lon = f64::from(r.i32()) / 1e7;
                    let lat = f64::from(r.i32()) / 1e7;
                    let hfsl = r.i32() as f32 / 1000.0;
                    let v = r.f32x3().map(|c| c / 100.0);
                    let (hdop, pdop, fix) = (r.f32(), r.f32(), r.f32());
                    r.skip(3 * 4); // gnssStatus, hacc, sacc
                    let satellites = r.u32().saturating_add(r.u32());
                    r.skip(2 + 2); // NSV, GPS counter
                    self.gps = Some(Gps {
                        date,
                        time,
                        latitude: lat,
                        longitude: lon,
                        hfsl,
                        velocity_ned: v,
                        hdop,
                        pdop,
                        fix,
                        satellites,
                    });
                }
                MAG => self.mag = Some([r.i16(), r.i16(), r.i16()]),
                RC => {
                    self.rc = Some(Rc {
                        roll: r.i16(),
                        pitch: r.i16(),
                        yaw: r.i16(),
                        throttle: r.i16(),
                        mode: r.i16(),
                        gear: r.i16(),
                    });
                }
                GIMBAL => {
                    let [roll, pitch, yaw] = r.f32x3();
                    r.skip(1);
                    self.gimbal = Some(Gimbal { roll, pitch, yaw });
                }
                STATUS => {
                    let (flight, mode, gear, error) = (r.u8(), r.u8(), r.u8(), r.u8());
                    self.status = Some(Status {
                        flight,
                        flight_name: flight_name(flight),
                        mode,
                        mode_name: mode_name(mode),
                        gear,
                        gear_name: gear_name(gear),
                        error,
                    });
                }
                BATTERY => {
                    self.battery = Some(Battery {
                        capacity: r.u32(),
                        voltage: r.i32(),
                        current: r.i32(),
                        percent: r.u8(),
                    });
                }
                COMPASS => self.compass = Some(r.u8()),
                // RTK and SDK info: not shown yet.
                _ => r.skip(size),
            }
        }
        self.last_flags = flags;
        Ok(flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic 267-byte payload shaped like the M210 V1's full broadcast:
    /// flag word 0, sections TIME..=STATUS.
    fn full_payload() -> Vec<u8> {
        let mut p = vec![0, 0];
        let f = |p: &mut Vec<u8>, v: f32| p.extend_from_slice(&v.to_le_bytes());
        p.extend_from_slice(&532_800u32.to_le_bytes());
        p.extend_from_slice(&[0; 4 + 7]);
        // Yaw −90°: w = cos(−45°), z = sin(−45°).
        let h = std::f32::consts::FRAC_1_SQRT_2;
        for v in [h, 0.0, 0.0, -h] {
            f(&mut p, v);
        }
        p.extend_from_slice(&[0; 12]); // acceleration
        for v in [1.0, 2.0, -0.5] {
            f(&mut p, v);
        }
        p.push(1); // velocity info: GPS
        p.extend_from_slice(&[0; 12]); // angular rate
        p.extend_from_slice(&0.5f64.to_le_bytes()); // lat rad
        p.extend_from_slice(&(-1.0f64).to_le_bytes()); // lon rad
        f(&mut p, 167.6);
        f(&mut p, 2.5);
        p.push(4);
        for v in [0.13, 0.9, 100.0, 100.0, 100.0, 1.86] {
            f(&mut p, v);
        }
        p.push(0x20);
        p.extend_from_slice(&20_261_027u32.to_le_bytes());
        p.extend_from_slice(&224_425u32.to_le_bytes());
        p.extend_from_slice(&(-123_456_789i32).to_le_bytes()); // lon
        p.extend_from_slice(&456_789_012i32.to_le_bytes()); // lat
        p.extend_from_slice(&270_608i32.to_le_bytes()); // mm
        for v in [100.0, -50.0, 0.0, 1.2, 2.3, 3.0, 0.0, 0.0, 0.0] {
            f(&mut p, v); // velNED cm/s, hdop, pdop, fix, gnss, hacc, sacc
        }
        p.extend_from_slice(&9u32.to_le_bytes());
        p.extend_from_slice(&3u32.to_le_bytes());
        p.extend_from_slice(&[0; 4]);
        p.extend_from_slice(&[0; 44]); // RTK
        for v in [101i16, 582, 1366] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0i16, 10_000, 0, -10_000, 0, 0] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0.0, -30.0, -97.4] {
            f(&mut p, v);
        }
        p.push(0);
        p.extend_from_slice(&[0, 6, 1, 0]);
        p
    }

    #[test]
    fn layout_sizes_match_the_observed_frames() {
        assert_eq!(body_len(TIME), 15); // the 17-byte frames
        assert_eq!(body_len(0x0FFF), 265); // the 267-byte frames
        assert_eq!(full_payload().len(), 267);
    }

    #[test]
    fn zero_flag_word_is_resolved_by_length() {
        assert_eq!(resolve_flags(0, 265), Some(0x0FFF));
        assert_eq!(resolve_flags(0, 15), Some(TIME));
        assert_eq!(resolve_flags(0, 14), None);
        assert_eq!(resolve_flags(0, 0), None);
        assert_eq!(resolve_flags(TIME, 15), Some(TIME));
        assert_eq!(resolve_flags(TIME, 16), None); // lying flag word
    }

    #[test]
    fn decodes_the_full_broadcast() {
        let mut d = FlightData::default();
        assert_eq!(d.apply(&full_payload()), Ok(0x0FFF));
        assert_eq!(d.time_ms, Some(532_800));
        let a = d.attitude.unwrap();
        assert!(a.roll.abs() < 0.01 && a.pitch.abs() < 0.01);
        assert!((a.yaw + 90.0).abs() < 0.01);
        let v = d.velocity.unwrap();
        assert_eq!((v.x, v.y, v.z, v.gps), (1.0, 2.0, -0.5, true));
        let p = d.position.unwrap();
        assert!((p.latitude - 0.5f64.to_degrees()).abs() < 1e-9);
        assert_eq!((p.altitude, p.height, p.health), (167.6, 2.5, 4));
        let o = d.obstacles.unwrap();
        assert_eq!((o.down, o.up, o.health), (0.13, 1.86, 0x20));
        let g = d.gps.unwrap();
        assert_eq!((g.date, g.time, g.satellites), (20_261_027, 224_425, 12));
        assert!((g.longitude + 12.345_678_9).abs() < 1e-9);
        assert!((g.latitude - 45.678_901_2).abs() < 1e-9);
        assert_eq!(g.hfsl, 270.608);
        assert_eq!(g.velocity_ned, [1.0, -0.5, 0.0]);
        assert_eq!(d.mag, Some([101, 582, 1366]));
        assert_eq!(d.rc.as_ref().unwrap().throttle, -10_000);
        assert_eq!(d.gimbal.as_ref().unwrap().yaw, -97.4);
        let s = d.status.unwrap();
        assert_eq!(
            (s.flight_name, s.mode_name, s.gear_name),
            ("Stopped", "P-GPS", "Down")
        );
        assert_eq!(d.battery, None);
    }

    #[test]
    fn time_only_frames_keep_the_other_sections() {
        let mut d = FlightData::default();
        d.apply(&full_payload()).unwrap();
        let mut t = vec![1, 0];
        t.extend_from_slice(&600_000u32.to_le_bytes());
        t.extend_from_slice(&[0; 11]);
        assert_eq!(d.apply(&t), Ok(TIME));
        assert_eq!(d.time_ms, Some(600_000));
        assert!(d.attitude.is_some() && d.status.is_some());
        assert_eq!(d.last_flags, TIME);
    }

    #[test]
    fn malformed_payloads_change_nothing() {
        let mut d = FlightData::default();
        assert_eq!(d.apply(&[1]), Err(Error::Short));
        let mut bad = full_payload();
        bad.pop();
        assert!(matches!(d.apply(&bad), Err(Error::Layout { len: 264, .. })));
        assert_eq!(d, FlightData::default());
    }
}
