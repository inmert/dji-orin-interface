//! Gimbal command injection: building the DUML frames the Orin sends *to* the
//! gimbals. This is the first and lowest-risk injection target (props off, it
//! only moves the camera mount).
//!
//! The move command was decoded from captured DJI Pilot traffic:
//!
//! * `cmd_set = 0x04` (Gimbal), `cmd_id = 0x0c`, sender App, receiver Gimbal.
//! * 7-byte payload, little-endian: `int16 yaw, int16 roll, int16 pitch, u8 flag`.
//!   (The yaw/pitch order was confirmed on hardware — the two fields are
//!   transposed from the first guess.) Roll is always 0 (the app does not drive
//!   it); `flag = 0x80` while a move is active, `0x00` to release/stop (a full
//!   release is an all-zero payload).
//!
//! The command-type byte `0x00` is verified on hardware; it is isolated in
//! [`cmd_type`] so it can still be overridden for experiments. The values Pilot
//! sent ramped smoothly while a stick was held, so whether they are a driven
//! setpoint or a speed is not yet characterized.

use super::{CMD_ID_LOCK, CMD_ID_LOCK_AUX, CMD_ID_MOVE, CMD_SET, CMD_SET_SYNC, Camera, SYNC_INDEX};
use crate::duml::{self, Device, Header};

/// Default command-type byte for gimbal commands (`0x00`, verified on
/// hardware). `GIMBAL_CMD_TYPE` overrides it at runtime (e.g.
/// `GIMBAL_CMD_TYPE=0x40`) so trying values needs no recompile.
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

/// Native recenter actions decoded from DJI's Onboard SDK. Wire encoding is
/// verified against its V1 packer. Yaw recenter is hardware-verified
/// for both independent cameras and the linked pair. Pitch-and-yaw is untested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recenter {
    /// Drone yaw return-to-center, matching the observed RC-button axes.
    Yaw = 1,
    /// Return both pitch and yaw to center.
    PitchAndYaw = 3,
}

/// Build a native reset request without changing the gimbal's work mode.
///
/// SDK-derived; yaw recenter is validated on this M210, including linked mode. See
/// `docs/protocol.md` for provenance and remaining validation. This is
/// deliberately not wired into the server injector yet.
pub fn recenter_frame(camera: Camera, action: Recenter, seq: u16) -> Vec<u8> {
    let header = Header {
        sender: duml::addr(Device::App, 0),
        receiver: camera.gimbal_addr(),
        seq,
        // Request a completion ACK (2 << 5), independently of move overrides.
        cmd_type: 0x40,
        cmd_set: CMD_SET,
        cmd_id: 0x4c,
    };
    duml::build(&header, &[0xfe, action as u8]).expect("recenter frame within length limit")
}

/// A gimbal move request: pitch and yaw in the units Pilot used (to be
/// calibrated). Roll is not driven. `active = false` builds a release/stop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GimbalMove {
    pub pitch: i16,
    pub yaw: i16,
    pub active: bool,
}

impl GimbalMove {
    /// A release/stop.
    pub const RELEASE: GimbalMove = GimbalMove {
        pitch: 0,
        yaw: 0,
        active: false,
    };

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
pub fn move_frame(camera: Camera, mv: GimbalMove, seq: u16) -> Vec<u8> {
    let header = Header {
        sender: duml::addr(Device::App, 0),
        receiver: camera.gimbal_addr(),
        seq,
        cmd_type: cmd_type(),
        cmd_set: CMD_SET,
        cmd_id: CMD_ID_MOVE,
    };
    // A 7-byte payload is always within the frame-length limit, so build cannot
    // fail here; surface a panic rather than silently sending a truncated frame.
    duml::build(&header, &mv.payload()).expect("gimbal frame within length limit")
}

fn sync_header(seq: u16, cmd_id: u8) -> Header {
    Header {
        sender: duml::addr(Device::App, 0),
        receiver: duml::addr(Device::Gimbal, SYNC_INDEX),
        seq,
        cmd_type: cmd_type(),
        cmd_set: CMD_SET_SYNC,
        cmd_id,
    }
}

/// The camera-lock command (dual-gimbal follow): `on` engages the lock so both
/// cameras move together driven by camera 1, `off` disengages. Payload byte 0 is
/// the enable flag (captured: `01 00` = on, `00 00` = off).
pub fn lock_frame(on: bool, seq: u16) -> Vec<u8> {
    duml::build(&sync_header(seq, CMD_ID_LOCK), &[u8::from(on), 0])
        .expect("lock frame within length limit")
}

/// The companion command Pilot sends alongside disengaging the lock
/// (`cmd_id 0x05`, payload `00`).
pub fn lock_release_aux_frame(seq: u16) -> Vec<u8> {
    duml::build(&sync_header(seq, CMD_ID_LOCK_AUX), &[0])
        .expect("lock-aux frame within length limit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recenter_matches_independent_dji_sdk_packer() {
        // Generated by DJI OSDK's OsdkProtocol_v1Pack, not our frame builder.
        // SDK revision and reproduction details are recorded in docs/protocol.md.
        let cases = [
            (Camera::One, Recenter::Yaw, [0x04, 0x01, 0x4d, 0x9d]),
            (Camera::One, Recenter::PitchAndYaw, [0x04, 0x03, 0x5f, 0xbe]),
            (Camera::Two, Recenter::Yaw, [0x44, 0x01, 0xbc, 0xf8]),
            (Camera::Two, Recenter::PitchAndYaw, [0x44, 0x03, 0xae, 0xdb]),
        ];
        for (camera, action, [receiver, mode, crc_lo, crc_hi]) in cases {
            let expected = [
                0x55, 0x0f, 0x04, 0xa2, 0x02, receiver, 0x34, 0x12, 0x40, 0x04, 0x4c, 0xfe, mode,
                crc_lo, crc_hi,
            ];
            let actual = recenter_frame(camera, action, 0x1234);
            assert_eq!(actual, expected);
            let (frame, consumed) = duml::parse(&actual).unwrap();
            assert_eq!(consumed, actual.len());
            assert!(frame.hdr_crc_ok && frame.crc_ok);
        }
    }

    #[test]
    fn release_payload_is_all_zero() {
        let mv = GimbalMove {
            pitch: 500,
            yaw: -300,
            active: false,
        };
        assert_eq!(mv.payload(), [0u8; 7]);
        assert_eq!(GimbalMove::RELEASE.payload(), [0u8; 7]);
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
        let on = lock_frame(true, 1);
        let (f, n) = duml::parse(&on).unwrap();
        assert_eq!(n, on.len());
        assert!(f.hdr_crc_ok && f.crc_ok);
        assert_eq!(f.receiver, 0xe4); // gimbal type, index 7
        assert_eq!(f.cmd_set, CMD_SET_SYNC);
        assert_eq!(f.cmd_id, CMD_ID_LOCK);
        assert_eq!(f.payload, &[0x01, 0x00]);

        let off = lock_frame(false, 2);
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
        let frame = move_frame(Camera::Two, mv, 0x1234);
        let (f, n) = duml::parse(&frame).expect("builds a parseable frame");
        assert_eq!(n, frame.len());
        assert!(f.hdr_crc_ok && f.crc_ok);
        assert_eq!(f.sender, 0x02);
        assert_eq!(f.receiver, 0x44);
        assert_eq!(f.seq, 0x1234);
        assert_eq!(f.cmd_set, CMD_SET);
        assert_eq!(f.cmd_id, CMD_ID_MOVE);
        assert_eq!(f.payload, &mv.payload());
    }
}
