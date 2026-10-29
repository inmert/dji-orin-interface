//! Flight controller feature: the reports the FC pushes up over DUML.
//!
//! * [`osd`] — the `0x03/0x43` general-data report (attitude, flight mode,
//!   satellites), sent once a second on the aircraft's USB API port.
//!
//! * [`function_control_frame`] — `0x03/0x2a`, which starts and stops the
//!   motors from the app side of the RC link.
//! * [`usb_flight_frame`] — allow arming while the USB
//!   API port is connected (Onboard SDK `setUSBFlightOn`). The FC accepts it
//!   only while an Onboard SDK app holds control authority, and forgets it on
//!   restart (see `docs/protocol.md`, "Motor start and the USB flight lock").

pub mod osd;

pub use osd::Osd;

use crate::duml::{self, Device};

/// Flight controller command set.
pub const CMD_SET: u8 = 0x03;
/// General ("OSD") data report.
pub const CMD_ID_OSD: u8 = 0x43;
/// "FlyC Power Param" push (dji-firmware-tools), FC → app ~10 Hz on the RC
/// link: four f32, the first the average motor speed in rpm (0 with motors
/// off, ~1540 at idle, ~2030 spinning up; verified 2026-10-28).
pub const CMD_ID_POWER: u8 = 0x67;

/// Average motor speed (rpm) from a `0x03/0x67` payload.
pub fn motor_rpm(payload: &[u8]) -> Option<f32> {
    let b: [u8; 4] = payload.get(..4)?.try_into().ok()?;
    let v = f32::from_le_bytes(b);
    v.is_finite().then_some(v)
}

/// "Function Control" (dji-firmware-tools): one command byte for the FC.
pub const CMD_ID_FUNCTION_CONTROL: u8 = 0x2A;
/// Command codes, as in the FC's own "latest app command" list in `0x03/0x43`
/// (the same numbers as the OSDK's motor task). Start/stop verified on the
/// M210 V1 over the RC link (2026-10-28): ACK `00`, motors on/off in ~0.1 s.
pub const FUNCTION_START_MOTOR: u8 = 0x07;
pub const FUNCTION_STOP_MOTOR: u8 = 0x08;

/// `0x03/0x2a` from the app (over the RC link) to the FC with one function
/// code, e.g. [`FUNCTION_START_MOTOR`]. Needs no activation or control
/// authority. **Spins the motors.**
pub fn function_control_frame(code: u8, seq: u16) -> Vec<u8> {
    let h = duml::Header {
        sender: duml::addr(Device::App, 0),
        receiver: duml::addr(Device::FlightCtrl, 0),
        seq,
        cmd_type: 0x40,
        cmd_set: CMD_SET,
        cmd_id: CMD_ID_FUNCTION_CONTROL,
    };
    duml::build(&h, &[code]).expect("1-byte payload fits")
}

/// "USB-connected flight" setting (id from the Onboard SDK's linker library).
pub const CMD_ID_USB_FLIGHT: u8 = 0x1B;

/// `0x03/0x1b` from the PC on the USB API port to the FC: `01 81` (the SDK's
/// version word `0x8101`), then 1 = allow arming with USB connected, 0 = not.
/// The FC ACKs `00` on success, `02` without control authority.
pub fn usb_flight_frame(seq: u16, allow: bool) -> Vec<u8> {
    let h = duml::Header {
        sender: duml::addr(Device::Pc, 0),
        receiver: duml::addr(Device::FlightCtrl, 0),
        seq,
        cmd_type: 0x40,
        cmd_set: CMD_SET,
        cmd_id: CMD_ID_USB_FLIGHT,
    };
    duml::build(&h, &[0x01, 0x81, u8::from(allow)]).expect("3-byte payload fits")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_motor_rpm() {
        // Captured idle push: 1536.25 rpm, 0.071, 0, 99.86.
        let p = [
            0x00, 0x08, 0xc0, 0x44, 0x91, 0x6a, 0x91, 0x3d, 0, 0, 0, 0, 0x5c, 0xb8, 0xc7, 0x42,
        ];
        assert_eq!(motor_rpm(&p), Some(1536.25));
        assert_eq!(motor_rpm(&p[..3]), None);
    }

    #[test]
    fn function_control_frame_routes_app_to_fc() {
        let f = function_control_frame(FUNCTION_STOP_MOTOR, 1);
        let (h, n) = duml::parse(&f).unwrap();
        assert_eq!(n, f.len());
        assert_eq!((h.sender, h.receiver, h.cmd_type), (0x02, 0x03, 0x40));
        assert_eq!((h.cmd_set, h.cmd_id, h.payload), (0x03, 0x2A, &[0x08][..]));
    }

    #[test]
    fn usb_flight_frame_matches_the_live_capture() {
        // Sent on 2026-10-28 and ACKed 00 under SDK control authority.
        let f = usb_flight_frame(0x5a1b, true);
        assert_eq!(
            f,
            [
                0x55, 0x10, 0x04, 0x56, 0x0a, 0x03, 0x1b, 0x5a, 0x40, 0x03, 0x1b, 0x01, 0x81, 0x01,
                0x3d, 0x13
            ]
        );
    }
}
