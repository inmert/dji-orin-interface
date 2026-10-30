//! Runs a [`Plan`] against the vehicle: one step at a time, on its own
//! thread, reporting progress for the UI. One plan runs at a time.
//!
//! Safety: a plan is checked against [`Limits`] before it starts; a step
//! that fails, or a cancel, ends the plan, and if the plan had armed the
//! motors they are stopped first. Nothing here moves the aircraft beyond
//! what [`crate::motors`] can do today; flight steps are future variants.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_commands::{Command, Horizontal, Limits, Plan, Vertical};
use dji_common::logln;

use crate::Vehicle;
use crate::gimbal::{Camera, GimbalMove};
use crate::motors::Request;

/// Default gimbal move magnitude for plan steps, the portal joystick's default.
pub const GIMBAL_SPEED: i16 = 200;

/// How long one motor step may take (the motor path tries the RC link,
/// then the UART's activation sequence).
const MOTOR_STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Progress of the current (or last) plan, for the UI.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub running: bool,
    /// The plan being run, or the last one.
    pub plan: Option<Plan>,
    /// The sentence it came from, if any.
    pub text: String,
    /// Index of the step in progress (or where it stopped).
    pub step: usize,
    /// What happened, for the UI.
    pub message: String,
    /// Whether the last plan finished every step.
    pub ok: Option<bool>,
    /// The last plan checked but not run, and why it was refused.
    pub preview: Option<Plan>,
    pub preview_error: Option<String>,
    /// A sentence is being interpreted (a model may be answering).
    pub parsing: bool,
    /// What produced the plan or preview: a model name, `grammar` or `json`.
    pub source: String,
}

pub struct Executor {
    status: Mutex<Status>,
    cancel: AtomicBool,
    /// The latest interpretation started ([`Executor::parsing`]); an older
    /// one that finishes later is not shown.
    interpreting: AtomicU64,
    limits: Limits,
    gimbal_speed: i16,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new(Limits::default(), GIMBAL_SPEED)
    }
}

impl Executor {
    /// An executor checking plans against `limits` and driving gimbal steps
    /// at `gimbal_speed`.
    pub fn new(limits: Limits, gimbal_speed: i16) -> Self {
        Self {
            status: Mutex::default(),
            cancel: AtomicBool::new(false),
            interpreting: AtomicU64::new(0),
            limits,
            gimbal_speed,
        }
    }

    /// The status, also after a plan thread panicked holding it.
    fn lock(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn status(&self) -> Status {
        self.lock().clone()
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Note that `text` is being interpreted; [`Executor::preview`],
    /// [`Executor::interpreted`] or [`Executor::start`] ends it. Returns the
    /// interpretation's number for [`Executor::interpreted`].
    pub fn parsing(&self, text: &str) -> u64 {
        let mut s = self.lock();
        s.text = text.to_string();
        s.parsing = true;
        s.preview = None;
        s.preview_error = None;
        self.interpreting.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// [`Executor::preview`] for interpretation `id`, unless a later one was
    /// started meanwhile: an answer that arrives out of order is dropped.
    /// Whether it was shown.
    pub fn interpreted(
        &self,
        id: u64,
        text: &str,
        plan: Result<Plan, String>,
        source: &str,
    ) -> bool {
        let mut s = self.lock();
        if self.interpreting.load(Ordering::SeqCst) != id {
            return false;
        }
        Self::show(&mut s, text, plan, source);
        true
    }

    /// Record a plan for the UI to show without running it.
    pub fn preview(&self, text: &str, plan: Result<Plan, String>, source: &str) {
        Self::show(&mut self.lock(), text, plan, source);
    }

    fn show(s: &mut Status, text: &str, plan: Result<Plan, String>, source: &str) {
        s.text = text.to_string();
        s.parsing = false;
        s.source = source.to_string();
        match plan {
            Ok(p) => {
                s.preview = Some(p);
                s.preview_error = None;
            }
            Err(e) => {
                s.preview = None;
                s.preview_error = Some(e);
            }
        }
    }

    /// Start `plan` on its own thread. `Err` if one is already running or
    /// the plan is over the limits.
    pub fn start(
        &self,
        vehicle: &Arc<Vehicle>,
        text: &str,
        plan: Plan,
        source: &str,
    ) -> Result<(), String> {
        plan.validate(&self.limits).map_err(|e| e.to_string())?;
        {
            let mut s = self.lock();
            if s.running {
                return Err(ALREADY_RUNNING.into());
            }
            // Under the lock, before the plan is visible: a cancel from now
            // on is for this plan.
            self.cancel.store(false, Ordering::SeqCst);
            *s = Status {
                running: true,
                plan: Some(plan.clone()),
                text: text.to_string(),
                step: 0,
                message: "starting".into(),
                ok: None,
                preview: None,
                preview_error: None,
                parsing: false,
                source: source.to_string(),
            };
        }
        let vehicle = Arc::clone(vehicle);
        std::thread::spawn(move || {
            if std::panic::catch_unwind(AssertUnwindSafe(|| run(&vehicle, plan))).is_err() {
                // Whatever the plan had done, it can no longer stop the motors
                // itself: ask for a stop, and let a new plan start.
                let stop = vehicle.motors.request(Request::Stop);
                let ex = &vehicle.executor;
                let at = ex.status().step;
                let message = match stop {
                    Ok(()) => "the plan failed (internal error); motor stop requested",
                    Err(_) => "the plan failed (internal error)",
                };
                logln!("plan: {message}");
                ex.finish(at, false, message.into());
            }
        });
        Ok(())
    }

    /// Stop the running plan after its current step (a wait ends at once).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn progress(&self, step: usize, message: impl Into<String>) {
        let mut s = self.lock();
        s.step = step;
        s.message = message.into();
    }

    fn finish(&self, step: usize, ok: bool, message: String) {
        let mut s = self.lock();
        s.running = false;
        s.step = step;
        s.ok = Some(ok);
        s.message = message;
    }
}

/// Why [`Executor::start`] refused a plan that is within the limits.
pub const ALREADY_RUNNING: &str = "a plan is already running";

/// Why a plan stopped short.
enum Stop {
    Cancelled,
    Failed(String),
}

fn run(vehicle: &Vehicle, plan: Plan) {
    let ex = &vehicle.executor;
    logln!("plan: running: {plan}");
    // The motors may be running because of this plan: from the moment it
    // asks for a start (whose ACK can be lost while the motors do start)
    // until a stop succeeds.
    let mut armed = false;
    let mut outcome = Ok(());
    let mut at = 0;
    for (i, step) in plan.steps.iter().enumerate() {
        at = i;
        if ex.cancelled() {
            outcome = Err(Stop::Cancelled);
            break;
        }
        ex.progress(i, format!("step {}: {step}", i + 1));
        logln!("plan: step {}: {step}", i + 1);
        let result = match step {
            Command::Arm => {
                armed = true;
                vehicle
                    .motors
                    .perform(Request::Start, MOTOR_STEP_TIMEOUT)
                    .map(|_| ())
            }
            Command::Disarm => vehicle
                .motors
                .perform(Request::Stop, MOTOR_STEP_TIMEOUT)
                .map(|_| armed = false),
            Command::Wait { seconds } => wait(ex, Duration::from_secs_f32(*seconds)),
            Command::Pan {
                camera,
                direction,
                seconds,
            } => {
                let yaw = match direction {
                    Horizontal::Left => -ex.gimbal_speed,
                    Horizontal::Right => ex.gimbal_speed,
                };
                let mv = GimbalMove {
                    pitch: 0,
                    yaw,
                    active: true,
                };
                gimbal_move(vehicle, *camera, mv, *seconds)
            }
            Command::Tilt {
                camera,
                direction,
                seconds,
            } => {
                let pitch = match direction {
                    Vertical::Up => ex.gimbal_speed,
                    Vertical::Down => -ex.gimbal_speed,
                };
                let mv = GimbalMove {
                    pitch,
                    yaw: 0,
                    active: true,
                };
                gimbal_move(vehicle, *camera, mv, *seconds)
            }
            Command::Recenter { camera } => match camera.map(Camera::from_number) {
                Some(None) => Err(format!("no camera {}", camera.unwrap_or(0))),
                target => {
                    vehicle.gimbal.request_reset(target.flatten());
                    Ok(())
                }
            },
            Command::Link { on } => {
                vehicle.gimbal.set_linked(*on);
                Ok(())
            }
        };
        if let Err(e) = result {
            outcome = Err(if e.is_empty() {
                Stop::Cancelled
            } else {
                Stop::Failed(e)
            });
            break;
        }
    }
    let (ok, mut message) = match outcome {
        Ok(()) => (true, "done".to_string()),
        Err(Stop::Cancelled) => (false, "cancelled".to_string()),
        Err(Stop::Failed(e)) => (false, format!("step {} failed: {e}", at + 1)),
    };
    if !ok && armed {
        // Never leave the motors running because a plan stopped early,
        // including after a start that failed: its ACK may have been lost
        // while the motors started. A failed stop when they were already
        // off (the FC refuses a redundant stop) is not a reason to alarm.
        message += if vehicle.motors.status().armed == Some(false) {
            "; motors already stopped"
        } else {
            match vehicle.motors.perform(Request::Stop, MOTOR_STEP_TIMEOUT) {
                Ok(_) => "; motors stopped",
                Err(_) => "; MOTORS MAY BE RUNNING: stop them",
            }
        };
    }
    logln!("plan: {message}");
    ex.finish(at, ok, message);
}

/// Drive one camera for `seconds`, then release it, cancel or not.
fn gimbal_move(vehicle: &Vehicle, camera: u8, mv: GimbalMove, seconds: f32) -> Result<(), String> {
    let cam = Camera::from_number(camera).ok_or(format!("no camera {camera}"))?;
    vehicle.gimbal.apply_move(cam, mv);
    let result = wait(&vehicle.executor, Duration::from_secs_f32(seconds));
    vehicle.gimbal.apply_move(cam, GimbalMove::RELEASE);
    result
}

/// Sleep `d` in slices, ending early (with an empty error) on cancel.
fn wait(ex: &Executor, d: Duration) -> Result<(), String> {
    // A wait too long for the clock (only with absurd configured limits)
    // lasts until cancelled.
    let deadline = Instant::now()
        .checked_add(d)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400 * 365));
    while Instant::now() < deadline {
        if ex.cancelled() {
            return Err(String::new());
        }
        sleep(Duration::from_millis(100).min(deadline - Instant::now()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_run_and_cancel_ends_them_early() {
        let v = Vehicle::new();
        let plan = Plan::new(vec![
            Command::Wait { seconds: 0.2 },
            Command::Wait { seconds: 5.0 },
        ]);
        v.executor.start(&v, "wait a bit", plan, "test").unwrap();
        assert!(
            v.executor
                .start(&v, "", Plan::new(vec![Command::Disarm]), "test")
                .is_err()
        );
        sleep(Duration::from_millis(350));
        assert_eq!(v.executor.status().step, 1);
        v.executor.cancel();
        sleep(Duration::from_millis(250));
        let s = v.executor.status();
        assert!(!s.running);
        assert_eq!((s.ok, s.message.as_str()), (Some(false), "cancelled"));
    }

    #[test]
    fn gimbal_steps_drive_then_release_the_camera() {
        let v = Vehicle::new();
        let plan = Plan::new(vec![
            Command::Pan {
                camera: 2,
                direction: Horizontal::Left,
                seconds: 0.3,
            },
            Command::Link { on: true },
            Command::Recenter { camera: None },
        ]);
        v.executor.start(&v, "", plan, "test").unwrap();
        sleep(Duration::from_millis(120));
        let g = v.gimbal.lock();
        assert_eq!((g.cam2.active, g.cam2.yaw), (true, -GIMBAL_SPEED));
        drop(g);
        sleep(Duration::from_millis(400));
        let g = v.gimbal.lock();
        assert!(!g.cam2.active, "released after the move");
        assert!(g.linked);
        assert_eq!(g.reset_pending, [Camera::One]); // linked: the master only
        drop(g);
        assert_eq!(v.executor.status().ok, Some(true));
    }

    /// A vehicle whose motor requests are carried out by the real worker
    /// over the RC-link path, with a stand-in for the RC writer and the FC
    /// that answers every frame after `delay` with `answer(start)` (`None`:
    /// no ACK). Returns the starts (`true`) and stops the FC saw.
    fn with_fc(
        delay: Duration,
        answer: fn(bool) -> Option<u8>,
    ) -> (Arc<Vehicle>, Arc<Mutex<Vec<bool>>>) {
        use dji_protocol::duml::{self, Device, Header};
        use dji_protocol::flyc;
        let v = Vehicle::new();
        v.telemetry.lock().unwrap().link_up = true;
        {
            let v = Arc::clone(&v);
            std::thread::spawn(move || crate::motors::run(&v, None));
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let (v, seen) = (Arc::clone(&v), Arc::clone(&seen));
            std::thread::spawn(move || {
                let mut seq = 0u16;
                loop {
                    sleep(Duration::from_millis(5));
                    let Some(frame) = v.motors.take_rc_pending() else {
                        continue;
                    };
                    seq += 1;
                    v.motors.rc_sent(frame, seq);
                    seen.lock().unwrap().push(frame.on);
                    sleep(delay);
                    let Some(code) = answer(frame.on) else {
                        continue;
                    };
                    let h = Header {
                        sender: duml::addr(Device::FlightCtrl, 0),
                        receiver: duml::addr(Device::App, 0),
                        seq,
                        cmd_type: 0x80,
                        cmd_set: flyc::CMD_SET,
                        cmd_id: flyc::CMD_ID_FUNCTION_CONTROL,
                    };
                    let ack = duml::build(&h, &[code]).unwrap();
                    v.motors.deliver_duml_ack(&duml::parse(&ack).unwrap().0);
                }
            });
        }
        (v, seen)
    }

    /// An FC that accepts everything after `delay`.
    fn with_fake_fc(delay: Duration) -> (Arc<Vehicle>, Arc<Mutex<Vec<bool>>>) {
        with_fc(delay, |_| Some(0x00))
    }

    /// The executor's status once the plan has ended.
    fn ended(v: &Vehicle) -> Status {
        for _ in 0..200 {
            let s = v.executor.status();
            if !s.running {
                return s;
            }
            sleep(Duration::from_millis(25));
        }
        panic!("the plan did not end");
    }

    #[test]
    fn a_timed_arm_starts_then_stops_the_motors() {
        let (v, seen) = with_fake_fc(Duration::ZERO);
        let plan = Plan::new(vec![
            Command::Arm,
            Command::Wait { seconds: 0.2 },
            Command::Disarm,
        ]);
        v.executor.start(&v, "arm for 0.2 s", plan, "test").unwrap();
        let s = ended(&v);
        assert_eq!((s.ok, s.message.as_str(), s.step), (Some(true), "done", 2));
        assert_eq!(*seen.lock().unwrap(), [true, false]);
        assert_eq!(v.motors.status().ok, Some(true));
    }

    #[test]
    fn a_cancel_during_a_wait_stops_the_motors() {
        let (v, seen) = with_fake_fc(Duration::ZERO);
        let plan = Plan::new(vec![
            Command::Arm,
            Command::Wait { seconds: 10.0 },
            Command::Disarm,
        ]);
        v.executor.start(&v, "", plan, "test").unwrap();
        for _ in 0..100 {
            if v.executor.status().step == 1 {
                break;
            }
            sleep(Duration::from_millis(20));
        }
        v.executor.cancel();
        let s = ended(&v);
        assert_eq!(
            (s.ok, s.message.as_str()),
            (Some(false), "cancelled; motors stopped")
        );
        assert_eq!(*seen.lock().unwrap(), [true, false]);
    }

    #[test]
    fn a_cancel_during_a_motor_step_lets_it_finish_then_stops() {
        // The FC takes 300 ms to answer the start; the cancel comes first.
        let (v, seen) = with_fake_fc(Duration::from_millis(300));
        let plan = Plan::new(vec![
            Command::Arm,
            Command::Wait { seconds: 10.0 },
            Command::Disarm,
        ]);
        v.executor.start(&v, "", plan, "test").unwrap();
        sleep(Duration::from_millis(100));
        v.executor.cancel();
        let s = ended(&v);
        assert_eq!(
            (s.ok, s.message.as_str(), s.step),
            (Some(false), "cancelled; motors stopped", 1)
        );
        assert_eq!(*seen.lock().unwrap(), [true, false]);
    }

    #[test]
    fn a_plan_over_the_limits_never_starts() {
        let v = Vehicle::new();
        let plan = Plan::new(vec![Command::Wait { seconds: 1e9 }]);
        assert!(
            v.executor
                .start(&v, "", plan, "test")
                .unwrap_err()
                .contains("limit")
        );
        assert!(!v.executor.status().running);
    }

    #[test]
    fn a_failed_step_reports_where_it_stopped_and_stops_the_motors() {
        let v = Vehicle::new();
        v.executor.parsing("arm");
        assert!(v.executor.status().parsing);
        v.executor
            .preview("arm", Ok(Plan::new(vec![Command::Arm])), "grammar");
        let s = v.executor.status();
        assert!(s.preview.is_some() && !s.parsing && s.source == "grammar");
        // The FC refuses the start and accepts the stop.
        let (v, seen) = with_fc(Duration::ZERO, |start| Some(u8::from(start)));
        let plan = Plan::new(vec![
            Command::Wait { seconds: 0.1 },
            Command::Arm,
            Command::Wait { seconds: 1.0 },
            Command::Disarm,
        ]);
        v.executor.start(&v, "", plan, "test").unwrap();
        let s = ended(&v);
        assert_eq!(s.ok, Some(false));
        assert!(s.message.starts_with("step 2 failed"), "{}", s.message);
        assert!(s.message.ends_with("; motors stopped"), "{}", s.message);
        assert_eq!(*seen.lock().unwrap(), [true, false]);
        assert!(s.preview.is_none(), "a run clears the preview");
    }

    #[test]
    fn a_start_whose_acks_are_lost_is_still_stopped() {
        // The FC starts the motors but none of its ACKs arrive; with no
        // UART path the step fails. The plan must not walk away from
        // running motors.
        let (v, seen) = with_fc(Duration::ZERO, |start| (!start).then_some(0x00));
        let plan = Plan::new(vec![
            Command::Arm,
            Command::Wait { seconds: 5.0 },
            Command::Disarm,
        ]);
        v.executor.start(&v, "", plan, "test").unwrap();
        let s = ended(&v);
        assert!(s.message.starts_with("step 1 failed"), "{}", s.message);
        assert!(s.message.ends_with("; motors stopped"), "{}", s.message);
        assert_eq!(seen.lock().unwrap().last(), Some(&false));
    }

    #[test]
    fn a_plan_that_panics_ends_and_asks_for_a_stop() {
        let v = Vehicle::new();
        // Poison the gimbal state: the pan step panics on it.
        let g = Arc::clone(&v);
        let _ = std::thread::spawn(move || {
            let _held = g.gimbal.lock();
            panic!("poison");
        })
        .join();
        let plan = Plan::new(vec![Command::Pan {
            camera: 1,
            direction: Horizontal::Left,
            seconds: 0.1,
        }]);
        v.executor.start(&v, "", plan, "test").unwrap();
        let s = ended(&v);
        assert_eq!(s.ok, Some(false));
        assert!(s.message.contains("internal error"), "{}", s.message);
        assert_eq!(v.motors.lock().pending, Some(Request::Stop));
        // The executor takes plans again.
        let next = Plan::new(vec![Command::Wait { seconds: 0.01 }]);
        assert!(v.executor.start(&v, "", next, "test").is_ok());
    }

    #[test]
    fn an_interpretation_that_finishes_late_is_not_shown() {
        let ex = Executor::default();
        let first = ex.parsing("pan left");
        let second = ex.parsing("tilt up");
        let plan = |c| Ok(Plan::new(vec![c]));
        assert!(!ex.interpreted(first, "pan left", plan(Command::Disarm), "m"));
        let s = ex.status();
        assert!(s.parsing && s.text == "tilt up" && s.preview.is_none());
        assert!(ex.interpreted(second, "tilt up", plan(Command::Disarm), "m"));
        assert!(!ex.status().parsing);
    }
}
