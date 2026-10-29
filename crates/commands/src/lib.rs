//! The aircraft command model: what a caller (the portal, a voice front end,
//! an LLM) may ask the aircraft to do, independent of how it is carried out.
//!
//! * [`Command`] — one step: arm, disarm, wait, and the gimbal moves (pan,
//!   tilt, recenter, link). Flight steps (take off, land, hover, move) slot
//!   in as further variants once the vehicle can carry them out.
//! * [`Plan`] — an ordered list of steps with a check against [`Limits`],
//!   and the JSON shape (`{"steps":[{"cmd":"arm"},{"cmd":"wait","seconds":5},…]}`)
//!   a structured-output model is asked to produce.
//! * [`grammar`] — a deterministic parser for plain sentences such as
//!   `arm for 5 seconds then disarm`, tried before any model.
//!
//! Pure logic, no I/O: the vehicle's executor runs a plan.

pub mod grammar;

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// One step of a plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Command {
    /// Start all motors (props spin).
    Arm,
    /// Stop all motors.
    Disarm,
    /// Do nothing for a while (the motors stay as they are).
    Wait { seconds: f32 },
    /// Pan a camera's gimbal (yaw) for a while at the standard speed.
    Pan {
        camera: u8,
        direction: Horizontal,
        seconds: f32,
    },
    /// Tilt a camera's gimbal (pitch) for a while at the standard speed.
    Tilt {
        camera: u8,
        direction: Vertical,
        seconds: f32,
    },
    /// Return a camera's yaw to centre (`None`: both cameras).
    Recenter { camera: Option<u8> },
    /// Engage (`true`) or release the native camera lock (camera 2 follows 1).
    Link { on: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Horizontal {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vertical {
    Up,
    Down,
}

impl Command {
    /// A timed step's duration.
    fn seconds(&self) -> Option<f32> {
        match self {
            Command::Wait { seconds }
            | Command::Pan { seconds, .. }
            | Command::Tilt { seconds, .. } => Some(*seconds),
            _ => None,
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Arm => f.write_str("arm"),
            Command::Disarm => f.write_str("disarm"),
            Command::Wait { seconds } => write!(f, "wait {seconds} s"),
            Command::Pan {
                camera,
                direction,
                seconds,
            } => write!(f, "pan camera {camera} {direction:?} {seconds} s"),
            Command::Tilt {
                camera,
                direction,
                seconds,
            } => write!(f, "tilt camera {camera} {direction:?} {seconds} s"),
            Command::Recenter { camera: Some(c) } => write!(f, "recenter camera {c}"),
            Command::Recenter { camera: None } => f.write_str("recenter both cameras"),
            Command::Link { on: true } => f.write_str("link cameras"),
            Command::Link { on: false } => f.write_str("unlink cameras"),
        }
    }
}

/// What a plan may ask for. Any caller's plan is checked against these
/// before it runs, whatever produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// Longest single wait.
    pub max_wait: Duration,
    /// Longest the motors may run within one plan before it disarms.
    pub max_armed: Duration,
    /// Longest single gimbal move.
    pub max_gimbal: Duration,
    pub max_steps: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_wait: Duration::from_secs(60),
            max_armed: Duration::from_secs(60),
            max_gimbal: Duration::from_secs(10),
            max_steps: 32,
        }
    }
}

/// Why a plan was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invalid {
    Empty,
    TooManySteps(usize),
    /// A duration is not a finite, positive number of seconds.
    BadWait,
    WaitTooLong {
        seconds: u32,
        max: u32,
    },
    GimbalTooLong {
        seconds: u32,
        max: u32,
    },
    ArmedTooLong {
        seconds: u32,
        max: u32,
    },
    /// A camera number other than 1 or 2.
    BadCamera(u8),
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Invalid::Empty => f.write_str("the plan has no steps"),
            Invalid::TooManySteps(max) => write!(f, "more than {max} steps"),
            Invalid::BadWait => f.write_str("a duration must be a positive number of seconds"),
            Invalid::WaitTooLong { seconds, max } => {
                write!(f, "a {seconds} s wait is over the {max} s limit")
            }
            Invalid::GimbalTooLong { seconds, max } => {
                write!(f, "a {seconds} s gimbal move is over the {max} s limit")
            }
            Invalid::ArmedTooLong { seconds, max } => {
                write!(f, "motors would run {seconds} s, over the {max} s limit")
            }
            Invalid::BadCamera(c) => write!(f, "there is no camera {c} (1 or 2)"),
        }
    }
}

impl std::error::Error for Invalid {}

/// An ordered list of steps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<Command>,
}

impl Plan {
    pub fn new(steps: Vec<Command>) -> Self {
        Self { steps }
    }

    /// Check the plan against `limits`.
    pub fn validate(&self, limits: &Limits) -> Result<(), Invalid> {
        if self.steps.is_empty() {
            return Err(Invalid::Empty);
        }
        if self.steps.len() > limits.max_steps {
            return Err(Invalid::TooManySteps(limits.max_steps));
        }
        let mut armed = false;
        let mut armed_for = 0f32;
        for step in &self.steps {
            if let Some(seconds) = step.seconds() {
                if !(seconds.is_finite() && seconds > 0.0) {
                    return Err(Invalid::BadWait);
                }
                let is_wait = matches!(step, Command::Wait { .. });
                let max = if is_wait {
                    limits.max_wait
                } else {
                    limits.max_gimbal
                };
                if seconds > max.as_secs_f32() {
                    let (seconds, max) = (seconds.ceil() as u32, max.as_secs() as u32);
                    return Err(if is_wait {
                        Invalid::WaitTooLong { seconds, max }
                    } else {
                        Invalid::GimbalTooLong { seconds, max }
                    });
                }
                if armed {
                    armed_for += seconds;
                    if armed_for > limits.max_armed.as_secs_f32() {
                        return Err(Invalid::ArmedTooLong {
                            seconds: armed_for.ceil() as u32,
                            max: limits.max_armed.as_secs() as u32,
                        });
                    }
                }
            }
            match step {
                Command::Arm => armed = true,
                Command::Disarm => {
                    armed = false;
                    armed_for = 0.0;
                }
                Command::Pan { camera, .. } | Command::Tilt { camera, .. }
                    if !(1..=2).contains(camera) =>
                {
                    return Err(Invalid::BadCamera(*camera));
                }
                Command::Recenter { camera: Some(c) } if !(1..=2).contains(c) => {
                    return Err(Invalid::BadCamera(*c));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether the plan ends with the motors running.
    pub fn ends_armed(&self) -> bool {
        self.steps
            .iter()
            .rev()
            .find_map(|s| match s {
                Command::Arm => Some(true),
                Command::Disarm => Some(false),
                _ => None,
            })
            .unwrap_or(false)
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, step) in self.steps.iter().enumerate() {
            if i > 0 {
                f.write_str(", then ")?;
            }
            write!(f, "{step}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(steps: &[Command]) -> Plan {
        Plan::new(steps.to_vec())
    }

    #[test]
    fn limits_bound_waits_and_armed_time() {
        let l = Limits::default();
        assert_eq!(plan(&[]).validate(&l), Err(Invalid::Empty));
        assert_eq!(
            plan(&[Command::Wait { seconds: 0.0 }]).validate(&l),
            Err(Invalid::BadWait)
        );
        assert_eq!(
            plan(&[Command::Wait { seconds: f32::NAN }]).validate(&l),
            Err(Invalid::BadWait)
        );
        assert_eq!(
            plan(&[Command::Wait { seconds: 61.0 }]).validate(&l),
            Err(Invalid::WaitTooLong {
                seconds: 61,
                max: 60
            })
        );
        let armed_61 = [
            Command::Arm,
            Command::Wait { seconds: 30.0 },
            Command::Wait { seconds: 31.0 },
            Command::Disarm,
        ];
        assert_eq!(
            plan(&armed_61).validate(&l),
            Err(Invalid::ArmedTooLong {
                seconds: 61,
                max: 60
            })
        );
        // Disarming resets the armed clock.
        let ok = [
            Command::Arm,
            Command::Wait { seconds: 40.0 },
            Command::Disarm,
            Command::Wait { seconds: 2.0 },
            Command::Arm,
            Command::Wait { seconds: 40.0 },
            Command::Disarm,
        ];
        assert_eq!(plan(&ok).validate(&l), Ok(()));
        assert!(!plan(&ok).ends_armed());
        assert!(plan(&[Command::Arm, Command::Wait { seconds: 1.0 }]).ends_armed());
        assert_eq!(
            plan(&vec![Command::Disarm; 33]).validate(&l),
            Err(Invalid::TooManySteps(32))
        );
    }

    #[test]
    fn gimbal_steps_have_their_own_limits() {
        let l = Limits::default();
        let pan = |camera, seconds| Command::Pan {
            camera,
            direction: Horizontal::Left,
            seconds,
        };
        assert_eq!(plan(&[pan(1, 10.0)]).validate(&l), Ok(()));
        assert_eq!(
            plan(&[pan(2, 11.0)]).validate(&l),
            Err(Invalid::GimbalTooLong {
                seconds: 11,
                max: 10
            })
        );
        assert_eq!(
            plan(&[pan(3, 1.0)]).validate(&l),
            Err(Invalid::BadCamera(3))
        );
        assert_eq!(
            plan(&[Command::Recenter { camera: Some(0) }]).validate(&l),
            Err(Invalid::BadCamera(0))
        );
        // Gimbal time while armed counts as armed time too.
        assert_eq!(
            plan(&[Command::Arm, pan(1, 10.0), Command::Wait { seconds: 51.0 }]).validate(&l),
            Err(Invalid::ArmedTooLong {
                seconds: 61,
                max: 60
            })
        );
    }

    #[test]
    fn json_shape_round_trips() {
        let p = plan(&[
            Command::Arm,
            Command::Wait { seconds: 5.0 },
            Command::Disarm,
        ]);
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(
            json,
            r#"{"steps":[{"cmd":"arm"},{"cmd":"wait","seconds":5.0},{"cmd":"disarm"}]}"#
        );
        assert_eq!(serde_json::from_str::<Plan>(&json).unwrap(), p);
        assert!(serde_json::from_str::<Plan>(r#"{"steps":[{"cmd":"fly"}]}"#).is_err());
        assert_eq!(p.to_string(), "arm, then wait 5 s, then disarm");
        let g = plan(&[
            Command::Pan {
                camera: 2,
                direction: Horizontal::Right,
                seconds: 1.5,
            },
            Command::Recenter { camera: None },
            Command::Link { on: true },
        ]);
        assert_eq!(
            serde_json::to_string(&g).unwrap(),
            r#"{"steps":[{"cmd":"pan","camera":2,"direction":"right","seconds":1.5},{"cmd":"recenter","camera":null},{"cmd":"link","on":true}]}"#
        );
        assert_eq!(
            g.to_string(),
            "pan camera 2 Right 1.5 s, then recenter both cameras, then link cameras"
        );
    }
}
