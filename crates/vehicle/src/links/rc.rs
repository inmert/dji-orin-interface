//! The RC link: the two bulk endpoints of the Orin's accessory gadget.
//!
//! * [`read`] — ep1 (OUT, RC → Orin): reassembles the DUML and folds it into
//!   the [`Vehicle`]'s telemetry, hands ACKs to the motors' mailbox, and
//!   passes the RC mux's other channels (camera video) to a sink.
//! * [`write()`] — ep2 (IN, Orin → RC): streams the gimbal state and sends the
//!   queued thermal, lock, recenter and motor frames at [`INJECT_HZ`].
//!
//! [`read`] can also append every raw ep1 byte to a file (up to 512 MB) for
//! offline analysis, e.g. of the non-DUML video downlink.

use std::fs::File;
use std::io::{ErrorKind, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
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
    fn open(path: &Path) -> Option<Self> {
        match File::options().create(true).append(true).open(path) {
            Ok(file) => {
                logln!("telemetry: dumping raw ep1 to {}", path.display());
                Some(Self { file, written: 0 })
            }
            Err(e) => {
                logln!("telemetry: cannot open dump {}: {e}", path.display());
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
/// the next ENABLE without being respawned. `dump` is a file to append the
/// raw bytes to, if wanted.
///
/// [`rcmux`] splits the stream: DUML arrives on two separate byte streams
/// (the mux's DUML channel, and outer frames sent outside the mux), each with
/// its own decoder so a frame split across packets is reassembled; every
/// other channel's payload goes to `video(channel, payload)`, which must not
/// block (the camera feeds drop what they cannot take).
pub fn read(
    ep1: OwnedFd,
    vehicle: &Vehicle,
    dump: Option<&Path>,
    mut video: impl FnMut(u8, &[u8]),
) {
    let tele = &vehicle.telemetry;
    // One decoder per DUML byte stream: [mux channel, direct].
    let mut decoders = [duml::Decoder::new(), duml::Decoder::new()];
    let mut demux = rcmux::Demux::new();
    let mut buf = vec![0u8; 16384];
    let mut dump = dump.and_then(Dump::open);
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
    // Start each writer's sequence numbers somewhere new, so an ACK left
    // over from an earlier writer cannot match a new request.
    let mut seq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| (d.subsec_nanos() >> 8) as u16);
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
        if let Some(frame) = rc_motor {
            let code = if frame.on {
                flyc::FUNCTION_START_MOTOR
            } else {
                flyc::FUNCTION_STOP_MOTOR
            };
            let frame_seq = seq;
            match send(flyc::function_control_frame(code, seq), &mut seq) {
                Sent::Ok => {
                    motors.rc_sent(frame, frame_seq);
                    logln!("rc: function {code:#04x} sent (seq {frame_seq})");
                }
                Sent::Retry => motors.requeue_rc(frame),
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

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};

    use dji_protocol::duml::{Device, Header};

    use super::*;

    /// One DUML frame as the RC passes it to the app: a type-0x49 outer frame.
    fn outer(duml: &[u8]) -> Vec<u8> {
        let len = u16::try_from(duml.len()).unwrap().to_le_bytes();
        [&[0x55, 0xcc, 0x49, 0x57, len[0], len[1], 0, 0][..], duml].concat()
    }

    /// Every frame the RC received: command set, id and payload.
    type Seen = Arc<Mutex<Vec<(u8, u8, Vec<u8>)>>>;

    /// The real reader and writer on socket pairs standing in for ep1 and
    /// ep2, and an RC + FC behind them that records every frame the writer
    /// sends (set, id, payload) and ACKs each motor frame with `code`, one
    /// byte at a time so the reader has to reassemble it.
    fn link(code: u8) -> (Arc<Vehicle>, Seen) {
        let v = Vehicle::new();
        let (ep1, mut rc_out) = UnixStream::pair().unwrap();
        let (ep2, mut rc_in) = UnixStream::pair().unwrap();
        {
            let v = Arc::clone(&v);
            std::thread::spawn(move || read(OwnedFd::from(ep1), &v, None, |_, _| {}));
        }
        {
            let v = Arc::clone(&v);
            std::thread::spawn(move || write(OwnedFd::from(ep2), &v));
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let mut d = duml::Decoder::new();
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = rc_in.read(&mut buf) {
                    d.push(&buf[..n]);
                    let mut acks = Vec::new();
                    d.drain(|f| {
                        assert!(f.crc_ok, "the writer sent a corrupt frame");
                        seen.lock()
                            .unwrap()
                            .push((f.cmd_set, f.cmd_id, f.payload.to_vec()));
                        if (f.cmd_set, f.cmd_id) == (flyc::CMD_SET, flyc::CMD_ID_FUNCTION_CONTROL) {
                            let h = Header {
                                sender: duml::addr(Device::FlightCtrl, 0),
                                receiver: f.sender,
                                seq: f.seq,
                                cmd_type: 0x80,
                                cmd_set: f.cmd_set,
                                cmd_id: f.cmd_id,
                            };
                            acks.push(outer(&duml::build(&h, &[code]).unwrap()));
                        }
                    });
                    for b in acks.concat() {
                        rc_out.write_all(&[b]).unwrap();
                    }
                }
            });
        }
        v.telemetry.lock().unwrap().link_up = true;
        (v, seen)
    }

    #[test]
    fn a_motor_start_goes_out_on_ep2_and_its_ack_comes_back_on_ep1() {
        let (v, seen) = link(0x00);
        match crate::motors::rc::send(true, &v.motors) {
            Ok(m) => assert_eq!(m, "motors started over the RC link"),
            Err(_) => panic!("the ACK did not reach the request"),
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [(
                flyc::CMD_SET,
                flyc::CMD_ID_FUNCTION_CONTROL,
                vec![flyc::FUNCTION_START_MOTOR]
            )]
        );
        // The reader folded the ACK into the snapshot as well.
        let t = v.snapshot();
        assert_eq!((t.link_up, t.frames), (true, 1));
    }

    #[test]
    fn a_refusal_over_the_wire_is_reported() {
        let (v, _) = link(0x03);
        assert!(matches!(
            crate::motors::rc::send(false, &v.motors),
            Err(crate::motors::rc::Fail::Refused(_))
        ));
    }

    #[test]
    fn a_move_streams_until_released_and_a_recenter_follows_the_release() {
        let (v, seen) = link(0x00);
        let mv = GimbalMove {
            pitch: 0,
            yaw: 200,
            active: true,
        };
        v.gimbal.apply_move(Camera::Two, mv);
        sleep(Duration::from_millis(350));
        v.gimbal.request_reset(Some(Camera::Two));
        sleep(Duration::from_millis(350));
        let frames = seen.lock().unwrap().clone();
        let moves: Vec<&Vec<u8>> = frames
            .iter()
            .filter(|(s, i, _)| (*s, *i) == (gimbal::CMD_SET, gimbal::CMD_ID_MOVE))
            .map(|(_, _, p)| p)
            .collect();
        assert!(
            moves.len() >= 2,
            "a held move repeats every tick: {moves:?}"
        );
        assert!(moves[..moves.len() - 1].iter().all(|p| **p == mv.payload()));
        assert_eq!(*moves[moves.len() - 1], [0; 7], "then exactly one release");
        let last = frames.last().unwrap();
        assert_eq!(
            (last.0, last.1, last.2.as_slice()),
            (gimbal::CMD_SET, 0x4c, &[0xfe, 0x01][..])
        );
        assert!(
            v.gimbal.lock().reset_pending.is_empty(),
            "sent once, not repeated"
        );
    }
}
