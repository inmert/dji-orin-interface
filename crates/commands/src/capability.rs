//! The capability registry: one descriptor per kind of step, and everything
//! a caller needs derived from the list. The JSON schema a model fills
//! ([`schema`], [`schema_compact`]), the instructions it reads
//! ([`instructions`]), the decoding of a step it produced
//! ([`Command::from_step`]) and the duration limit a step is checked
//! against ([`Capability::duration`]) all come from [`CAPABILITIES`], so a
//! new command is one entry here, one [`Command`] variant, and the code that
//! carries it out. A step's execution (how the vehicle does it) stays with
//! the vehicle; a step's meaning is defined once, here.
//!
//! The schema is deliberately flat, one step shape with every parameter any
//! capability takes (unused ones null): structured-output APIs allow no
//! recursion and work best without `oneOf`. Key order is the order here,
//! `cmd` first, which the models follow when generating.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Command, Horizontal, Vertical};

/// What a parameter holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A duration in seconds (positive, finite).
    Seconds,
    /// A camera number, 1 or 2.
    Camera,
    /// One of a fixed set of words.
    Word(&'static [&'static str]),
    Bool,
}

/// One parameter of a capability.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    /// Whether a step may omit it (the capability then applies a default).
    pub optional: bool,
}

/// Which limit a timed step is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duration {
    /// Not a timed step.
    None,
    /// A wait: `Limits::max_wait`.
    Wait,
    /// A gimbal move: `Limits::max_gimbal`.
    Gimbal,
}

/// One kind of step.
#[derive(Clone, Copy, Debug)]
pub struct Capability {
    /// The `cmd` value in a step.
    pub name: &'static str,
    /// What the model is told it does, with the conventions that matter.
    pub description: &'static str,
    pub params: &'static [Param],
    pub duration: Duration,
    /// The step's [`Command`], given its checked parameters.
    build: fn(&Params) -> Result<Command, String>,
}

const SECONDS: Param = Param {
    name: "seconds",
    kind: Kind::Seconds,
    optional: true,
};
const CAMERA: Param = Param {
    name: "camera",
    kind: Kind::Camera,
    optional: true,
};
const DIRECTION_H: Param = Param {
    name: "direction",
    kind: Kind::Word(&["left", "right"]),
    optional: false,
};
const DIRECTION_V: Param = Param {
    name: "direction",
    kind: Kind::Word(&["up", "down"]),
    optional: false,
};
const ON: Param = Param {
    name: "on",
    kind: Kind::Bool,
    optional: false,
};

/// Every step the aircraft can be asked for, in the order the model sees
/// them. Flight steps (take off, land, scan) are future entries.
pub const CAPABILITIES: &[Capability] = &[
    Capability {
        name: "arm",
        description: "start all motors",
        params: &[],
        duration: Duration::None,
        build: |_| Ok(Command::Arm),
    },
    Capability {
        name: "disarm",
        description: "stop all motors",
        params: &[],
        duration: Duration::None,
        build: |_| Ok(Command::Disarm),
    },
    Capability {
        name: "wait",
        description: "do nothing for the given seconds",
        params: &[Param {
            optional: false,
            ..SECONDS
        }],
        duration: Duration::Wait,
        build: |p| {
            Ok(Command::Wait {
                seconds: p.seconds()?.ok_or("a wait step without seconds")?,
            })
        },
    },
    Capability {
        name: "pan",
        description: "gimbal yaw: camera 1 or 2, direction left or right, seconds",
        params: &[CAMERA, DIRECTION_H, SECONDS],
        duration: Duration::Gimbal,
        build: |p| {
            Ok(Command::Pan {
                camera: p.camera()?.unwrap_or(1),
                direction: match p.word("direction", DIRECTION_H.kind)? {
                    "left" => Horizontal::Left,
                    _ => Horizontal::Right,
                },
                seconds: p.seconds()?.unwrap_or(1.0),
            })
        },
    },
    Capability {
        name: "tilt",
        description: "gimbal pitch: camera 1 or 2, direction up or down, seconds",
        params: &[CAMERA, DIRECTION_V, SECONDS],
        duration: Duration::Gimbal,
        build: |p| {
            Ok(Command::Tilt {
                camera: p.camera()?.unwrap_or(1),
                direction: match p.word("direction", DIRECTION_V.kind)? {
                    "up" => Vertical::Up,
                    _ => Vertical::Down,
                },
                seconds: p.seconds()?.unwrap_or(1.0),
            })
        },
    },
    Capability {
        name: "recenter",
        description: "return a gimbal's yaw to centre: camera 1 or 2, or null for both",
        params: &[CAMERA],
        duration: Duration::None,
        build: |p| {
            Ok(Command::Recenter {
                camera: p.camera()?,
            })
        },
    },
    Capability {
        name: "link",
        description: "on true engages the lock so camera 2 follows camera 1, false releases it",
        params: &[ON],
        duration: Duration::None,
        build: |p| Ok(Command::Link { on: p.bool("on")? }),
    },
];

/// The capability named `name`.
pub fn capability(name: &str) -> Option<&'static Capability> {
    CAPABILITIES.iter().find(|c| c.name == name)
}

/// Every distinct parameter, in order of first appearance: the flat step
/// shape's fields after `cmd`.
fn all_params() -> Vec<Param> {
    let mut out: Vec<Param> = Vec::new();
    for c in CAPABILITIES {
        for p in c.params {
            match out.iter_mut().find(|q| q.name == p.name) {
                // The same name with different words: the union.
                Some(q) => {
                    if let (Kind::Word(a), Kind::Word(b)) = (q.kind, p.kind)
                        && a != b
                    {
                        q.kind = Kind::Word(words_union(a, b));
                    }
                }
                None => out.push(*p),
            }
        }
    }
    out
}

/// The union of two word sets, as a leaked static (built once per schema).
fn words_union(a: &'static [&'static str], b: &'static [&'static str]) -> &'static [&'static str] {
    let mut v: Vec<&'static str> = a.to_vec();
    v.extend(b.iter().filter(|w| !a.contains(w)));
    Box::leak(v.into_boxed_slice())
}

fn kind_schema(kind: Kind) -> Value {
    match kind {
        Kind::Seconds => json!({"type": ["number", "null"]}),
        Kind::Camera => json!({"type": ["integer", "null"]}),
        Kind::Word(words) => json!({"anyOf": [
            {"type": "string", "enum": words},
            {"type": "null"}
        ]}),
        Kind::Bool => json!({"type": ["boolean", "null"]}),
    }
}

/// The schema with only `cmd` required per step, for a server that turns
/// the schema into a grammar: a step is then a few tokens instead of a null
/// per unused field. [`Command::from_step`] reads a missing field as null.
pub fn schema_compact() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "cmd".into(),
        json!({"type": "string", "enum": CAPABILITIES.iter().map(|c| c.name).collect::<Vec<_>>()}),
    );
    for p in all_params() {
        props.insert(p.name.into(), kind_schema(p.kind));
    }
    json!({
        "type": "object",
        "properties": {
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": Value::Object(props),
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

/// The schema the online models fill: every field of a step is required
/// (OpenAI's strict mode insists), so unused ones are null.
///
/// Key order matters: `serde_json` is built with `preserve_order` so the
/// schema reaches the model as written, `steps` before `problem` and `cmd`
/// first in a step. Sorted alphabetically (`camera, cmd, …`, `problem`
/// first) Claude stopped after the first step on most multi-step sentences.
pub fn schema() -> Value {
    let mut s = schema_compact();
    let mut required = vec!["cmd"];
    required.extend(all_params().iter().map(|p| p.name));
    s["properties"]["steps"]["items"]["required"] = json!(required);
    s
}

/// What the models are told: the aircraft, the capabilities from the
/// registry, the conventions, and a few worked examples.
pub fn instructions() -> String {
    let steps = CAPABILITIES
        .iter()
        .map(|c| format!("{} ({})", c.name, c.description))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "You translate spoken or typed instructions for a DJI Matrice 210 drone into a list of \
         steps. The only steps are: {steps}. Camera defaults to 1 and a gimbal move defaults to \
         1 second when not stated. 'arm for N seconds' means arm, wait N, disarm; do not add a \
         disarm when the motors are already stopped at that point. Flight (take off, land, hover, \
         move the aircraft) is not available: never invent steps for it. Fill every field; use \
         null for fields a step does not use. If the instruction is not one of these commands, \
         is ambiguous, or asks for something unsafe, return an empty steps list and say why in \
         problem; otherwise problem is null. Examples: 'arm for 5 seconds then disarm' is arm, \
         wait 5, disarm. 'start the motors, hold for ten seconds, then stop them' is arm, wait \
         10, disarm. 'look up for three seconds on camera 2' is tilt camera 2 up 3. 'unlink and \
         pan right for two and a half seconds' is link off, then pan camera 1 right 2.5. \
         'recenter' alone is recenter with camera null."
    )
}

/// A step as a model produces it: the capability name and its parameters.
#[derive(Clone, Debug, Deserialize)]
pub struct Step {
    pub cmd: String,
    #[serde(flatten)]
    pub params: Params,
}

/// A step's parameters by name, with typed, checked readers. A missing
/// parameter and an explicit null are the same thing.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Params(pub BTreeMap<String, Value>);

impl Params {
    fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name).filter(|v| !v.is_null())
    }

    pub fn seconds(&self) -> Result<Option<f32>, String> {
        self.get("seconds")
            .map(|v| {
                v.as_f64()
                    .map(|x| x as f32)
                    .ok_or_else(|| format!("seconds {v} is not a number"))
            })
            .transpose()
    }

    /// A camera number as given; whether it is 1 or 2 is the limits' check,
    /// but anything that is not a small whole number is refused here rather
    /// than wrapped.
    pub fn camera(&self) -> Result<Option<u8>, String> {
        self.get("camera")
            .map(|v| {
                v.as_u64()
                    .and_then(|c| u8::try_from(c).ok())
                    .ok_or_else(|| format!("camera {v} is not a camera number"))
            })
            .transpose()
    }

    /// A required word from the parameter's set.
    pub fn word(&self, name: &str, kind: Kind) -> Result<&str, String> {
        let Kind::Word(words) = kind else {
            return Err(format!("{name} is not a word parameter"));
        };
        let w = self
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("a step without {name}"))?;
        words
            .iter()
            .find(|k| **k == w)
            .copied()
            .ok_or_else(|| format!("{name} {w:?} is not one of {}", words.join(", ")))
    }

    pub fn bool(&self, name: &str) -> Result<bool, String> {
        self.get(name)
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("a step without {name}"))
    }
}

impl Command {
    /// The command a model's step means, or why it means nothing.
    pub fn from_step(step: &Step) -> Result<Command, String> {
        let cap = capability(&step.cmd).ok_or_else(|| format!("unknown step {:?}", step.cmd))?;
        (cap.build)(&step.params)
    }

    /// The registry entry this command is an instance of.
    pub fn capability(&self) -> &'static Capability {
        let name = match self {
            Command::Arm => "arm",
            Command::Disarm => "disarm",
            Command::Wait { .. } => "wait",
            Command::Pan { .. } => "pan",
            Command::Tilt { .. } => "tilt",
            Command::Recenter { .. } => "recenter",
            Command::Link { .. } => "link",
        };
        capability(name).expect("every command variant is registered")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_lists_every_capability_and_parameter_in_order() {
        let s = schema();
        let step = &s["properties"]["steps"]["items"];
        assert_eq!(
            step["properties"]["cmd"]["enum"],
            json!(["arm", "disarm", "wait", "pan", "tilt", "recenter", "link"])
        );
        let keys: Vec<&String> = step["properties"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["cmd", "seconds", "camera", "direction", "on"]);
        assert_eq!(
            step["required"],
            json!(["cmd", "seconds", "camera", "direction", "on"])
        );
        assert_eq!(
            step["properties"]["direction"]["anyOf"][0]["enum"],
            json!(["left", "right", "up", "down"])
        );
        assert_eq!(
            schema_compact()["properties"]["steps"]["items"]["required"],
            json!(["cmd"])
        );
        let keys: Vec<&String> = s.as_object().unwrap()["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(keys, ["steps", "problem"], "steps before problem");
    }

    #[test]
    fn instructions_name_every_capability() {
        let text = instructions();
        for c in CAPABILITIES {
            assert!(text.contains(&format!("{} (", c.name)), "{}", c.name);
        }
        assert!(text.contains("never invent steps"));
    }

    fn step(json: &str) -> Result<Command, String> {
        Command::from_step(&serde_json::from_str::<Step>(json).unwrap())
    }

    #[test]
    fn steps_decode_with_defaults_and_refuse_bad_parameters() {
        assert_eq!(step(r#"{"cmd":"arm"}"#), Ok(Command::Arm));
        assert_eq!(
            step(r#"{"cmd":"wait","seconds":3,"camera":null}"#),
            Ok(Command::Wait { seconds: 3.0 })
        );
        assert_eq!(
            step(r#"{"cmd":"pan","direction":"left","camera":2}"#),
            Ok(Command::Pan {
                camera: 2,
                direction: Horizontal::Left,
                seconds: 1.0
            })
        );
        assert_eq!(
            step(r#"{"cmd":"tilt","direction":"down","seconds":2.5}"#),
            Ok(Command::Tilt {
                camera: 1,
                direction: Vertical::Down,
                seconds: 2.5
            })
        );
        assert_eq!(
            step(r#"{"cmd":"recenter"}"#),
            Ok(Command::Recenter { camera: None })
        );
        assert_eq!(
            step(r#"{"cmd":"link","on":false}"#),
            Ok(Command::Link { on: false })
        );
        assert!(step(r#"{"cmd":"wait"}"#).is_err());
        assert!(step(r#"{"cmd":"pan","direction":"up"}"#).is_err());
        assert!(step(r#"{"cmd":"link"}"#).is_err());
        assert!(step(r#"{"cmd":"fly"}"#).is_err());
        for bad in ["257", "-1", "1.5", "\"two\""] {
            assert!(
                step(&format!(r#"{{"cmd":"recenter","camera":{bad}}}"#)).is_err(),
                "camera {bad}"
            );
        }
    }

    #[test]
    fn the_registry_has_unique_names_and_one_kind_per_parameter() {
        for (i, a) in CAPABILITIES.iter().enumerate() {
            assert!(
                CAPABILITIES[i + 1..].iter().all(|b| b.name != a.name),
                "duplicate capability {}",
                a.name
            );
            for (j, p) in a.params.iter().enumerate() {
                assert!(
                    a.params[j + 1..].iter().all(|q| q.name != p.name),
                    "{} lists {} twice",
                    a.name,
                    p.name
                );
            }
        }
        // The flat step shape has one schema per parameter name, so a name
        // must mean the same kind everywhere (word sets may differ: they are
        // merged).
        let same_kind =
            |a: Kind, b: Kind| matches!((a, b), (Kind::Word(_), Kind::Word(_))) || a == b;
        for a in CAPABILITIES.iter().flat_map(|c| c.params) {
            for b in CAPABILITIES.iter().flat_map(|c| c.params) {
                assert!(
                    a.name != b.name || same_kind(a.kind, b.kind),
                    "{} has two kinds",
                    a.name
                );
            }
        }
    }

    #[test]
    fn every_capability_decodes_its_minimal_step() {
        // The required parameters, each with a value of its kind: every
        // registered name must build a command, and that command must name
        // its own capability back.
        for c in CAPABILITIES {
            let mut params = BTreeMap::new();
            for p in c.params.iter().filter(|p| !p.optional) {
                let value = match p.kind {
                    Kind::Seconds => json!(2.5),
                    Kind::Camera => json!(2),
                    Kind::Word(words) => json!(words[0]),
                    Kind::Bool => json!(true),
                };
                params.insert(p.name.to_string(), value);
            }
            let step = Step {
                cmd: c.name.into(),
                params: Params(params),
            };
            let cmd = Command::from_step(&step).unwrap_or_else(|e| panic!("{}: {e}", c.name));
            assert_eq!(cmd.capability().name, c.name);
        }
    }

    #[test]
    fn parameters_of_the_wrong_type_are_refused() {
        for bad in [
            r#"{"cmd":"wait","seconds":"3"}"#,
            r#"{"cmd":"wait","seconds":true}"#,
            r#"{"cmd":"pan","direction":"left","camera":"2"}"#,
            r#"{"cmd":"pan","direction":7}"#,
            r#"{"cmd":"link","on":"true"}"#,
            r#"{"cmd":"link","on":1}"#,
            r#"{"cmd":"ARM"}"#,
            r#"{"cmd":""}"#,
        ] {
            assert!(step(bad).is_err(), "{bad}");
        }
        // Values of the right type out of range decode, and the limits refuse
        // them (see `Plan::validate`).
        assert_eq!(
            step(r#"{"cmd":"wait","seconds":-2}"#),
            Ok(Command::Wait { seconds: -2.0 })
        );
        assert_eq!(
            step(r#"{"cmd":"recenter","camera":0}"#),
            Ok(Command::Recenter { camera: Some(0) })
        );
        assert_eq!(
            step(r#"{"cmd":"recenter","camera":255}"#),
            Ok(Command::Recenter { camera: Some(255) })
        );
    }

    #[test]
    fn every_command_knows_its_capability() {
        let cmds = [
            Command::Arm,
            Command::Disarm,
            Command::Wait { seconds: 1.0 },
            Command::Pan {
                camera: 1,
                direction: Horizontal::Left,
                seconds: 1.0,
            },
            Command::Tilt {
                camera: 1,
                direction: Vertical::Up,
                seconds: 1.0,
            },
            Command::Recenter { camera: None },
            Command::Link { on: true },
        ];
        assert_eq!(cmds.len(), CAPABILITIES.len());
        for (c, cap) in cmds.iter().zip(CAPABILITIES) {
            assert_eq!(c.capability().name, cap.name);
        }
        assert_eq!(
            Command::Wait { seconds: 1.0 }.capability().duration,
            Duration::Wait
        );
        assert_eq!(Command::Arm.capability().duration, Duration::None);
    }
}
