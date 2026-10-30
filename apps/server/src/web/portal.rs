//! The portal's command messages (mirrors `Command` in web/src/api.ts),
//! applied to the [`Vehicle`]. This is the only place the web wire format
//! meets the vehicle's typed operations.

use std::sync::Arc;

use serde::Deserialize;

use dji_common::logln;
use dji_vehicle::Vehicle;
use dji_vehicle::executor::ALREADY_RUNNING;

use crate::ai::understand;
use dji_vehicle::gimbal::{Camera, GimbalMove};
use dji_vehicle::motors::Request;
use dji_vehicle::thermal::{DisplayMode, Report};

/// A command message from the UI.
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
    /// Start (`on`) or stop all motors: over the RC link when it is up, else
    /// the OSDK UART ([`dji_vehicle::motors`]).
    Motors { on: bool },
    /// Allow arming while the aircraft's USB API port is connected.
    #[serde(rename = "usb_flight")]
    UsbFlight,
    /// A command sentence (`arm for 5 seconds then disarm`) or a JSON plan
    /// (`{"steps":[…]}`): interpreted ([`understand`]), then shown back
    /// (`run` false) or run. Interpretation may ask an online model, so it
    /// happens on its own thread; the snapshot's plan status shows progress.
    /// With `plan`, the steps the UI previewed and the operator confirmed,
    /// those run as given (validated) instead of interpreting `text` again:
    /// a model asked twice may answer differently.
    Plan {
        text: String,
        #[serde(default)]
        run: bool,
        #[serde(default)]
        plan: Option<dji_commands::Plan>,
    },
    /// Stop the running plan after its current step.
    Cancel,
}

/// Interpret `text` and then preview or run it, off the caller's thread.
/// A `confirmed` plan runs as given, with `text` as its label.
fn plan(vehicle: &Arc<Vehicle>, text: String, run: bool, confirmed: Option<dji_commands::Plan>) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return logln!("plan: empty command ignored");
    }
    if let Some(p) = confirmed {
        let ex = &vehicle.executor;
        let status = ex.status();
        // The preview's source, when this is the plan it showed.
        let source = if status.text == text && status.preview.as_ref() == Some(&p) {
            status.source
        } else {
            "confirmed".to_string()
        };
        match ex.start(vehicle, &text, p.clone(), &source) {
            Ok(()) => logln!("plan: started confirmed {text:?} ({source}): {p}"),
            Err(e) => {
                logln!("plan: refused confirmed {text:?}: {e}");
                // The status shows the running plan; its text stays its own.
                if e != ALREADY_RUNNING {
                    ex.preview(&text, Err(e), &source);
                }
            }
        }
        return;
    }
    let id = vehicle.executor.parsing(&text);
    let vehicle = Arc::clone(vehicle);
    std::thread::spawn(move || {
        let ex = &vehicle.executor;
        let result = understand::interpret(&text).and_then(|i| {
            i.plan
                .validate(ex.limits())
                .map_err(|e| format!("{e} (from {})", i.source))?;
            Ok(i)
        });
        match result {
            Ok(i) if run => match ex.start(&vehicle, &text, i.plan.clone(), &i.source) {
                Ok(()) => logln!("plan: started {text:?} ({}): {}", i.source, i.plan),
                Err(e) => {
                    logln!("plan: refused {text:?}: {e}");
                    if e != ALREADY_RUNNING {
                        ex.interpreted(id, &text, Err(e), &i.source);
                    }
                }
            },
            Ok(i) => {
                logln!("plan: checked {text:?} ({}): {}", i.source, i.plan);
                if !ex.interpreted(id, &text, Ok(i.plan), &i.source) {
                    logln!("plan: {text:?} was overtaken by a newer check");
                }
            }
            Err(e) => {
                logln!("plan: refused {text:?}: {e}");
                ex.interpreted(id, &text, Err(e), "");
            }
        }
    });
}

/// Parse one UI message and apply it. `thermal` is the thermal camera's
/// latest report, which some thermal commands depend on.
pub fn handle(text: &str, vehicle: &Arc<Vehicle>, thermal: Option<&Report>) {
    match serde_json::from_str::<WsCommand>(text) {
        Ok(WsCommand::Gimbal {
            camera,
            pitch,
            yaw,
            active,
        }) => {
            if let Some(cam) = Camera::from_number(camera) {
                vehicle
                    .gimbal
                    .apply_move(cam, GimbalMove { pitch, yaw, active });
            }
        }
        Ok(WsCommand::Link { on }) => {
            vehicle.gimbal.set_linked(on);
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
            vehicle.gimbal.request_reset(target);
            logln!("reset: queued for {camera:?} (None = all)");
        }
        Ok(WsCommand::Stop) => vehicle.gimbal.release_all(),
        Ok(WsCommand::Spot { x, y }) => {
            if let Err(e) = vehicle.thermal.spot(x, y, thermal) {
                logln!("ws: spot rejected: {e}");
            }
        }
        Ok(WsCommand::ThermalMode { mode }) => match DisplayMode::from_u8(mode) {
            Some(m) => match vehicle.thermal.set_mode(m) {
                Ok(()) => logln!("thermal: display mode {mode} queued"),
                Err(e) => logln!("ws: thermal mode rejected: {e}"),
            },
            None => logln!("ws: thermal_mode: unknown mode {mode}"),
        },
        Ok(WsCommand::Isotherm {
            lower,
            middle,
            upper,
        }) => {
            let target = (lower, middle, upper);
            match vehicle.thermal.set_isotherm(target, thermal) {
                Ok(()) => logln!("thermal: isotherm {target:?} queued (auto off)"),
                Err(e) => logln!("ws: isotherm {target:?} rejected: {e}"),
            }
        }
        Ok(WsCommand::ThermalAuto { on }) => {
            vehicle.thermal.set_auto(on);
            logln!("thermal: auto range {}", if on { "on" } else { "off" });
        }
        Ok(WsCommand::Motors { on }) => {
            let req = if on { Request::Start } else { Request::Stop };
            match vehicle.motors.request(req) {
                Ok(()) => logln!("ws: motors {} queued", if on { "start" } else { "stop" }),
                Err(e) => logln!("ws: motors: {e}"),
            }
        }
        Ok(WsCommand::UsbFlight) => match vehicle.motors.request(Request::UsbFlight) {
            Ok(()) => logln!("ws: usb_flight queued"),
            Err(e) => logln!("ws: {e}"),
        },
        // Steps run only with `run`: a check that carries steps is a check.
        Ok(WsCommand::Plan { text, run, plan: p }) => plan(vehicle, text, run, p.filter(|_| run)),
        Ok(WsCommand::Cancel) => {
            vehicle.executor.cancel();
            logln!("plan: cancel requested");
        }
        Err(e) => logln!("ws: bad command: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dji_vehicle::thermal::{Measure, ThermalCmd};

    #[test]
    fn handle_parses_ui_messages() {
        let v = Vehicle::new();
        handle(
            r#"{"cmd":"gimbal","camera":2,"pitch":5,"yaw":-3,"active":true}"#,
            &v,
            None,
        );
        assert_eq!(
            v.gimbal.lock().cam2,
            GimbalMove {
                pitch: 5,
                yaw: -3,
                active: true
            }
        );
        handle(r#"{"cmd":"stop"}"#, &v, None);
        assert!(!v.gimbal.lock().cam2.active);
        handle(r#"{"cmd":"link","on":true}"#, &v, None);
        assert!(v.gimbal.linked());
        handle(r#"{"cmd":"reset","camera":2}"#, &v, None);
        handle(r#"{"cmd":"reset","camera":7}"#, &v, None); // ignored
        assert_eq!(v.gimbal.lock().reset_pending, [Camera::One]); // linked
        handle(r#"{"cmd":"gimbal","camera":9,"active":true}"#, &v, None); // ignored
        handle("not json", &v, None); // logged, ignored
    }

    #[test]
    fn thermal_messages_reach_the_queue() {
        let v = Vehicle::new();
        handle(r#"{"cmd":"spot","x":0.1,"y":0.2}"#, &v, None);
        handle(r#"{"cmd":"thermal_mode","mode":3}"#, &v, None);
        handle(r#"{"cmd":"spot","x":0.5,"y":0.6}"#, &v, None); // replaces 0.1/0.2
        handle(r#"{"cmd":"thermal_mode","mode":1}"#, &v, None); // unknown: ignored
        handle(r#"{"cmd":"spot","x":"nan","y":0}"#, &v, None); // not a number: ignored
        assert_eq!(
            v.thermal.lock().pending,
            [
                ThermalCmd::Mode(DisplayMode::Thermal),
                ThermalCmd::Measure(Measure::Spot),
                ThermalCmd::Spot(0.5, 0.6)
            ]
        );
        handle(r#"{"cmd":"thermal_auto","on":true}"#, &v, None);
        assert!(v.thermal.auto());
        handle(
            r#"{"cmd":"isotherm","lower":20,"middle":30,"upper":40}"#,
            &v,
            None,
        ); // no report
        assert!(v.thermal.auto());
    }

    #[test]
    fn motor_messages_follow_the_request_rules() {
        let v = Vehicle::new();
        handle(r#"{"cmd":"motors","on":true}"#, &v, None);
        assert_eq!(v.motors.lock().pending, Some(Request::Start));
        handle(r#"{"cmd":"motors","on":false}"#, &v, None);
        handle(r#"{"cmd":"motors","on":true}"#, &v, None);
        handle(r#"{"cmd":"usb_flight"}"#, &v, None);
        assert_eq!(v.motors.lock().pending, Some(Request::Stop));
        // No UI message reaches the RC writer's motor slot directly.
        handle(r#"{"cmd":"motors_rc","on":true}"#, &v, None);
        assert_eq!(v.motors.take_rc_pending(), None);
    }

    #[test]
    fn garbage_changes_nothing() {
        let v = Vehicle::new();
        for garbage in [
            "",
            "null",
            "[]",
            "{\"cmd\":null}",
            r#"{"cmd":"gimbal"}"#,
            r#"{"cmd":"gimbal","camera":1,"pitch":40000,"active":true}"#,
            r#"{"cmd":"gimbal","camera":-1,"active":true}"#,
            r#"{"cmd":"Motors","on":true}"#,
            r#"{"cmd":"motors"}"#,
            r#"{"cmd":"motors","on":1}"#,
            r#"{"cmd":"spot","x":1e999,"y":0}"#,
            r#"{"cmd":"isotherm","lower":20,"middle":30}"#,
            r#"{"cmd":"thermal_mode","mode":300}"#,
            r#"{"cmd":"plan"}"#,
            r#"{"cmd":"plan","text":"   ","run":true}"#,
            r#"{"cmd":"plan","text":"x","run":true,"plan":{"steps":[{"cmd":"fly"}]}}"#,
            r#"{"cmd":"plan","text":"x","run":true,"plan":{"steps":[]}}"#,
            "{\"cmd\":\"stop\"",
        ] {
            handle(garbage, &v, None);
        }
        let g = v.gimbal.lock();
        assert!(!g.cam1.active && !g.cam2.active && !g.linked && g.reset_pending.is_empty());
        drop(g);
        assert_eq!(v.motors.lock().pending, None);
        assert!(v.thermal.lock().pending.is_empty());
        let s = v.executor.status();
        assert!(!s.running && !s.parsing && s.plan.is_none(), "{s:?}");
    }

    /// The RC link's reader and writer on socket pairs, the motor worker,
    /// and an RC + FC behind them that ACKs every motor frame; returns the
    /// function codes the FC received.
    fn aircraft(v: &Arc<Vehicle>) -> Arc<std::sync::Mutex<Vec<u8>>> {
        use std::io::{Read, Write};
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;

        use dji_protocol::duml::{self, Device, Header};
        use dji_protocol::flyc;
        use dji_vehicle::links::rc;

        let (ep1, mut rc_out) = UnixStream::pair().unwrap();
        let (ep2, mut rc_in) = UnixStream::pair().unwrap();
        let spawn = |f: Box<dyn FnOnce(&Vehicle) + Send>| {
            let v = Arc::clone(v);
            std::thread::spawn(move || f(&v));
        };
        spawn(Box::new(|v| {
            rc::read(OwnedFd::from(ep1), v, None, |_, _| {})
        }));
        spawn(Box::new(|v| rc::write(OwnedFd::from(ep2), v)));
        {
            let v = Arc::clone(v);
            std::thread::spawn(move || dji_vehicle::motors::run(&v, None));
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fc = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut d = duml::Decoder::new();
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = rc_in.read(&mut buf) {
                d.push(&buf[..n]);
                let mut acks = Vec::new();
                d.drain(|f| {
                    if (f.cmd_set, f.cmd_id) != (flyc::CMD_SET, flyc::CMD_ID_FUNCTION_CONTROL) {
                        return;
                    }
                    fc.lock().unwrap().push(f.payload[0]);
                    let h = Header {
                        sender: duml::addr(Device::FlightCtrl, 0),
                        receiver: f.sender,
                        seq: f.seq,
                        cmd_type: 0x80,
                        cmd_set: f.cmd_set,
                        cmd_id: f.cmd_id,
                    };
                    let ack = duml::build(&h, &[0x00]).unwrap();
                    let len = u16::try_from(ack.len()).unwrap().to_le_bytes();
                    acks.extend_from_slice(&[0x55, 0xcc, 0x49, 0x57, len[0], len[1], 0, 0]);
                    acks.extend_from_slice(&ack);
                });
                rc_out.write_all(&acks).unwrap();
            }
        });
        v.telemetry.lock().unwrap().link_up = true;
        seen
    }

    #[test]
    fn a_checked_sentence_runs_on_the_aircraft_as_the_ui_confirms_it() {
        use dji_protocol::flyc::{FUNCTION_START_MOTOR, FUNCTION_STOP_MOTOR};
        let v = Vehicle::new();
        let fc = aircraft(&v);
        // Check, as the portal's Check button sends it…
        let text = "arm for 0.3 seconds then disarm";
        handle(&format!(r#"{{"cmd":"plan","text":"{text}"}}"#), &v, None);
        let preview = settled(&v).preview.expect("a plan to confirm");
        assert!(fc.lock().unwrap().is_empty(), "checking sends nothing");
        // …then Run with the previewed steps, as the portal does after its
        // confirmation dialog.
        let run = serde_json::json!({"cmd": "plan", "text": text, "run": true, "plan": preview});
        handle(&run.to_string(), &v, None);
        let mut s = v.executor.status();
        for _ in 0..200 {
            if !s.running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
            s = v.executor.status();
        }
        assert_eq!((s.ok, s.message.as_str()), (Some(true), "done"), "{s:?}");
        assert_eq!(
            *fc.lock().unwrap(),
            [FUNCTION_START_MOTOR, FUNCTION_STOP_MOTOR]
        );
        assert_eq!(v.snapshot().frames, 2, "both ACKs came back over ep1");
    }

    /// Wait for the interpretation thread to finish.
    fn settled(v: &Vehicle) -> dji_vehicle::executor::Status {
        for _ in 0..100 {
            let s = v.executor.status();
            if !s.parsing {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("interpretation did not finish");
    }

    #[test]
    fn plan_messages_check_then_run() {
        use dji_commands::Command;
        // No model key in the test environment: the grammar does the work.
        let v = Vehicle::new();
        handle(
            r#"{"cmd":"plan","text":"arm for 2 seconds then disarm"}"#,
            &v,
            None,
        );
        let s = settled(&v);
        assert!(!s.running);
        assert_eq!(
            s.preview.map(|p| p.steps),
            Some(vec![
                Command::Arm,
                Command::Wait { seconds: 2.0 },
                Command::Disarm
            ])
        );
        handle(r#"{"cmd":"plan","text":"fly away"}"#, &v, None);
        assert!(settled(&v).preview_error.unwrap().contains("fly away"));
        handle(
            r#"{"cmd":"plan","text":"wait 1000 seconds","run":true}"#,
            &v,
            None,
        );
        let s = settled(&v);
        assert!(!s.running && s.preview_error.unwrap().contains("limit"));
        handle(
            r#"{"cmd":"plan","text":"{\"steps\":[{\"cmd\":\"wait\",\"seconds\":0.2}]}","run":true}"#,
            &v,
            None,
        );
        let s = settled(&v);
        assert!(s.running && s.source == "json");
        handle(r#"{"cmd":"cancel"}"#, &v, None);
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert_eq!(v.executor.status().message, "cancelled");
    }

    #[test]
    fn steps_sent_without_run_are_only_checked() {
        let v = Vehicle::new();
        handle(
            r#"{"cmd":"plan","text":"arm for 1 second","plan":{"steps":[{"cmd":"wait","seconds":5}]}}"#,
            &v,
            None,
        );
        let s = settled(&v);
        assert!(!s.running && s.plan.is_none(), "{s:?}");
        // The sentence was interpreted instead, and only shown.
        assert_eq!(s.preview.map(|p| p.steps.len()), Some(3));
    }

    #[test]
    fn a_refused_run_leaves_the_running_plan_as_it_is() {
        let v = Vehicle::new();
        let run = |text: &str, seconds: f32| {
            serde_json::json!({"cmd": "plan", "text": text, "run": true,
                "plan": {"steps": [{"cmd": "wait", "seconds": seconds}]}})
            .to_string()
        };
        handle(&run("first", 2.0), &v, None);
        handle(&run("second", 0.1), &v, None);
        let s = v.executor.status();
        assert!(s.running);
        assert_eq!(
            (s.text.as_str(), s.preview_error),
            ("first", None),
            "the status still describes the running plan"
        );
        handle(r#"{"cmd":"cancel"}"#, &v, None);
    }

    #[test]
    fn a_confirmed_plan_runs_as_given_without_a_second_interpretation() {
        use dji_commands::Command;
        let v = Vehicle::new();
        // The sentence would not parse, but the operator confirmed these steps.
        handle(
            r#"{"cmd":"plan","text":"whatever the model said","run":true,"plan":{"steps":[{"cmd":"wait","seconds":0.2}]}}"#,
            &v,
            None,
        );
        let s = v.executor.status();
        assert!(s.running && !s.parsing && s.source == "confirmed", "{s:?}");
        assert_eq!(s.plan.unwrap().steps, [Command::Wait { seconds: 0.2 }]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        // Over the limits: refused before anything runs.
        handle(
            r#"{"cmd":"plan","text":"long","run":true,"plan":{"steps":[{"cmd":"wait","seconds":1000}]}}"#,
            &v,
            None,
        );
        let s = v.executor.status();
        assert!(!s.running && s.preview_error.unwrap().contains("limit"));
    }
}
