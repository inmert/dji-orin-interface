//! Command injection: building DUML frames the Orin sends *to* the aircraft.
//!
//! This is the active counterpart to the passive [`duml`](crate::duml) decoder.
//! For now it builds the gimbal move command only; that is the first and
//! lowest-risk injection target (props off, it only moves the camera mount).
//!
//! The gimbal move command was decoded from captured DJI Pilot traffic:
//!
//! * `cmd_set = 0x04` (Gimbal), `cmd_id = 0x0c`, sender App, receiver Gimbal.
//! * 7-byte payload, little-endian: `int16 yaw, int16 roll, int16 pitch, u8 flag`.
//!   (The yaw/pitch order was confirmed on hardware — the two fields are
//!   transposed from the first guess.) Roll is always 0 (the app does not drive
//!   it); `flag = 0x80` while a move is active, `0x00` to release/stop (a full
//!   release is an all-zero payload).
//! * The two cameras are the gimbal **device index**: camera 1 = index 0,
//!   camera 2 = index 2.
//!
//! The values Pilot sent ramped smoothly while a stick was held, so the exact
//! semantics (driven setpoint vs. speed) and the [`CMD_TYPE`] byte are to be
//! confirmed against a fresh capture on the first hardware injection test;
//! both are isolated here so they are a one-line change.

use crate::duml::{self, Device, Header};

/// Gimbal command set.
pub const CMD_SET_GIMBAL: u8 = 0x04;
/// Gimbal move command id (pitch/roll/yaw + flag).
pub const CMD_ID_GIMBAL_MOVE: u8 = 0x0c;

/// Dual-gimbal sync command set and the camera-lock ids (captured from Pilot's
/// "lock cameras" button). The lock is addressed to the gimbal sync index 7.
pub const CMD_SET_GIMBAL_SYNC: u8 = 0x19;
pub const CMD_ID_LOCK: u8 = 0x07;
pub const CMD_ID_LOCK_AUX: u8 = 0x05;
const GIMBAL_SYNC_INDEX: u8 = 7;

/// Default command-type byte for the move command. Provisional: the captured
/// value was not recorded (the log showed set/id/payload only). The first
/// hardware injection test calibrates it; `GIMBAL_CMD_TYPE` overrides it at
/// runtime (e.g. `GIMBAL_CMD_TYPE=0x40`) so trying values needs no recompile.
pub const CMD_TYPE_DEFAULT: u8 = 0x00;

/// The command-type byte to use, read once from `GIMBAL_CMD_TYPE` (hex, with or
/// without `0x`) or [`CMD_TYPE_DEFAULT`].
pub fn cmd_type() -> u8 {
    use std::sync::OnceLock;
    static CT: OnceLock<u8> = OnceLock::new();
    *CT.get_or_init(|| {
        std::env::var("GIMBAL_CMD_TYPE")
            .ok()
            .and_then(|s| u8::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(CMD_TYPE_DEFAULT)
    })
}

/// Payload flag: a move is active.
const FLAG_ACTIVE: u8 = 0x80;

/// Encode a sender/receiver byte from a device type and index (the inverse of
/// the decode in [`duml::DevName`]): low 5 bits type, high 3 bits index.
pub fn addr(device: Device, index: u8) -> u8 {
    (device as u8 & 0x1F) | (index << 5)
}

/// The two gimbals, identified by their DUML device index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Camera {
    /// Camera 1 — gimbal device index 0.
    One = 0,
    /// Camera 2 — gimbal device index 2.
    Two = 2,
}

impl Camera {
    /// The gimbal receiver byte for this camera.
    pub fn gimbal_addr(self) -> u8 {
        addr(Device::Gimbal, self as u8)
    }

    /// Map a UI camera number (1 or 2) to a [`Camera`].
    pub fn from_number(n: u8) -> Option<Camera> {
        match n {
            1 => Some(Camera::One),
            2 => Some(Camera::Two),
            _ => None,
        }
    }
}

/// A gimbal move request: pitch and yaw in the units Pilot used (to be
/// calibrated). Roll is not driven. `active = false` builds a release/stop.
#[derive(Debug, Clone, Copy, Default)]
pub struct GimbalMove {
    pub pitch: i16,
    pub yaw: i16,
    pub active: bool,
}

impl GimbalMove {
    /// The 7-byte DUML payload for this move. Wire layout, confirmed on
    /// hardware: yaw/pan at `[0:2]`, roll at `[2:4]` (0), pitch/tilt at `[4:6]`.
    pub fn payload(self) -> [u8; 7] {
        if !self.active {
            return [0; 7]; // full release
        }
        let p = self.pitch.to_le_bytes();
        let y = self.yaw.to_le_bytes();
        [y[0], y[1], 0, 0, p[0], p[1], FLAG_ACTIVE]
    }
}

/// Build a complete gimbal move frame for `camera`, with sequence `seq`.
pub fn gimbal_move_frame(camera: Camera, mv: GimbalMove, seq: u16) -> Vec<u8> {
    let header = Header {
        sender: addr(Device::App, 0),
        receiver: camera.gimbal_addr(),
        seq,
        cmd_type: cmd_type(),
        cmd_set: CMD_SET_GIMBAL,
        cmd_id: CMD_ID_GIMBAL_MOVE,
    };
    // A 7-byte payload is always within the frame-length limit, so build cannot
    // fail here; surface a panic rather than silently sending a truncated frame.
    duml::build(&header, &mv.payload()).expect("gimbal frame within length limit")
}

fn sync_header(seq: u16, cmd_id: u8) -> Header {
    Header {
        sender: addr(Device::App, 0),
        receiver: addr(Device::Gimbal, GIMBAL_SYNC_INDEX),
        seq,
        cmd_type: cmd_type(),
        cmd_set: CMD_SET_GIMBAL_SYNC,
        cmd_id,
    }
}

/// The camera-lock command (dual-gimbal follow): `on` engages the lock so both
/// cameras move together driven by camera 1, `off` disengages. Payload byte 0 is
/// the enable flag (captured: `01 00` = on, `00 00` = off).
pub fn gimbal_lock_frame(on: bool, seq: u16) -> Vec<u8> {
    duml::build(&sync_header(seq, CMD_ID_LOCK), &[u8::from(on), 0])
        .expect("lock frame within length limit")
}

/// The companion command Pilot sends alongside disengaging the lock
/// (`cmd_id 0x05`, payload `00`).
pub fn gimbal_lock_release_aux_frame(seq: u16) -> Vec<u8> {
    duml::build(&sync_header(seq, CMD_ID_LOCK_AUX), &[0])
        .expect("lock-aux frame within length limit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_addresses() {
        assert_eq!(Camera::One.gimbal_addr(), 0x04); // index 0, type gimbal
        assert_eq!(Camera::Two.gimbal_addr(), 0x44); // index 2, type gimbal
    }

    #[test]
    fn release_payload_is_all_zero() {
        let mv = GimbalMove {
            pitch: 500,
            yaw: -300,
            active: false,
        };
        assert_eq!(mv.payload(), [0u8; 7]);
    }

    #[test]
    fn active_payload_layout() {
        // Wire layout: yaw at [0:2], roll at [2:4], pitch at [4:6], flag at [6].
        // yaw = 0xfed4 (-300), pitch = 0x01f4 (500), active flag, roll zeroed.
        let mv = GimbalMove {
            pitch: 500,
            yaw: -300,
            active: true,
        };
        assert_eq!(mv.payload(), [0xd4, 0xfe, 0x00, 0x00, 0xf4, 0x01, 0x80]);
    }

    #[test]
    fn lock_frame_matches_capture() {
        // Captured: lock-on = 0x19/0x07 "01 00" to Gimbal[7] (receiver 0xe4).
        let on = gimbal_lock_frame(true, 1);
        let (f, n) = duml::parse(&on).unwrap();
        assert_eq!(n, on.len());
        assert!(f.hdr_crc_ok && f.crc_ok);
        assert_eq!(f.receiver, 0xe4); // gimbal type, index 7
        assert_eq!(f.cmd_set, CMD_SET_GIMBAL_SYNC);
        assert_eq!(f.cmd_id, CMD_ID_LOCK);
        assert_eq!(f.payload, &[0x01, 0x00]);

        let off = gimbal_lock_frame(false, 2);
        let (f, _) = duml::parse(&off).unwrap();
        assert_eq!(f.payload, &[0x00, 0x00]);
    }

    #[test]
    fn frame_round_trips_through_the_decoder() {
        let mv = GimbalMove {
            pitch: 123,
            yaw: -45,
            active: true,
        };
        let frame = gimbal_move_frame(Camera::Two, mv, 0x1234);
        let (f, n) = duml::parse(&frame).expect("builds a parseable frame");
        assert_eq!(n, frame.len());
        assert!(f.hdr_crc_ok && f.crc_ok);
        assert_eq!(f.sender, 0x02);
        assert_eq!(f.receiver, 0x44);
        assert_eq!(f.seq, 0x1234);
        assert_eq!(f.cmd_set, CMD_SET_GIMBAL);
        assert_eq!(f.cmd_id, CMD_ID_GIMBAL_MOVE);
        assert_eq!(f.payload, &mv.payload());
    }
}
