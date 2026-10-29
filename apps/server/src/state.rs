//! Shared desired gimbal state, and the UI command messages that change it.

use std::sync::{Arc, Mutex};

use serde::Deserialize;

use dji_common::logln;
use dji_protocol::gimbal::{Camera, GimbalMove};
use dji_protocol::thermal::{DisplayMode, Isotherm, Measure, Report};

/// Desired gimbal state, one slot per camera, updated by the WS clients and
/// streamed to ep2 by the injector thread.
#[derive(Default)]
pub struct Commands {
    pub cam1: GimbalMove,
    pub cam2: GimbalMove,
    /// When linked, the aircraft's native camera-lock is engaged: we drive only
    /// camera 1 (the master) and the drone mirrors camera 2 (see [`apply_move`]).
    ///
    /// [`apply_move`]: Commands::apply_move
    pub linked: bool,
    /// A pending lock change for the injector to emit once: `Some(true)` engages
    /// the lock, `Some(false)` releases it.
    pub lock_pending: Option<bool>,
    /// Yaw recenters for the injector to emit once each, in order, no repeats.
    pub reset_pending: Vec<Camera>,
    /// Thermal camera settings for the injector to send once each, in order.
    pub thermal_pending: Vec<ThermalCmd>,
    /// Reserve queue capacity while the injector writes without holding this mutex.
    thermal_in_flight: usize,
    /// Keep the isotherm band fitted to the scene ([`crate::thermal_auto`]).
    pub thermal_auto: bool,
    /// Motor start (`true`) / stop over the RC link, sent once
    /// by the injector as `0x03/0x2a`.
    pub rc_motor_pending: Option<bool>,
    /// The sequence number the injector sent the last RC-link motor frame
    /// with, for [`crate::motors`] to match the FC's ACK.
    pub rc_motor_sent: Option<u16>,
    /// A request for [`crate::motors`], carried out once.
    pub motors_pending: Option<crate::motors::Request>,
    /// Progress and outcome of motor requests, and the live motor state.
    pub motors: crate::motors::Status,
    /// "Allow with USB" succeeded during the current USB session.
    pub usb_allowed: bool,
}

/// A thermal camera setting from the UI (see [`dji_protocol::thermal`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThermalCmd {
    /// Move the spot meter to normalised `(x, y)` on the thermal image.
    Spot(f32, f32),
    /// Switch what the thermal camera's video shows.
    Mode(DisplayMode),
    /// Set one isotherm bound, °C.
    Isotherm(Isotherm, i16),
    /// Choose what the camera measures.
    Measure(Measure),
    /// Area-measurement rectangle: left, top, right, bottom (0..1).
    Area([f32; 4]),
}

/// Isotherm bounds in °C: lower, middle, upper.
pub type Band = (i16, i16, i16);
const MAX_THERMAL_PENDING: usize = 64;

/// The isotherm bounds the UI may set (Pilot's fire preset went to 751 °C;
/// the XT2's low-gain range ends around there).
pub const ISOTHERM_RANGE: std::ops::RangeInclusive<i16> = -40..=800;

/// Widen over every accepted target before changing the middle, then narrow.
/// Always send all steps: reports can lag previously sent or in-flight commands.
/// Include the reported extremes as well, in case Pilot set a wider band.
pub fn isotherm_sequence(cur: Band, target: Band) -> Vec<ThermalCmd> {
    vec![
        ThermalCmd::Isotherm(Isotherm::Lower, cur.0.min(*ISOTHERM_RANGE.start())),
        ThermalCmd::Isotherm(Isotherm::Upper, cur.2.max(*ISOTHERM_RANGE.end())),
        ThermalCmd::Isotherm(Isotherm::Middle, target.1),
        ThermalCmd::Isotherm(Isotherm::Lower, target.0),
        ThermalCmd::Isotherm(Isotherm::Upper, target.2),
    ]
}

/// The camera's current isotherm bounds from its report.
pub fn band_of(r: &Report) -> Band {
    (r.isotherm_lower, r.isotherm_middle, r.isotherm_upper)
}

pub type Shared = Arc<Mutex<Commands>>;

impl Commands {
    pub fn thermal_capacity(&self, additional: usize) -> bool {
        self.thermal_pending.len() + self.thermal_in_flight + additional <= MAX_THERMAL_PENDING
    }

    pub fn take_thermal(&mut self) -> Vec<ThermalCmd> {
        let pending = std::mem::take(&mut self.thermal_pending);
        self.thermal_in_flight = pending.len();
        pending
    }

    pub fn thermal_sent(&mut self) {
        self.thermal_in_flight -= 1;
    }

    pub fn retry_thermal(&mut self, unsent: &[ThermalCmd]) {
        let newer = std::mem::take(&mut self.thermal_pending);
        self.thermal_pending = unsent.iter().copied().chain(newer).collect();
        self.thermal_in_flight = 0;
    }

    fn set(&mut self, cam: Camera, mv: GimbalMove) {
        match cam {
            Camera::One => self.cam1 = mv,
            Camera::Two => self.cam2 = mv,
        }
    }

    /// Apply a move from the UI. When linked, everything drives camera 1 (the
    /// master); the aircraft moves camera 2 to follow, so we never send camera 2
    /// directly. Otherwise only the named camera moves.
    pub fn apply_move(&mut self, cam: Camera, mv: GimbalMove) {
        if self.linked {
            self.cam1 = mv;
        } else {
            self.set(cam, mv);
        }
    }

    pub fn release_all(&mut self) {
        self.cam1.active = false;
        self.cam2.active = false;
    }

    /// Queue a yaw recenter (the hardware-verified native reset) for `target`
    /// (`None` = both cameras), releasing any movement on the targets first.
    ///
    /// While linked, the aircraft mirrors camera 2 from camera 1, so every
    /// reset goes to the master only; that resets the pair (verified on the
    /// M210). Independent, each camera is reset on its own.
    pub fn request_reset(&mut self, target: Option<Camera>) {
        let cams: &[Camera] = match (self.linked, target) {
            (true, _) => &[Camera::One],
            (false, Some(Camera::One)) => &[Camera::One],
            (false, Some(Camera::Two)) => &[Camera::Two],
            (false, None) => &[Camera::One, Camera::Two],
        };
        for &cam in cams {
            self.set(cam, GimbalMove::default());
            if !self.reset_pending.contains(&cam) {
                self.reset_pending.push(cam);
            }
        }
    }

    /// Enter/leave linked mode: queue the native lock command and, when linking,
    /// stop driving camera 2 directly (the drone takes it over).
    pub fn set_linked(&mut self, on: bool) {
        self.linked = on;
        self.lock_pending = Some(on);
        if on {
            self.cam2.active = false;
        }
    }
}

/// A command message from the UI (mirrors `Command` in web/src/api.ts).
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum WsCommand {
    /// Set one camera's gimbal move (pitch/yaw, active while held).
    Gimbal {
        camera: u8,
        #[serde(default)]
        pitch: i16,
        #[serde(default)]
        yaw: i16,
        #[serde(default)]
        active: bool,
    },
    /// Engage/release the aircraft's native camera lock (camera 2 follows 1).
    Link { on: bool },
    /// Recenter yaw on one camera (1 or 2), or both when `camera` is absent.
    Reset {
        #[serde(default)]
        camera: Option<u8>,
    },
    /// Release every gimbal immediately.
    Stop,
    /// Move the thermal spot meter (normalised 0..1 on the thermal image).
    Spot { x: f32, y: f32 },
    /// Set the thermal camera's display mode (0 visual, 2 PiP, 3 thermal).
    #[serde(rename = "thermal_mode")]
    ThermalMode { mode: u8 },
    /// Set the isotherm band, °C (lower ≤ middle ≤ upper).
    Isotherm { lower: i16, middle: i16, upper: i16 },
    /// Fit the isotherm band to the scene automatically, or stop doing so.
    #[serde(rename = "thermal_auto")]
    ThermalAuto { on: bool },
    /// Start (`on`) or stop all motors over the OSDK UART.
    Motors { on: bool },
    /// Allow arming while the aircraft's USB API port is connected.
    #[serde(rename = "usb_flight")]
    UsbFlight,
    /// Start/stop motors over the RC link (DUML `0x03/0x2a`).
    #[serde(rename = "motors_rc")]
    MotorsRc { on: bool },
}

/// Parse one UI message and apply it to the shared state. `thermal` is the
/// thermal camera's latest report, which some thermal commands depend on.
pub fn handle(text: &str, cmds: &Shared, thermal: Option<&Report>) {
    match serde_json::from_str::<WsCommand>(text) {
        Ok(WsCommand::Gimbal {
            camera,
            pitch,
            yaw,
            active,
        }) => {
            if let Some(cam) = Camera::from_number(camera) {
                cmds.lock()
                    .unwrap()
                    .apply_move(cam, GimbalMove { pitch, yaw, active });
            }
        }
        Ok(WsCommand::Link { on }) => {
            cmds.lock().unwrap().set_linked(on);
            logln!("link: {}", if on { "on" } else { "off" });
        }
        Ok(WsCommand::Reset { camera }) => {
            let target = match camera {
                None => None,
                Some(n) => match Camera::from_number(n) {
                    Some(cam) => Some(cam),
                    None => return logln!("ws: reset: no camera {n}"),
                },
            };
            cmds.lock().unwrap().request_reset(target);
            logln!("reset: queued for {camera:?} (None = all)");
        }
        Ok(WsCommand::Stop) => cmds.lock().unwrap().release_all(),
        Ok(WsCommand::Spot { x, y }) => {
            if !(x.is_finite() && y.is_finite()) {
                return logln!("ws: spot: bad position");
            }
            let mut c = cmds.lock().unwrap();
            if !c.thermal_capacity(2) {
                return logln!("ws: thermal queue full; spot rejected");
            }
            // Only the latest unsent spot (and its switch to spot measurement)
            // matters.
            c.thermal_pending.retain(|t| {
                !matches!(t, ThermalCmd::Spot(..) | ThermalCmd::Measure(Measure::Spot))
            });
            // The camera rejects a spot unless it is in spot measurement, which
            // also ends area-based auto range.
            if thermal.is_none_or(|r| r.measure != Measure::Spot as u8) {
                c.thermal_auto = false;
                c.thermal_pending.push(ThermalCmd::Measure(Measure::Spot));
            }
            c.thermal_pending.push(ThermalCmd::Spot(x, y));
        }
        Ok(WsCommand::ThermalMode { mode }) => match DisplayMode::from_u8(mode) {
            Some(m) => {
                let mut c = cmds.lock().unwrap();
                if let Some(mode) = c
                    .thermal_pending
                    .iter_mut()
                    .find(|t| matches!(t, ThermalCmd::Mode(_)))
                {
                    *mode = ThermalCmd::Mode(m);
                } else if c.thermal_capacity(1) {
                    c.thermal_pending.push(ThermalCmd::Mode(m));
                } else {
                    return logln!("ws: thermal queue full; mode rejected");
                }
                logln!("thermal: display mode {mode} queued");
            }
            None => logln!("ws: thermal_mode: unknown mode {mode}"),
        },
        Ok(WsCommand::Isotherm {
            lower,
            middle,
            upper,
        }) => {
            let target = (lower, middle, upper);
            if !(lower <= middle && middle <= upper)
                || ![lower, upper].iter().all(|v| ISOTHERM_RANGE.contains(v))
            {
                return logln!("ws: isotherm: {target:?} out of order or range");
            }
            let Some(r) = thermal else {
                return logln!("ws: isotherm: no thermal report yet");
            };
            let mut c = cmds.lock().unwrap();
            if !c.thermal_capacity(5) {
                return logln!("ws: thermal queue full; isotherm rejected");
            }
            c.thermal_auto = false;
            c.thermal_pending
                .retain(|t| !matches!(t, ThermalCmd::Isotherm(..)));
            c.thermal_pending
                .extend(isotherm_sequence(band_of(r), target));
            logln!("thermal: isotherm {target:?} queued (auto off)");
        }
        Ok(WsCommand::ThermalAuto { on }) => {
            cmds.lock().unwrap().thermal_auto = on;
            logln!("thermal: auto range {}", if on { "on" } else { "off" });
        }
        Ok(WsCommand::Motors { on }) => {
            use crate::motors::Request;
            let mut c = cmds.lock().unwrap();
            // A stop replaces anything waiting; nothing replaces a waiting stop.
            if on && c.motors_pending == Some(Request::Stop) {
                return logln!("ws: motors: start ignored, stop pending");
            }
            c.motors_pending = Some(if on { Request::Start } else { Request::Stop });
            logln!("ws: motors {} queued", if on { "start" } else { "stop" });
        }
        Ok(WsCommand::MotorsRc { on }) => {
            cmds.lock().unwrap().rc_motor_pending = Some(on);
            logln!(
                "ws: RC-link motors {} queued",
                if on { "start" } else { "stop" }
            );
        }
        Ok(WsCommand::UsbFlight) => {
            use crate::motors::Request;
            let mut c = cmds.lock().unwrap();
            if c.motors_pending.is_some() {
                return logln!("ws: usb_flight ignored, a motor request is pending");
            }
            c.motors_pending = Some(Request::UsbFlight);
            logln!("ws: usb_flight queued");
        }
        Err(e) => logln!("ws: bad command: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle_t(text: &str, cmds: &Shared) {
        handle(text, cmds, None);
    }

    fn pending(cmds: &Shared) -> Vec<ThermalCmd> {
        cmds.lock().unwrap().thermal_pending.clone()
    }

    fn spot_report(band: Band) -> Report {
        Report {
            measure: Measure::Spot as u8,
            isotherm_lower: band.0,
            isotherm_middle: band.1,
            isotherm_upper: band.2,
            ..Report::default()
        }
    }

    #[test]
    fn isotherm_sequence_keeps_the_bounds_ordered_at_every_step() {
        let cases = [
            ((27, 32, 38), (100, 150, 200)),  // all up
            ((100, 150, 200), (-10, 0, 5)),   // all down
            ((27, 32, 751), (119, 177, 695)), // Pilot's custom step
            ((0, 10, 20), (5, 10, 15)),       // narrow
            ((5, 10, 15), (5, 10, 15)),       // no change
        ];
        for (cur, target) in cases {
            let mut band = cur;
            for cmd in isotherm_sequence(cur, target) {
                let ThermalCmd::Isotherm(bound, v) = cmd else {
                    panic!("not an isotherm write: {cmd:?}")
                };
                match bound {
                    Isotherm::Lower => band.0 = v,
                    Isotherm::Middle => band.1 = v,
                    Isotherm::Upper => band.2 = v,
                }
                assert!(
                    band.0 <= band.1 && band.1 <= band.2,
                    "{cur:?}->{target:?}: {band:?}"
                );
            }
            assert_eq!(band, target);
        }
        assert_eq!(isotherm_sequence((5, 10, 15), (5, 10, 15)).len(), 5);
    }

    #[test]
    fn stale_report_and_partial_previous_band_do_not_break_ordering() {
        let report = (0, 10, 20);
        let first = isotherm_sequence(report, (100, 150, 200));
        for sent in 0..=first.len() {
            let mut actual = report;
            for cmd in first[..sent]
                .iter()
                .copied()
                .chain(isotherm_sequence(report, (5, 10, 15)))
            {
                match cmd {
                    ThermalCmd::Isotherm(Isotherm::Lower, v) => actual.0 = v,
                    ThermalCmd::Isotherm(Isotherm::Middle, v) => actual.1 = v,
                    ThermalCmd::Isotherm(Isotherm::Upper, v) => actual.2 = v,
                    _ => unreachable!(),
                }
                assert!(actual.0 <= actual.1 && actual.1 <= actual.2, "{actual:?}");
            }
            assert_eq!(actual, (5, 10, 15));
        }
    }

    #[test]
    fn thermal_queue_reserves_in_flight_capacity_and_retries_only_unsent() {
        let mut c = Commands::default();
        let mode = ThermalCmd::Mode(DisplayMode::Thermal);
        c.thermal_pending = vec![mode; MAX_THERMAL_PENDING];
        let batch = c.take_thermal();
        assert!(!c.thermal_capacity(1));
        c.thermal_sent();
        assert!(c.thermal_capacity(1));
        c.thermal_pending.push(ThermalCmd::Spot(0.5, 0.5));
        c.retry_thermal(&batch[1..]);
        assert_eq!(c.thermal_pending.len(), MAX_THERMAL_PENDING);
        assert_eq!(c.thermal_pending.last(), Some(&ThermalCmd::Spot(0.5, 0.5)));
        assert_eq!(c.thermal_in_flight, 0);
        assert!(!c.thermal_capacity(1));
    }

    #[test]
    fn queued_modes_coalesce_and_full_queue_rejects_without_mutation() {
        let cmds: Shared = Arc::default();
        for _ in 0..100 {
            handle_t(r#"{"cmd":"thermal_mode","mode":3}"#, &cmds);
            handle_t(r#"{"cmd":"thermal_mode","mode":0}"#, &cmds);
        }
        assert_eq!(pending(&cmds), [ThermalCmd::Mode(DisplayMode::Visual)]);
        cmds.lock().unwrap().thermal_pending =
            vec![ThermalCmd::Measure(Measure::Area); MAX_THERMAL_PENDING];
        handle_t(r#"{"cmd":"thermal_mode","mode":3}"#, &cmds);
        handle_t(r#"{"cmd":"spot","x":0.5,"y":0.5}"#, &cmds);
        assert_eq!(pending(&cmds).len(), MAX_THERMAL_PENDING);
    }

    #[test]
    fn isotherm_and_spot_commands_respect_the_camera_state() {
        let cmds: Shared = Arc::default();
        cmds.lock().unwrap().thermal_auto = true;
        let r = spot_report((27, 32, 38));
        handle(
            r#"{"cmd":"isotherm","lower":30,"middle":20,"upper":40}"#,
            &cmds,
            Some(&r),
        ); // out of order
        handle(
            r#"{"cmd":"isotherm","lower":20,"middle":30,"upper":40}"#,
            &cmds,
            None,
        ); // no report yet
        assert!(pending(&cmds).is_empty());
        handle(
            r#"{"cmd":"isotherm","lower":20,"middle":30,"upper":40}"#,
            &cmds,
            Some(&r),
        );
        assert_eq!(
            pending(&cmds),
            [
                ThermalCmd::Isotherm(Isotherm::Lower, -40),
                ThermalCmd::Isotherm(Isotherm::Upper, 800),
                ThermalCmd::Isotherm(Isotherm::Middle, 30),
                ThermalCmd::Isotherm(Isotherm::Lower, 20),
                ThermalCmd::Isotherm(Isotherm::Upper, 40),
            ]
        );
        assert!(!cmds.lock().unwrap().thermal_auto); // a manual band ends auto

        // A spot while measuring an area switches to spot measurement first.
        let cmds: Shared = Arc::default();
        let area = Report {
            measure: Measure::Area as u8,
            ..r
        };
        handle(r#"{"cmd":"spot","x":0.5,"y":0.5}"#, &cmds, Some(&area));
        assert_eq!(
            pending(&cmds),
            [
                ThermalCmd::Measure(Measure::Spot),
                ThermalCmd::Spot(0.5, 0.5)
            ]
        );
        let cmds: Shared = Arc::default();
        handle(r#"{"cmd":"spot","x":0.5,"y":0.5}"#, &cmds, Some(&r));
        assert_eq!(pending(&cmds), [ThermalCmd::Spot(0.5, 0.5)]);
    }

    #[test]
    fn linked_drives_only_the_master_and_queues_the_lock() {
        let mut c = Commands::default();
        c.set_linked(true);
        assert!(c.linked);
        assert_eq!(c.lock_pending, Some(true)); // injector will engage the lock
        // A move while linked drives camera 1 only; the drone follows camera 2.
        c.apply_move(
            Camera::Two,
            GimbalMove {
                pitch: 30,
                yaw: -5,
                active: true,
            },
        );
        assert!(c.cam1.active);
        assert!(!c.cam2.active);
        assert_eq!((c.cam1.pitch, c.cam1.yaw), (30, -5));
    }

    #[test]
    fn unlinking_queues_the_release() {
        let mut c = Commands::default();
        c.set_linked(true);
        c.set_linked(false);
        assert!(!c.linked);
        assert_eq!(c.lock_pending, Some(false));
    }

    #[test]
    fn unlinked_move_drives_only_the_named_camera() {
        let mut c = Commands::default();
        c.apply_move(
            Camera::Two,
            GimbalMove {
                pitch: 10,
                yaw: 0,
                active: true,
            },
        );
        assert!(!c.cam1.active && c.cam2.active);
    }

    #[test]
    fn independent_resets_target_the_named_cameras_and_release_them() {
        let mut c = Commands {
            cam2: GimbalMove {
                pitch: 5,
                yaw: 5,
                active: true,
            },
            ..Default::default()
        };
        c.request_reset(Some(Camera::Two));
        assert_eq!(c.reset_pending, [Camera::Two]);
        assert!(!c.cam2.active); // released before the reset goes out
        c.request_reset(None);
        assert_eq!(c.reset_pending, [Camera::Two, Camera::One]); // no repeats
    }

    #[test]
    fn linked_resets_go_to_the_master_only() {
        let mut c = Commands::default();
        c.set_linked(true);
        for target in [Some(Camera::Two), None, Some(Camera::One)] {
            c.request_reset(target);
        }
        assert_eq!(c.reset_pending, [Camera::One]);
    }

    #[test]
    fn handle_parses_ui_messages() {
        let cmds: Shared = Arc::default();
        handle_t(
            r#"{"cmd":"gimbal","camera":2,"pitch":5,"yaw":-3,"active":true}"#,
            &cmds,
        );
        assert_eq!(
            cmds.lock().unwrap().cam2,
            GimbalMove {
                pitch: 5,
                yaw: -3,
                active: true
            }
        );
        handle_t(r#"{"cmd":"stop"}"#, &cmds);
        assert!(!cmds.lock().unwrap().cam2.active);
        handle_t(r#"{"cmd":"link","on":true}"#, &cmds);
        assert!(cmds.lock().unwrap().linked);
        handle_t(r#"{"cmd":"reset","camera":2}"#, &cmds);
        handle_t(r#"{"cmd":"reset","camera":7}"#, &cmds); // ignored
        assert_eq!(cmds.lock().unwrap().reset_pending, [Camera::One]); // linked
        handle_t(r#"{"cmd":"gimbal","camera":9,"active":true}"#, &cmds); // ignored
        handle_t("not json", &cmds); // logged, ignored
    }

    #[test]
    fn thermal_commands_queue_in_order_with_only_the_latest_spot() {
        let cmds: Shared = Arc::default();
        handle_t(r#"{"cmd":"spot","x":0.1,"y":0.2}"#, &cmds);
        handle_t(r#"{"cmd":"thermal_mode","mode":3}"#, &cmds);
        handle_t(r#"{"cmd":"spot","x":0.5,"y":0.6}"#, &cmds); // replaces 0.1/0.2
        handle_t(r#"{"cmd":"thermal_mode","mode":1}"#, &cmds); // unknown: ignored
        handle_t(r#"{"cmd":"spot","x":"nan","y":0}"#, &cmds); // not a number: ignored
        assert_eq!(
            cmds.lock().unwrap().thermal_pending,
            [
                ThermalCmd::Mode(DisplayMode::Thermal),
                // No report yet, so the spot is preceded by spot measurement.
                ThermalCmd::Measure(Measure::Spot),
                ThermalCmd::Spot(0.5, 0.6)
            ]
        );
    }

    #[test]
    fn a_pending_motor_stop_is_never_replaced_by_a_start() {
        let cmds: Shared = Arc::new(Mutex::new(Commands::default()));
        handle_t(r#"{"cmd":"motors","on":true}"#, &cmds);
        use crate::motors::Request;
        assert_eq!(cmds.lock().unwrap().motors_pending, Some(Request::Start));
        handle_t(r#"{"cmd":"motors","on":false}"#, &cmds);
        handle_t(r#"{"cmd":"motors","on":true}"#, &cmds);
        handle_t(r#"{"cmd":"usb_flight"}"#, &cmds);
        assert_eq!(cmds.lock().unwrap().motors_pending, Some(Request::Stop));
    }
}
