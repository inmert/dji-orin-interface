//! Understanding: turning a sentence (typed or transcribed) into a
//! [`Plan`]. An online model does it first ([`Provider`]: [`anthropic`],
//! [`openai`]), since it copes with phrasing the local grammar does not;
//! the grammar ([`dji_commands::grammar`]) is the fallback when no model is
//! configured or the call fails. Either way the Orin validates the plan
//! against its limits before anything runs, and a plan typed as JSON
//! (`{"steps":[…]}`) skips both.
//!
//! The model is asked for the plan as structured output against
//! [`schema`], one flat step shape (such schemas allow no recursion), which
//! [`plan_from_json`] turns into a [`Plan`]. Keys come from
//! [`crate::online`]; `LLM_PROVIDER` picks the provider, by default the
//! first configured of `anthropic`, `openai`; `LLM_MODEL` the model.

pub mod anthropic;
pub mod openai;

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;

use dji_commands::{Command, Horizontal, Plan, Vertical, grammar};
use dji_common::logln;

/// How long a status check (key, spend) is reused before asking again.
const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// What the UI shows about one provider.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub provider: &'static str,
    /// The one understanding uses.
    pub active: bool,
    pub model: String,
    /// Whether the service accepted the key; `None` when it could not be asked.
    pub key_ok: Option<bool>,
    /// Spend or balance, when the service exposes it (e.g. `$1.23 this month`).
    pub balance: Option<String>,
    /// Anything else worth knowing (errors, what balance would need).
    pub note: String,
}

/// One online model.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;
    /// The plan for `text`, or why there is none (not a drone command,
    /// ambiguous, unsafe, or the service failed).
    fn interpret(&self, text: &str) -> Result<Plan, String>;
    /// Check the key and, where offered, spend or balance.
    fn status(&self) -> Status;
}

/// A plan and where it came from (`claude-opus-5-5`, `grammar`, `json`).
pub struct Interpreted {
    pub plan: Plan,
    pub source: String,
}

struct Registry {
    providers: Vec<Box<dyn Provider>>,
    active: Option<usize>,
    status_cache: Mutex<Option<(Instant, Vec<Status>)>>,
}

impl Registry {
    fn from_env() -> Self {
        let keys = crate::online::keys();
        let mut providers: Vec<Box<dyn Provider>> = Vec::new();
        if let Some(k) = keys.anthropic.clone() {
            providers.push(Box::new(anthropic::Anthropic::new(
                k,
                keys.anthropic_admin.clone(),
            )));
        }
        if let Some(k) = keys.openai.clone() {
            providers.push(Box::new(openai::OpenAi::new(k)));
        }
        let wanted = std::env::var("LLM_PROVIDER").ok();
        let active = providers
            .iter()
            .position(|p| wanted.as_deref().is_none_or(|w| w == p.name()));
        if let (Some(w), None) = (&wanted, active) {
            logln!("understand: LLM_PROVIDER={w} is not configured (no key)");
        }
        logln!(
            "understand: model providers [{}]; using {}",
            providers
                .iter()
                .map(|p| format!("{} {}", p.name(), p.model()))
                .collect::<Vec<_>>()
                .join(", "),
            active.map_or("the local grammar only", |i| providers[i].name())
        );
        Self {
            providers,
            active,
            status_cache: Mutex::new(None),
        }
    }

    fn active(&self) -> Option<&dyn Provider> {
        self.active.map(|i| self.providers[i].as_ref())
    }

    fn statuses(&self) -> Vec<Status> {
        let mut cache = self.status_cache.lock().unwrap();
        if let Some((at, s)) = cache.as_ref()
            && at.elapsed() < STATUS_TTL
        {
            return s.clone();
        }
        let statuses: Vec<Status> = self
            .providers
            .iter()
            .enumerate()
            .map(|(i, p)| Status {
                active: self.active == Some(i),
                ..p.status()
            })
            .collect();
        *cache = Some((Instant::now(), statuses.clone()));
        statuses
    }
}

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Registry::from_env)
}

/// Status of every configured provider (`GET /ai/status`).
pub fn statuses() -> Vec<Status> {
    registry().statuses()
}

/// The plan for `text`: JSON as given, else the model, else the grammar.
/// The plan is **not** yet checked against the limits.
pub fn interpret(text: &str) -> Result<Interpreted, String> {
    let text = text.trim();
    if text.starts_with('{') {
        let plan = serde_json::from_str::<Plan>(text).map_err(|e| format!("bad plan JSON: {e}"))?;
        return Ok(Interpreted {
            plan,
            source: "json".into(),
        });
    }
    let model_error = match registry().active() {
        None => None,
        Some(p) => {
            let start = Instant::now();
            match p.interpret(text) {
                Ok(plan) => {
                    logln!(
                        "understand: {} {} in {} ms: {plan}",
                        p.name(),
                        p.model(),
                        start.elapsed().as_millis()
                    );
                    return Ok(Interpreted {
                        plan,
                        source: p.model().to_string(),
                    });
                }
                Err(e) => {
                    logln!("understand: {} failed: {e}; trying the grammar", p.name());
                    Some(e)
                }
            }
        }
    };
    match grammar::parse(text) {
        Ok(plan) => Ok(Interpreted {
            plan,
            source: "grammar".into(),
        }),
        Err(g) => Err(match model_error {
            Some(m) => format!("{m} (local grammar: {g})"),
            None => g.to_string(),
        }),
    }
}

/// What the models are told about the aircraft and the command set.
pub const INSTRUCTIONS: &str = "You translate spoken or typed instructions for a DJI Matrice 210 \
drone into a list of steps. The only steps are: arm (start all motors), disarm (stop all motors), \
wait (seconds), pan (gimbal yaw: camera 1 or 2, direction left or right, seconds), tilt (gimbal \
pitch: camera 1 or 2, direction up or down, seconds), recenter (camera 1 or 2, or null for both), \
link (on: true engages the lock so camera 2 follows camera 1, false releases it). Camera defaults to \
1 and a gimbal move defaults to 1 second when not stated. 'arm for N seconds' means arm, wait N, \
disarm; do not add a disarm when the motors are already stopped at that point. Flight (take off, \
land, hover, move the aircraft) is not available: never invent steps for it. Fill every field; use \
null for fields a step does not use. If the instruction is not one of these commands, is ambiguous, \
or asks for something unsafe, return an empty steps list and say why in problem; otherwise problem \
is null.";

/// The JSON schema the models fill: one flat step shape.
pub fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "cmd": {"type": "string", "enum": ["arm", "disarm", "wait", "pan", "tilt", "recenter", "link"]},
                        "seconds": {"type": ["number", "null"]},
                        "camera": {"type": ["integer", "null"]},
                        "direction": {"anyOf": [
                            {"type": "string", "enum": ["left", "right", "up", "down"]},
                            {"type": "null"}
                        ]},
                        "on": {"type": ["boolean", "null"]}
                    },
                    "required": ["cmd", "seconds", "camera", "direction", "on"],
                    "additionalProperties": false
                }
            },
            "problem": {"type": ["string", "null"]}
        },
        "required": ["steps", "problem"],
        "additionalProperties": false
    })
}

/// A [`Plan`] from the JSON the schema produces.
pub fn plan_from_json(text: &str) -> Result<Plan, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("model returned invalid JSON: {e}"))?;
    if let Some(problem) = v["problem"].as_str().filter(|p| !p.is_empty()) {
        return Err(problem.to_string());
    }
    let steps = v["steps"].as_array().ok_or("model returned no steps")?;
    let mut plan = Vec::with_capacity(steps.len());
    for s in steps {
        let seconds = s["seconds"].as_f64().map(|x| x as f32);
        let camera = s["camera"].as_u64().map(|c| c as u8).unwrap_or(1);
        let direction = s["direction"].as_str().unwrap_or("");
        plan.push(match s["cmd"].as_str().unwrap_or("") {
            "arm" => Command::Arm,
            "disarm" => Command::Disarm,
            "wait" => Command::Wait {
                seconds: seconds.ok_or("a wait step without seconds")?,
            },
            "pan" => Command::Pan {
                camera,
                direction: match direction {
                    "left" => Horizontal::Left,
                    "right" => Horizontal::Right,
                    d => return Err(format!("pan direction {d:?} is not left or right")),
                },
                seconds: seconds.unwrap_or(1.0),
            },
            "tilt" => Command::Tilt {
                camera,
                direction: match direction {
                    "up" => Vertical::Up,
                    "down" => Vertical::Down,
                    d => return Err(format!("tilt direction {d:?} is not up or down")),
                },
                seconds: seconds.unwrap_or(1.0),
            },
            "recenter" => Command::Recenter {
                camera: s["camera"].as_u64().map(|c| c as u8),
            },
            "link" => Command::Link {
                on: s["on"].as_bool().ok_or("a link step without on")?,
            },
            other => return Err(format!("unknown step {other:?}")),
        });
    }
    if plan.is_empty() {
        return Err("the model found nothing to do".into());
    }
    Ok(Plan::new(plan))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_model_json_into_a_plan() {
        let json = r#"{"steps":[
            {"cmd":"arm","seconds":null,"camera":null,"direction":null,"on":null},
            {"cmd":"wait","seconds":3,"camera":null,"direction":null,"on":null},
            {"cmd":"pan","seconds":null,"camera":2,"direction":"left","on":null},
            {"cmd":"recenter","seconds":null,"camera":null,"direction":null,"on":null},
            {"cmd":"link","seconds":null,"camera":null,"direction":null,"on":false},
            {"cmd":"disarm","seconds":null,"camera":null,"direction":null,"on":null}],
            "problem":null}"#;
        let plan = plan_from_json(json).unwrap();
        assert_eq!(
            plan.steps,
            [
                Command::Arm,
                Command::Wait { seconds: 3.0 },
                Command::Pan {
                    camera: 2,
                    direction: Horizontal::Left,
                    seconds: 1.0
                },
                Command::Recenter { camera: None },
                Command::Link { on: false },
                Command::Disarm,
            ]
        );
        assert_eq!(
            plan_from_json(r#"{"steps":[],"problem":"take-off is not available"}"#).unwrap_err(),
            "take-off is not available"
        );
        assert!(plan_from_json(r#"{"steps":[{"cmd":"pan","seconds":1,"camera":1,"direction":"up","on":null}],"problem":null}"#).is_err());
        assert!(plan_from_json("nonsense").is_err());
        assert!(schema()["properties"]["steps"].is_object());
    }

    #[test]
    fn json_and_grammar_paths_need_no_model() {
        // No keys in the test environment: JSON is taken as given, else the grammar.
        let i = interpret(r#"{"steps":[{"cmd":"disarm"}]}"#).unwrap();
        assert_eq!(
            (i.plan.steps, i.source.as_str()),
            (vec![Command::Disarm], "json")
        );
        assert!(interpret("{not json").is_err());
        let i = interpret("arm for 2 seconds").unwrap();
        assert_eq!(
            i.source,
            if registry().active().is_some() {
                i.source.clone()
            } else {
                "grammar".into()
            }
        );
        assert_eq!(i.plan.steps.len(), 3);
    }
}
