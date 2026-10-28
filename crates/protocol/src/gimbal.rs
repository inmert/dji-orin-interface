//! Gimbal (camera mount) feature: the commands the Orin injects to move the
//! two cameras, and the attitude reports the gimbals push back up.
//!
//! * [`command`] — building the move and camera-lock frames we send.
//! * [`attitude`] — decoding the attitude reports for telemetry.
//!
//! The M210 carries two gimbals, addressed by their DUML device **index**:
//! camera 1 = index 0, camera 2 = index 2 (see [`Camera`]). Command ids were
//! decoded from captured DJI Pilot traffic; see `docs/protocol.md`.

use crate::duml::{self, Device};

pub mod attitude;
pub mod command;

pub use attitude::Attitude;
pub use command::{
    GimbalMove, Recenter, lock_frame, lock_release_aux_frame, move_frame, recenter_frame,
};

/// Gimbal command set.
pub const CMD_SET: u8 = 0x04;
/// Attitude report the gimbals push up (pitch/roll/yaw, provisional decode).
pub const CMD_ID_ATTITUDE: u8 = 0x05;
/// Move command (yaw/roll/pitch + flag).
pub const CMD_ID_MOVE: u8 = 0x0c;

/// Dual-gimbal sync command set and the camera-lock ids (captured from Pilot's
/// "lock cameras" button). The lock is addressed to the gimbal sync index 7.
pub const CMD_SET_SYNC: u8 = 0x19;
pub const CMD_ID_LOCK: u8 = 0x07;
pub const CMD_ID_LOCK_AUX: u8 = 0x05;
const SYNC_INDEX: u8 = 7;

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
        duml::addr(Device::Gimbal, self as u8)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_addresses() {
        assert_eq!(Camera::One.gimbal_addr(), 0x04); // index 0, type gimbal
        assert_eq!(Camera::Two.gimbal_addr(), 0x44); // index 2, type gimbal
    }
}
