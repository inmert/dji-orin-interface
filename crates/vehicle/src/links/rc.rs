//! The RC link: the two bulk endpoints of the Orin's accessory gadget.
//!
//! * [`read`] — ep1 (OUT, RC → Orin): reassembles the DUML and folds it into
//!   the [`Vehicle`]'s telemetry, hands ACKs to the motors' mailbox, and
//!   passes the RC mux's other channels (camera video) to a sink.
//! * [`write()`] — ep2 (IN, Orin → RC): streams the gimbal state and sends the
//!   queued thermal, lock, recenter and motor frames at [`INJECT_HZ`].
//!
//! `RC_DUMP=<file>` also appends every raw ep1 byte to `<file>` (up to
//! 512 MB) for offline analysis, e.g. of the non-DUML video downlink.

use std::fs::File;
use std::io::{ErrorKind, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::thread::sleep;
use std::time::Duration;

use dji_common::logln;
use dji_protocol::gimbal::{self, Camera, GimbalMove, Recenter};
use dji_protocol::{duml, flyc, rcmux, thermal};
use dji_transport::usb;

use crate::Vehicle;
use crate::thermal::ThermalCmd;

/// Command/telemetry rate, matching the ~10 Hz Pilot drove the gimbal at.
pub const INJECT_HZ: u64 = 10;

/// Size limit for an `RC_DUMP` file, so a forgotten dump can't fill the disk.
const DUMP_CAP: u64 = 512 * 1024 * 1024;

/// Raw ep1 bytes appended to a file, stopping at [`DUMP_CAP`].
struct Dump {
    file: File,
    written: u64,
}

impl Dump {
    fn from_env() -> Option<Self> {
        let path = std::env::var("RC_DUMP").ok()?;
        match File::options().create(true).append(true).open(&path) {
            Ok(file) => {
                logln!("telemetry: dumping raw ep1 to {path}");
                Some(Self { file, written: 0 })
            }
            Err(e) => {
                logln!("telemetry: cannot open RC_DUMP {path}: {e}");
                None
            }
        }
    }

    /// Append `data`; `false` once the dump is finished (cap reached or error).
    fn write(&mut self, data: &[u8]) -> bool {
        if self.written + data.len() as u64 > DUMP_CAP {
            logln!("telemetry: RC_DUMP reached {DUMP_CAP} bytes; stopped");
            return false;
        }
        self.written += data.len() as u64;
        self.file
            .write_all(data)
            .map_err(|e| logln!("telemetry: RC_DUMP write failed: {e}"))
            .is_ok()
    }
}

/// Read until the endpoint is gone (`ENODEV`). `ESHUTDOWN` (the RC disabled the
/// gadget, e.g. it was power-cycled) is waited out, so the reader resumes on
/// the next ENABLE without being respawned.
///
/// [`rcmux`] splits the stream: DUML arrives on two separate byte streams
/// (the mux's DUML channel, and outer frames sent outside the mux), each with
/// its own decoder so a frame split across packets is reassembled; every
/// other channel's payload goes to `video(channel, payload)`, which must not
/// block (the camera feeds drop what they cannot take).
pub fn read(ep1: OwnedFd, vehicle: &Vehicle, mut video: impl FnMut(u8, &[u8])) {
    let tele = &vehicle.telemetry;
    // One decoder per DUML byte stream: [mux channel, direct].
    let mut decoders = [duml::Decoder::new(), duml::Decoder::new()];
    let mut demux = rcmux::Demux::new();
    let mut buf = vec![0u8; 16384];
    let mut dump = Dump::from_env();
    loop {
        match usb::read(ep1.as_fd(), &mut buf) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
                if dump.as_mut().is_some_and(|d| !d.write(&buf[..n])) {
                    dump = None;
                }
                demux.push(&buf[..n], |ch, payload| match ch {
                    rcmux::DUML => decoders[0].push(payload),
                    rcmux::DUML_DIRECT => decoders[1].push(payload),
                    _ => video(ch, payload),
                });
                let mut t = tele.lock().unwrap();
                let mut each = |f: &duml::Frame| {
                    if f.crc_ok {
                        vehicle.motors.deliver_duml_ack(f);
                        t.update(f);
                        if f.cmd_set == flyc::CMD_SET && f.cmd_id == flyc::CMD_ID_FUNCTION_CONTROL {
                            logln!(
                                "flyc: 0x03/0x2a {} seq {} type {:#04x} payload {:02x?}",
                                duml::DevName(f.sender),
                                f.seq,
                                f.cmd_type,
                                f.payload
                            );
                        }
                        if let Some((seq, status)) = thermal::ack(f) {
                            match status {
                                0x00 => logln!("thermal: camera accepted seq {seq}"),
                                _ => logln!(
                                    "thermal: camera REJECTED seq {seq} (status {status:#04x})"
                                ),
                            }
                        }
                    }
                };
                for d in &mut decoders {
                    d.drain(&mut each);
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == usb::errno::ENODEV {
                    logln!("telemetry: ep1 gone; reader stopping");
                    return;
                }
                if code == usb::errno::ESHUTDOWN {
                    // Any partial frame belongs to the old session.
                    decoders = [duml::Decoder::new(), duml::Decoder::new()];
                    demux = rcmux::Demux::new();
                }
                sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Outcome of one ep2 write.
enum Sent {
    Ok,
    /// Transient failure (e.g. `ESHUTDOWN` while the RC has the gadget
    /// disabled); the caller retries on a later tick.
    Retry,
    /// The endpoint is gone (`ENODEV`); the writer stops.
    Gone,
}

fn write_ep2(ep2: BorrowedFd<'_>, frame: &[u8]) -> Sent {
    match usb::write(ep2, frame) {
        Ok(_) => Sent::Ok,
        Err(e) if e.raw_os_error() == Some(usb::errno::ENODEV) => {
            logln!("rc: ep2 gone; stopping");
            Sent::Gone
        }
        Err(_) => Sent::Retry,
    }
}

/// Write to ep2 until it is gone (`ENODEV`).
///
/// Each tick: emit a pending camera-lock change once, a move frame per active
/// camera, a single release frame when a camera goes from active to released,
/// then any queued yaw recenter once its camera's release is out. A frame that
/// fails to *write* is retried next tick, so a lock change, release or reset is
/// never silently lost; a written reset is never repeated (its ACK is not
/// awaited, and a missing ACK does not mean the gimbal did not move).
pub fn write(ep2: OwnedFd, vehicle: &Vehicle) {
    let (gimbal, thermal, motors) = (&vehicle.gimbal, &vehicle.thermal, &vehicle.motors);
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
        let lock = gimbal.lock().lock_pending.take();
        if let Some(on) = lock {
            let mut r = send(gimbal::lock_frame(on, seq), &mut seq);
            if !on && matches!(r, Sent::Ok) {
                // Pilot pairs the release with the 0x05 companion command.
                r = send(gimbal::lock_release_aux_frame(seq), &mut seq);
            }
            match r {
                Sent::Ok => logln!(
                    "rc: camera lock {}",
                    if on { "engaged" } else { "released" }
                ),
                // Requeue unless the UI changed its mind in the meantime.
                Sent::Retry => {
                    gimbal.lock().lock_pending.get_or_insert(on);
                }
                Sent::Gone => return,
            }
        }

        // RC-link motor start/stop: sent once; the reader logs the FC's ACK
        // and the motors module matches it by sequence.
        let rc_motor = motors.take_rc_pending();
        if let Some(on) = rc_motor {
            let code = if on {
                flyc::FUNCTION_START_MOTOR
            } else {
                flyc::FUNCTION_STOP_MOTOR
            };
            let frame_seq = seq;
            match send(flyc::function_control_frame(code, seq), &mut seq) {
                Sent::Ok => {
                    motors.rc_sent(frame_seq);
                    logln!("rc: function {code:#04x} sent (seq {frame_seq})");
                }
                Sent::Retry => motors.requeue_rc(on),
                Sent::Gone => return,
            }
        }

        let (c1, c2) = {
            let g = gimbal.lock();
            (g.cam1, g.cam2)
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

        let resets = std::mem::take(&mut gimbal.lock().reset_pending);
        let mut retry = Vec::new();
        for cam in resets {
            let slot = usize::from(cam == Camera::Two);
            if was_active[slot] {
                retry.push(cam); // its release has not gone out yet
                continue;
            }
            match send(gimbal::recenter_frame(cam, Recenter::Yaw, seq), &mut seq) {
                Sent::Ok => logln!("rc: yaw recenter sent to camera {}", slot + 1),
                Sent::Retry => retry.push(cam),
                Sent::Gone => return,
            }
        }
        if !retry.is_empty() {
            let mut g = gimbal.lock();
            for cam in retry {
                g.requeue_reset(cam);
            }
        }

        // Thermal camera settings: each sent once, in order; a failed write
        // is retried next tick ahead of anything queued since.
        let pending = thermal.lock().take();
        for (i, cmd) in pending.iter().enumerate() {
            let frame = match *cmd {
                ThermalCmd::Spot(x, y) => thermal::spot_frame(x, y, seq),
                ThermalCmd::Mode(m) => thermal::display_mode_frame(m, seq),
                ThermalCmd::Isotherm(bound, v) => thermal::isotherm_frame(bound, v, seq),
                ThermalCmd::Measure(m) => thermal::measure_frame(m, seq),
                ThermalCmd::Area([l, t, r, b]) => thermal::area_frame(l, t, r, b, seq),
            };
            let frame_seq = seq;
            match send(frame, &mut seq) {
                Sent::Ok => {
                    thermal.lock().sent();
                    // The reader logs the camera's ACK under the same seq.
                    logln!("rc: thermal {cmd:?} sent (seq {frame_seq})");
                }
                Sent::Retry => {
                    thermal.lock().retry(&pending[i..]);
                    break;
                }
                Sent::Gone => {
                    thermal.lock().retry(&pending[i..]);
                    return;
                }
            }
        }
        sleep(period);
    }
}
