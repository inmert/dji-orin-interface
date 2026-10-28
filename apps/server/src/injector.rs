//! Gimbal command injector: streams the desired [`Commands`](crate::state::Commands) to ep2 (IN,
//! Orin → RC) at a fixed rate.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::thread::sleep;
use std::time::Duration;

use dji_common::logln;
use dji_protocol::gimbal::{self, Camera, GimbalMove, Recenter};
use dji_protocol::thermal;
use dji_transport::usb;

use crate::INJECT_HZ;
use crate::state::{Shared, ThermalCmd};

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
/// camera, a single release frame when a camera goes from active to released,
/// then any queued yaw recenter once its camera's release is out. A frame that
/// fails to *write* is retried next tick, so a lock change, release or reset is
/// never silently lost; a written reset is never repeated (its ACK is not
/// awaited, and a missing ACK does not mean the gimbal did not move).
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

        let resets = std::mem::take(&mut cmds.lock().unwrap().reset_pending);
        let mut retry = Vec::new();
        for cam in resets {
            let slot = usize::from(cam == Camera::Two);
            if was_active[slot] {
                retry.push(cam); // its release has not gone out yet
                continue;
            }
            match send(gimbal::recenter_frame(cam, Recenter::Yaw, seq), &mut seq) {
                Sent::Ok => logln!("injector: yaw recenter sent to camera {}", slot + 1),
                Sent::Retry => retry.push(cam),
                Sent::Gone => return,
            }
        }
        if !retry.is_empty() {
            let mut c = cmds.lock().unwrap();
            for cam in retry {
                if !c.reset_pending.contains(&cam) {
                    c.reset_pending.push(cam);
                }
            }
        }

        // Thermal camera settings: each sent once, in order; a failed write
        // is retried next tick ahead of anything queued since.
        let pending = cmds.lock().unwrap().take_thermal();
        for (i, cmd) in pending.iter().enumerate() {
            let frame = match *cmd {
                ThermalCmd::Spot(x, y) => thermal::spot_frame(x, y, seq),
                ThermalCmd::Mode(m) => thermal::display_mode_frame(m, seq),
                ThermalCmd::Isotherm(bound, v) => thermal::isotherm_frame(bound, v, seq),
                ThermalCmd::Measure(m) => thermal::measure_frame(m, seq),
                ThermalCmd::Area([l, t, r, b]) => thermal::area_frame(l, t, r, b, seq),
            };
            match send(frame, &mut seq) {
                Sent::Ok => {
                    cmds.lock().unwrap().thermal_sent();
                    logln!("injector: thermal {cmd:?} sent");
                }
                Sent::Retry => {
                    cmds.lock().unwrap().retry_thermal(&pending[i..]);
                    break;
                }
                Sent::Gone => {
                    cmds.lock().unwrap().retry_thermal(&pending[i..]);
                    return;
                }
            }
        }
        sleep(period);
    }
}
