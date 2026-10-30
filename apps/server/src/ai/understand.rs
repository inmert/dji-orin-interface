//! Understanding: turning a sentence (typed or transcribed) into a
//! [`Plan`]. A model does it first ([`Provider`]: [`anthropic`],
//! [`openai`] online, or [`local`], a model served on the Orin), since it
//! copes with phrasing the local grammar does not; the grammar
//! ([`dji_commands::grammar`]) is the fallback when no model is configured
//! or the call fails. Either way the Orin validates the plan
//! against its limits before anything runs, and a plan typed as JSON
//! (`{"steps":[…]}`) skips both.
//!
//! The model is asked for the plan as structured output against the
//! schema the capability registry derives ([`dji_commands::capability`]:
//! one flat step shape, since such schemas allow no recursion), with the
//! instructions derived from the same registry; [`plan_from_json`] turns
//! the answer into a [`Plan`] through it. Keys come from
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

use dji_commands::{Command, Plan, Step, grammar, instructions, schema, schema_compact};
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

/// Why a model gave no plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The model answered and refused: not a drone command, ambiguous,
    /// unsafe, or a step that does not exist. Final: the grammar is not
    /// asked to second-guess it.
    Declined(String),
    /// No usable answer (network, HTTP error, malformed output): the
    /// grammar may still understand the sentence.
    Failed(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Declined(why) | Failure::Failed(why) => f.write_str(why),
        }
    }
}

impl From<String> for Failure {
    fn from(why: String) -> Self {
        Failure::Failed(why)
    }
}

impl From<&str> for Failure {
    fn from(why: &str) -> Self {
        Failure::Failed(why.into())
    }
}

/// One online model.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;
    /// The plan for `text`, or why there is none.
    fn interpret(&self, text: &str) -> Result<Plan, Failure>;
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
    interpret_with(text, registry().active())
}

/// [`interpret`] with `model` as the active provider.
fn interpret_with(text: &str, model: Option<&dyn Provider>) -> Result<Interpreted, String> {
    let text = text.trim();
    if text.starts_with('{') {
        let plan = serde_json::from_str::<Plan>(text).map_err(|e| format!("bad plan JSON: {e}"))?;
        return Ok(Interpreted {
            plan,
            source: "json".into(),
        });
    }
    let model_error = match model {
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
                Err(Failure::Declined(why)) => {
                    logln!("understand: {} declined: {why}", p.name());
                    return Err(why);
                }
                Err(Failure::Failed(e)) => {
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

/// A [`Plan`] from the JSON the schema produces: the model's `problem` is
/// the error when it gave one, else every step decoded by the registry.
pub fn plan_from_json(text: &str) -> Result<Plan, Failure> {
    #[derive(serde::Deserialize)]
    struct Answer {
        #[serde(default)]
        steps: Vec<Step>,
        #[serde(default)]
        problem: Option<String>,
    }
    let a: Answer =
        serde_json::from_str(text).map_err(|e| format!("model returned invalid JSON: {e}"))?;
    if let Some(problem) = a.problem.filter(|p| !p.is_empty()) {
        return Err(Failure::Declined(problem));
    }
    let steps = a
        .steps
        .iter()
        .map(Command::from_step)
        .collect::<Result<Vec<_>, _>>()?;
    if steps.is_empty() {
        return Err(Failure::Declined("the model found nothing to do".into()));
    }
    Ok(Plan::new(steps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dji_commands::Horizontal;

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
            Failure::Declined("take-off is not available".into())
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
        assert!(!instructions().is_empty());
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

    /// A model that always gives `answer`.
    struct Fixed(Result<Plan, Failure>);

    impl Provider for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }
        fn model(&self) -> &str {
            "fixed-1"
        }
        fn interpret(&self, _: &str) -> Result<Plan, Failure> {
            self.0.clone()
        }
        fn status(&self) -> Status {
            unreachable!()
        }
    }

    #[test]
    fn a_refusal_is_final_and_a_failure_falls_back_to_the_grammar() {
        // "move up" is aircraft movement to the model, a gimbal tilt to the
        // grammar: the model's refusal must stand.
        let declined = Fixed(Err(Failure::Declined(
            "moving the aircraft is not available".into(),
        )));
        assert_eq!(
            interpret_with("move up for 2 seconds", Some(&declined)).err(),
            Some("moving the aircraft is not available".to_string())
        );
        let failed = Fixed(Err(Failure::Failed("Claude: HTTP 529: overloaded".into())));
        let i = interpret_with("move up for 2 seconds", Some(&failed)).unwrap();
        assert_eq!(i.source, "grammar");
        // A model that found nothing to do has declined too.
        assert_eq!(
            plan_from_json(r#"{"steps":[],"problem":null}"#),
            Err(Failure::Declined("the model found nothing to do".into()))
        );
        // Malformed output is a failure, not a refusal.
        assert!(matches!(plan_from_json("{"), Err(Failure::Failed(_))));
        let answer = Fixed(Ok(Plan::new(vec![Command::Disarm])));
        assert_eq!(
            interpret_with("stop", Some(&answer)).unwrap().source,
            "fixed-1"
        );
    }
}
