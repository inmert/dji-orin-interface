//! Flight controller feature: the reports the FC pushes up over DUML.
//!
//! * [`osd`] — the `0x03/0x43` general-data report (attitude, flight mode,
//!   satellites), sent once a second on the aircraft's USB API port.
//!
//! * [`usb_flight_frame`] — the one command here: allow arming while the USB
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
