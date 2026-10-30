//! A deterministic parser for plain command sentences, tried before any
//! language model. It accepts only what it understands; anything else is
//! an error with the offending words, so a caller can fall back or ask.
//!
//! Sentences are clauses joined by `then`, `and`, `,` or `;`:
//!
//! * `arm` / `start motors` / `spin up`
//! * `arm for 5 seconds` — arm, wait, and disarm again
//!
//! The parser tracks the motor state through the sentence: an `arm` while
//! armed or a `disarm` while disarmed is dropped, so `arm for 5 seconds
//! then disarm` does not ask the FC to stop motors that are already stopped
//! (it would refuse). A comma splits clauses only when a command verb
//! follows it, so a transcriber's `pan camera two, left for two seconds`
//! stays one clause.
//! * `disarm` / `stop motors` / `stop`
//! * `wait 3 seconds` / `hold for 2 s` / `wait a minute`
//! * `pan left 2 seconds` / `pan camera 2 right for 1 s` / `look up 3 s` /
//!   `tilt down` (one second when no duration is given; camera 1 by default)
//! * `recenter` / `recenter camera 2` / `recenter both`
//! * `link cameras` / `unlink`
//!
//! Numbers may be digits (`5`, `2.5`) or words up to twenty; units are
//! seconds (`s`, `sec`, `secs`, `second(s)`) or minutes (`m`, `min`,
//! `minute(s)`). A missing unit means seconds.

use std::fmt;

use crate::{Command, Horizontal, Plan, Vertical};

/// Why a sentence did not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    /// A clause that is not one of the known forms.
    UnknownClause(String),
    /// A number or unit that could not be read.
    BadDuration(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => f.write_str("nothing to do"),
            ParseError::UnknownClause(c) => write!(f, "did not understand {c:?}"),
            ParseError::BadDuration(c) => write!(f, "unreadable duration in {c:?}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse a sentence into a plan (unvalidated: check it against `Limits`).
pub fn parse(text: &str) -> Result<Plan, ParseError> {
    let mut steps: Vec<Command> = Vec::new();
    let mut any = false;
    for clause in clauses(text) {
        any = true;
        let words: Vec<&str> = clause.split_whitespace().collect();
        let unknown = || ParseError::UnknownClause(clause.clone());
        let bad_duration = || ParseError::BadDuration(clause.clone());
        match words.as_slice() {
            ["arm"] | ["arm", "motors"] | ["start", "motors"] | ["spin", "up"] | ["start"] => {
                push_motor(&mut steps, Command::Arm);
            }
            ["arm", "for", rest @ ..]
            | ["arm", "motors", "for", rest @ ..]
            | ["start", "motors", "for", rest @ ..]
            | ["spin", "up", "for", rest @ ..]
            | ["spin", "for", rest @ ..] => {
                let seconds = duration(rest).ok_or_else(bad_duration)?;
                push_motor(&mut steps, Command::Arm);
                steps.push(Command::Wait { seconds });
                push_motor(&mut steps, Command::Disarm);
            }
            ["disarm"]
            | ["disarm", "motors"]
            | ["stop"]
            | ["stop", "motors"]
            | ["spin", "down"] => {
                push_motor(&mut steps, Command::Disarm);
            }
            ["wait", rest @ ..]
            | ["hold", rest @ ..]
            | ["pause", rest @ ..]
            | ["sleep", rest @ ..] => {
                let rest = rest.strip_prefix(&["for"]).unwrap_or(rest);
                let seconds = duration(rest).ok_or_else(bad_duration)?;
                steps.push(Command::Wait { seconds });
            }
            ["pan" | "tilt" | "look" | "turn" | "move", rest @ ..] => {
                steps.push(gimbal_move(rest).ok_or_else(unknown)?);
            }
            ["recenter" | "center" | "centre" | "recentre", rest @ ..] => {
                let camera = match rest {
                    [] | ["all"] | ["both"] | ["cameras"] | ["both", "cameras"] | ["gimbals"] => {
                        None
                    }
                    _ => Some(camera(rest).ok_or_else(unknown)?),
                };
                steps.push(Command::Recenter { camera });
            }
            ["link"] | ["link", "cameras"] | ["link", "gimbals"] | ["lock", "cameras"] => {
                steps.push(Command::Link { on: true });
            }
            ["unlink"] | ["unlink", "cameras"] | ["unlink", "gimbals"] | ["unlock", "cameras"] => {
                steps.push(Command::Link { on: false });
            }
            _ => return Err(unknown()),
        }
    }
    if !any {
        return Err(ParseError::Empty);
    }
    Ok(Plan::new(steps))
}

/// Words that start a clause; a comma splits clauses only before one of
/// these (`then`, `and`, `;` and a full stop always split).
const VERBS: &[&str] = &[
    "arm", "start", "spin", "disarm", "stop", "wait", "hold", "pause", "sleep", "pan", "tilt",
    "look", "turn", "move", "recenter", "center", "centre", "recentre", "link", "unlink", "lock",
    "unlock", "please", "now",
];

/// Add an arm or disarm unless an earlier step of this plan already left
/// the motors in that state. With no earlier motor step the real state is
/// unknown, so the step is kept (a lone `stop` must always go out).
fn push_motor(steps: &mut Vec<Command>, step: Command) {
    let armed = steps.iter().rev().find_map(|s| match s {
        Command::Arm => Some(true),
        Command::Disarm => Some(false),
        _ => None,
    });
    let redundant = matches!(
        (&step, armed),
        (Command::Arm, Some(true)) | (Command::Disarm, Some(false))
    );
    if !redundant {
        steps.push(step);
    }
}

/// Lower-cased clauses, split on `then` / `and` / `;` / full stops, and on
/// commas before a command verb, with filler words dropped and empty
/// clauses skipped.
fn clauses(text: &str) -> Vec<String> {
    const FILLER: &[&str] = &["please", "the", "now", "a", "an"];
    // A full stop ends a clause, except inside a number such as `1.5`.
    let chars: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let mut flat = String::with_capacity(chars.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        let decimal = c == '.'
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(char::is_ascii_digit);
        if c == ';' || (c == '.' && !decimal) {
            flat.push_str(" then ");
        } else if c == ',' {
            // Decide once the next word is known: mark it, resolve below.
            flat.push_str(" ,");
        } else {
            flat.push(c);
        }
    }
    let words: Vec<&str> = flat.split_whitespace().collect();
    let mut resolved: Vec<&str> = Vec::with_capacity(words.len());
    for (i, w) in words.iter().enumerate() {
        if *w == "," {
            let next = words.get(i + 1).copied().unwrap_or("");
            if VERBS.contains(&next) || next == "then" || next == "and" {
                resolved.push("then");
            }
        } else {
            resolved.push(w);
        }
    }
    resolved
        .into_iter()
        .map(|w| if w == "and" { "then" } else { w })
        .collect::<Vec<_>>()
        .split(|w| *w == "then")
        .map(|ws| {
            ws.iter()
                .filter(|w| !FILLER.contains(w))
                .copied()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|c| !c.is_empty())
        .collect()
}

/// `camera 2`, `cam 1`, `camera two`, or a bare `1` / `2`.
fn camera(words: &[&str]) -> Option<u8> {
    let n = match words {
        ["camera" | "cam" | "gimbal", n] | ["on", "camera" | "cam", n] => n,
        [n] => n,
        _ => return None,
    };
    let n = number(n)?;
    (n == 1.0 || n == 2.0).then_some(n as u8)
}

/// The words after a gimbal verb: a direction, an optional `camera N` and
/// an optional duration (default one second), in any order. The direction
/// decides between pan and tilt whatever the verb was.
fn gimbal_move(words: &[&str]) -> Option<Command> {
    let mut rest: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| *w != "for" && *w != "on")
        .collect();
    let i = rest
        .iter()
        .position(|w| matches!(*w, "left" | "right" | "up" | "down"))?;
    let direction = rest.remove(i);
    let mut cam = 1;
    if let Some(i) = rest
        .iter()
        .position(|w| matches!(*w, "camera" | "cam" | "gimbal"))
    {
        let end = (i + 2).min(rest.len());
        cam = camera(&rest[i..end])?;
        rest.drain(i..end);
    }
    let seconds = if rest.is_empty() {
        1.0
    } else {
        duration(&rest)?
    };
    Some(match direction {
        "left" => Command::Pan {
            camera: cam,
            direction: Horizontal::Left,
            seconds,
        },
        "right" => Command::Pan {
            camera: cam,
            direction: Horizontal::Right,
            seconds,
        },
        "up" => Command::Tilt {
            camera: cam,
            direction: Vertical::Up,
            seconds,
        },
        _ => Command::Tilt {
            camera: cam,
            direction: Vertical::Down,
            seconds,
        },
    })
}

/// `5`, `5s`, `5 seconds`, `two minutes`, `half minute`, `minute`… in seconds.
fn duration(words: &[&str]) -> Option<f32> {
    let (number, unit) = match words {
        [u] if unit(u).is_some() => (1.0, unit(u)?),
        [n] => match split_unit(n) {
            Some((n, u)) => (number(n)?, u),
            None => (number(n)?, 1.0),
        },
        ["half", u] => (0.5, unit(u)?),
        [n, u] => (number(n)?, unit(u)?),
        _ => return None,
    };
    let seconds = number * unit;
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

/// `5s` → (`5`, seconds); `None` when the word has no trailing unit.
fn split_unit(word: &str) -> Option<(&str, f32)> {
    let digits = word.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    if digits.is_empty() || digits.len() == word.len() {
        return None;
    }
    Some((digits, unit(&word[digits.len()..])?))
}

fn number(word: &str) -> Option<f32> {
    const WORDS: [&str; 21] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
    ];
    if let Some(i) = WORDS.iter().position(|w| *w == word) {
        return Some(i as f32);
    }
    word.parse().ok()
}

fn unit(word: &str) -> Option<f32> {
    match word {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1.0),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Command::*;

    fn wait(seconds: f32) -> Command {
        Wait { seconds }
    }

    #[test]
    fn parses_timed_arming() {
        let want = Plan::new(vec![Arm, wait(5.0), Disarm]);
        for text in [
            "arm for 5 seconds then disarm",
            "Arm for 5 seconds, then disarm.",
            "arm for five seconds",
            "please arm the motors for 5s and then stop",
            "spin up for 5 sec then spin down",
            "arm for 5",
        ] {
            assert_eq!(parse(text).as_ref(), Ok(&want), "{text:?}");
        }
    }

    #[test]
    fn parses_separate_steps_and_units() {
        assert_eq!(
            parse("arm, wait 3 seconds, disarm, wait a minute, arm, hold for half minute, stop"),
            Ok(Plan::new(vec![
                Arm,
                wait(3.0),
                Disarm,
                wait(60.0),
                Arm,
                wait(30.0),
                Disarm
            ]))
        );
        assert_eq!(parse("arm"), Ok(Plan::new(vec![Arm])));
        assert_eq!(parse("stop motors"), Ok(Plan::new(vec![Disarm])));
        // Redundant motor steps are dropped: already disarmed after "arm
        // for N", so neither later disarm is sent; a repeated arm is not either.
        assert_eq!(
            parse("arm for 2 s then disarm then wait 1 s then disarm"),
            Ok(Plan::new(vec![Arm, wait(2.0), Disarm, wait(1.0)]))
        );
        assert_eq!(
            parse("arm, arm, wait 1 second, disarm"),
            Ok(Plan::new(vec![Arm, wait(1.0), Disarm]))
        );
        // As Deepgram transcribed it: a comma inside the gimbal clause, a
        // disarm after the motors already stopped.
        assert_eq!(
            parse("Arm for five seconds, then pan camera two, left for two seconds, then disarm."),
            Ok(Plan::new(vec![
                Arm,
                wait(5.0),
                Disarm,
                Pan {
                    camera: 2,
                    direction: Horizontal::Left,
                    seconds: 2.0
                },
            ]))
        );
    }

    #[test]
    fn parses_gimbal_moves() {
        let pan = |camera, direction, seconds| Pan {
            camera,
            direction,
            seconds,
        };
        let tilt = |camera, direction, seconds| Tilt {
            camera,
            direction,
            seconds,
        };
        for (text, want) in [
            ("pan left 2 seconds", pan(1, Horizontal::Left, 2.0)),
            (
                "pan camera 2 right for 1.5 s",
                pan(2, Horizontal::Right, 1.5),
            ),
            ("look up 3 seconds on camera 2", tilt(2, Vertical::Up, 3.0)),
            ("tilt down", tilt(1, Vertical::Down, 1.0)),
            ("turn right for two seconds", pan(1, Horizontal::Right, 2.0)),
            ("tilt cam 2 up", tilt(2, Vertical::Up, 1.0)),
        ] {
            assert_eq!(parse(text), Ok(Plan::new(vec![want])), "{text:?}");
        }
        assert_eq!(
            parse(
                "recenter, then recenter camera 2, then center both, then link cameras, then unlink"
            ),
            Ok(Plan::new(vec![
                Recenter { camera: None },
                Recenter { camera: Some(2) },
                Recenter { camera: None },
                Link { on: true },
                Link { on: false },
            ]))
        );
        assert!(parse("pan").is_err());
        assert!(parse("pan sideways").is_err());
        assert!(parse("pan camera 3 left").is_err());
    }

    #[test]
    fn rejects_what_it_does_not_understand() {
        assert_eq!(parse("   "), Err(ParseError::Empty));
        assert_eq!(
            parse("arm then fly to the moon"),
            Err(ParseError::UnknownClause("fly to moon".into()))
        );
        assert_eq!(
            parse("arm for ever"),
            Err(ParseError::BadDuration("arm for ever".into()))
        );
        assert_eq!(
            parse("wait -3 seconds"),
            Err(ParseError::BadDuration("wait -3 seconds".into()))
        );
        assert!(parse("take off").is_err());
    }
}
