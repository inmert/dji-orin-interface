//! The aircraft command model: what a caller (the portal, a voice front end,
//! an LLM) may ask the aircraft to do, independent of how it is carried out.
//!
//! * [`Command`] — one step. Today: arm, disarm, wait. Flight steps (take
//!   off, land, hover, move) slot in as further variants once the vehicle
//!   can carry them out.
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
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Arm => f.write_str("arm"),
            Command::Disarm => f.write_str("disarm"),
            Command::Wait { seconds } => write!(f, "wait {seconds} s"),
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
    pub max_steps: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_wait: Duration::from_secs(60),
            max_armed: Duration::from_secs(60),
            max_steps: 32,
        }
    }
}

/// Why a plan was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invalid {
    Empty,
    TooManySteps(usize),
    /// A wait is not a finite, positive number of seconds.
    BadWait,
    WaitTooLong {
        seconds: u32,
        max: u32,
    },
    ArmedTooLong {
        seconds: u32,
        max: u32,
    },
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Invalid::Empty => f.write_str("the plan has no steps"),
            Invalid::TooManySteps(max) => write!(f, "more than {max} steps"),
            Invalid::BadWait => f.write_str("a wait must be a positive number of seconds"),
            Invalid::WaitTooLong { seconds, max } => {
                write!(f, "a {seconds} s wait is over the {max} s limit")
            }
            Invalid::ArmedTooLong { seconds, max } => {
                write!(f, "motors would run {seconds} s, over the {max} s limit")
            }
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
            match step {
                Command::Arm => armed = true,
                Command::Disarm => {
                    armed = false;
                    armed_for = 0.0;
                }
                Command::Wait { seconds } => {
                    if !(seconds.is_finite() && *seconds > 0.0) {
                        return Err(Invalid::BadWait);
                    }
                    if *seconds > limits.max_wait.as_secs_f32() {
                        return Err(Invalid::WaitTooLong {
                            seconds: seconds.ceil() as u32,
                            max: limits.max_wait.as_secs() as u32,
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
                Command::Wait { .. } => None,
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
    }
}
