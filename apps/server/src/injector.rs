//! Gimbal command injector: streams the desired [`Commands`](crate::state::Commands) to ep2 (IN,
//! Orin → RC) at a fixed rate.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::thread::sleep;
use std::time::Duration;

use dji_common::logln;
use dji_protocol::gimbal::{self, Camera, GimbalMove};
use dji_transport::usb;

use crate::INJECT_HZ;
use crate::state::Shared;

/// Outcome of one ep2 write.
enum Sent {
    Ok,
    /// Transient failure (e.g. `ESHUTDOWN` while the RC has the gadget
    /// disabled); the caller retries on a later tick.
    Retry,
    /// The endpoint is gone (`ENODEV`); the injector stops.
    Gone,
}

fn write_ep2(ep2: BorrowedFd<'_>, frame: &[u8]) -> Sent {
    match usb::write(ep2, frame) {
        Ok(_) => Sent::Ok,
        Err(e) if e.raw_os_error() == Some(usb::errno::ENODEV) => {
            logln!("injector: ep2 gone; stopping");
            Sent::Gone
        }
        Err(_) => Sent::Retry,
    }
}

/// Each tick: emit a pending camera-lock change once, a move frame per active
/// camera, and a single release frame when a camera goes from active to
/// released. A frame that fails transiently is retried next tick, so a lock
/// change or a release is never silently lost.
pub fn run(ep2: OwnedFd, cmds: Shared) {
    let period = Duration::from_millis(1000 / INJECT_HZ);
    let mut seq: u16 = 0;
    let mut was_active = [false; 2];
    let send = |frame: Vec<u8>, seq: &mut u16| {
        let r = write_ep2(ep2.as_fd(), &frame);
        *seq = seq.wrapping_add(1);
        r
    };
    loop {
        // Emit a pending camera-lock change (native dual-gimbal follow).
        let lock = cmds.lock().unwrap().lock_pending.take();
        if let Some(on) = lock {
            let mut r = send(gimbal::lock_frame(on, seq), &mut seq);
            if !on && matches!(r, Sent::Ok) {
                // Pilot pairs the release with the 0x05 companion command.
                r = send(gimbal::lock_release_aux_frame(seq), &mut seq);
            }
            match r {
                Sent::Ok => logln!(
                    "injector: camera lock {}",
                    if on { "engaged" } else { "released" }
                ),
                // Requeue unless the UI changed its mind in the meantime.
                Sent::Retry => {
                    cmds.lock().unwrap().lock_pending.get_or_insert(on);
                }
                Sent::Gone => return,
            }
        }

        let (c1, c2) = {
            let c = cmds.lock().unwrap();
            (c.cam1, c.cam2)
        };
        for (slot, (cam, mv)) in [(Camera::One, c1), (Camera::Two, c2)]
            .into_iter()
            .enumerate()
        {
            let frame = if mv.active {
                Some(mv)
            } else if was_active[slot] {
                Some(GimbalMove::RELEASE)
            } else {
                None
            };
            let delivered = match frame.map(|m| send(gimbal::move_frame(cam, m, seq), &mut seq)) {
                None | Some(Sent::Ok) => true,
                Some(Sent::Retry) => false,
                Some(Sent::Gone) => return,
            };
            if delivered {
                was_active[slot] = mv.active;
            }
        }
        sleep(period);
    }
}
