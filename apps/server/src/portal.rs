//! The portal's command messages (mirrors `Command` in web/src/api.ts),
//! applied to the [`Vehicle`]. This is the only place the web wire format
//! meets the vehicle's typed operations.

use std::sync::Arc;

use serde::Deserialize;

use dji_commands::{Plan, grammar};
use dji_common::logln;
use dji_vehicle::Vehicle;
use dji_vehicle::executor::Executor;
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
    /// (`{"steps":[…]}`): checked and shown back (`run` false), or run.
    Plan {
        text: String,
        #[serde(default)]
        run: bool,
    },
    /// Stop the running plan after its current step.
    Cancel,
}

/// A plan from a sentence (the grammar) or from the JSON shape, checked
/// against the executor's limits.
fn plan_from(text: &str, executor: &Executor) -> Result<Plan, String> {
    let text = text.trim();
    let plan = if text.starts_with('{') {
        serde_json::from_str::<Plan>(text).map_err(|e| format!("bad plan JSON: {e}"))?
    } else {
        grammar::parse(text).map_err(|e| e.to_string())?
    };
    plan.validate(executor.limits())
        .map_err(|e| e.to_string())?;
    Ok(plan)
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
        Ok(WsCommand::Plan { text, run }) => {
            let ex = &vehicle.executor;
            let plan = plan_from(&text, ex);
            if !run {
                logln!("plan: checked {text:?}: {plan:?}");
                return ex.preview(&text, plan);
            }
            match plan.and_then(|p| ex.start(vehicle, &text, p)) {
                Ok(()) => logln!("plan: started {text:?}"),
                Err(e) => {
                    logln!("plan: refused {text:?}: {e}");
                    ex.preview(&text, Err(e));
                }
            }
        }
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
    fn plan_messages_check_then_run() {
        use dji_commands::Command;
        let v = Vehicle::new();
        handle(
            r#"{"cmd":"plan","text":"arm for 2 seconds then disarm"}"#,
            &v,
            None,
        );
        let s = v.executor.status();
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
        assert!(
            v.executor
                .status()
                .preview_error
                .unwrap()
                .contains("fly away")
        );
        handle(
            r#"{"cmd":"plan","text":"wait 1000 seconds","run":true}"#,
            &v,
            None,
        );
        let s = v.executor.status();
        assert!(!s.running && s.preview_error.unwrap().contains("limit"));
        handle(
            r#"{"cmd":"plan","text":"{\"steps\":[{\"cmd\":\"wait\",\"seconds\":0.2}]}","run":true}"#,
            &v,
            None,
        );
        assert!(v.executor.status().running);
        handle(r#"{"cmd":"cancel"}"#, &v, None);
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert_eq!(v.executor.status().message, "cancelled");
    }
}
