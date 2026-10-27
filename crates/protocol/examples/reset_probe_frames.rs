//! Print packets for the documented yaw-reset probes; performs no I/O.
use dji_protocol::gimbal::{
    Camera, GimbalMove, command, lock_frame, lock_release_aux_frame, move_frame,
};
fn main() {
    for (label, bytes) in [
        (
            "release",
            move_frame(Camera::One, GimbalMove::RELEASE, 0x7100),
        ),
        (
            "release2",
            move_frame(Camera::Two, GimbalMove::RELEASE, 0x7200),
        ),
        (
            "reset2",
            command::recenter_frame(Camera::Two, command::Recenter::Yaw, 0x7203),
        ),
        ("link", lock_frame(true, 0x7301)),
        (
            "reset_linked",
            command::recenter_frame(Camera::One, command::Recenter::Yaw, 0x7303),
        ),
        (
            "pan2",
            move_frame(
                Camera::Two,
                GimbalMove {
                    pitch: 0,
                    yaw: 80,
                    active: true,
                },
                0x7201,
            ),
        ),
        (
            "pan_linked",
            move_frame(
                Camera::One,
                GimbalMove {
                    pitch: 0,
                    yaw: 80,
                    active: true,
                },
                0x7302,
            ),
        ),
        ("unlink", lock_frame(false, 0x7101)),
        ("unlink_aux", lock_release_aux_frame(0x7102)),
        (
            "reset",
            command::recenter_frame(Camera::One, command::Recenter::Yaw, 0x7103),
        ),
    ] {
        print!("{label} ");
        for b in bytes {
            print!("{b:02x}");
        }
        println!();
    }
}
