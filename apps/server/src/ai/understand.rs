//! Understanding: turning a sentence (typed or transcribed) into a
//! [`Plan`]. A model does it first ([`Provider`]: [`anthropic`],
//! [`openai`] online, or [`local`], a model served on the Orin), since it
//! copes with phrasing the local grammar does not; the grammar
//! ([`dji_commands::grammar`]) is the fallback when no model is configured
//! or the call fails. Either way the Orin validates the plan
//! against its limits before anything runs, and a plan typed as JSON
//! (`{"steps":[…]}`) skips both.
//!
//! The model is asked for the plan as structured output against
//! [`schema`], one flat step shape (such schemas allow no recursion), which
//! [`plan_from_json`] turns into a [`Plan`]. Keys come from
//! [`super::online`]; `config.toml` `[understanding]` (`LLM_PROVIDER`,
//! `LLM_MODEL`) picks the provider, by default the first configured of
//! `anthropic`, `openai`, `local`, and the online model. The local provider
//! is configured under `[understanding.local]` (`LOCAL_LLM_URL`) and is
//! last unless chosen: the online model was the more accurate on the test
//! sentences (see the README).

pub mod anthropic;
#[cfg(test)]
mod bench;
pub mod local;
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

/// A plan and where it came from (`claude-opus-5-5`, a GGUF's name,
/// `grammar`, `json`).
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
    fn new(cfg: &crate::config::Understanding) -> Self {
        let keys = super::online::keys();
        let wanted = cfg.provider();
        let mut providers: Vec<Box<dyn Provider>> = Vec::new();
        // `model` names a model of the provider in use: the one chosen, or
        // the first configured. The others keep their defaults.
        let model_for =
            |name: &str, first: bool| cfg.model().filter(|_| wanted.map_or(first, |w| w == name));
        if let Some(k) = keys.anthropic.clone() {
            let model = model_for("anthropic", providers.is_empty());
            providers.push(Box::new(anthropic::Anthropic::new(
                k,
                keys.anthropic_admin.clone(),
                model,
            )));
        }
        if let Some(k) = keys.openai.clone() {
            let model = model_for("openai", providers.is_empty());
            providers.push(Box::new(openai::OpenAi::new(k, model)));
        }
        if cfg.local.enabled {
            providers.push(Box::new(local::Local::new(
                cfg.local.url.clone(),
                cfg.local.model().map(String::from),
            )));
        }
        let active = providers
            .iter()
            .position(|p| wanted.is_none_or(|w| w == p.name()));
        if let (Some(w), None) = (wanted, active) {
            logln!("understand: provider {w} is not configured (no key, or not a provider)");
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

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Choose the providers from the settings, once, at start (after
/// [`super::online::init`]).
pub fn init(cfg: &crate::config::Understanding) {
    let _ = REGISTRY.set(Registry::new(cfg));
}

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| Registry::new(&crate::config::Understanding::default()))
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
is null. Examples: 'arm for 5 seconds then disarm' is arm, wait 5, disarm. 'start the motors, hold \
for ten seconds, then stop them' is arm, wait 10, disarm. 'look up for three seconds on camera 2' is \
tilt camera 2 up 3. 'unlink and pan right for two and a half seconds' is link off, then pan camera 1 \
right 2.5. 'recenter' alone is recenter with camera null.";

/// The JSON schema the models fill: one flat step shape. Every field of a
/// step is required (OpenAI's strict mode insists), so unused ones are null.
///
/// Key order matters: `serde_json` is built with `preserve_order` so the
/// schema reaches the model as written, `steps` before `problem` and `cmd`
/// first in a step. Sorted alphabetically (`camera, cmd, …`, `problem`
/// first) Claude stopped after the first step on most multi-step sentences
/// and the local model attached durations to the wrong step.
pub fn schema() -> serde_json::Value {
    let mut s = schema_compact();
    s["properties"]["steps"]["items"]["required"] =
        serde_json::json!(["cmd", "seconds", "camera", "direction", "on"]);
    s
}

/// [`schema`] with only `cmd` required per step, for a server that turns
/// the schema into a grammar ([`local`]): a step is then a few tokens
/// instead of five nulls, which is most of a small model's output time.
/// [`plan_from_json`] reads a missing field as null.
pub fn schema_compact() -> serde_json::Value {
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
                    "required": ["cmd"],
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
        // Out of range is caught by the limits (camera 1 or 2); anything that
        // is not a small whole number is refused here rather than wrapped.
        let camera_given = match &s["camera"] {
            serde_json::Value::Null => None,
            v => Some(
                v.as_u64()
                    .and_then(|c| u8::try_from(c).ok())
                    .ok_or_else(|| format!("camera {v} is not a camera number"))?,
            ),
        };
        let camera = camera_given.unwrap_or(1);
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
                camera: camera_given,
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
        for bad in ["257", "-1", "1.5"] {
            let json = format!(
                r#"{{"steps":[{{"cmd":"recenter","seconds":null,"camera":{bad},"direction":null,"on":null}}],"problem":null}}"#
            );
            assert!(plan_from_json(&json).is_err(), "camera {bad}");
        }
        assert!(schema()["properties"]["steps"].is_object());
        assert_eq!(
            schema()["properties"]["steps"]["items"]["required"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            schema_compact()["properties"]["steps"]["items"]["required"],
            serde_json::json!(["cmd"])
        );
        assert_eq!(
            plan_from_json(
                r#"{"steps":[{"cmd":"wait","seconds":2},{"cmd":"recenter"}],"problem":null}"#
            )
            .unwrap()
            .steps,
            [
                Command::Wait { seconds: 2.0 },
                Command::Recenter { camera: None }
            ]
        );
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
