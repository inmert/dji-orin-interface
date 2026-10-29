//! A deterministic parser for plain command sentences, tried before any
//! language model. It accepts only what it understands; anything else is
//! an error with the offending words, so a caller can fall back or ask.
//!
//! Sentences are clauses joined by `then`, `and`, `,` or `;`:
//!
//! * `arm` / `start motors` / `spin up`
//! * `arm for 5 seconds` — arm, wait, and disarm again (a following
//!   `then disarm` is folded in, not doubled)
//! * `disarm` / `stop motors` / `stop`
//! * `wait 3 seconds` / `hold for 2 s` / `wait a minute`
//!
//! Numbers may be digits (`5`, `2.5`) or words up to twenty; units are
//! seconds (`s`, `sec`, `secs`, `second(s)`) or minutes (`m`, `min`,
//! `minute(s)`). A missing unit means seconds.

use std::fmt;

use crate::{Command, Plan};

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
    let mut steps = Vec::new();
    let mut implicit_disarm = false;
    let mut any = false;
    for clause in clauses(text) {
        any = true;
        let words: Vec<&str> = clause.split_whitespace().collect();
        match words.as_slice() {
            ["arm"] | ["arm", "motors"] | ["start", "motors"] | ["spin", "up"] | ["start"] => {
                steps.push(Command::Arm);
                implicit_disarm = false;
            }
            ["arm", "for", rest @ ..]
            | ["arm", "motors", "for", rest @ ..]
            | ["start", "motors", "for", rest @ ..]
            | ["spin", "up", "for", rest @ ..]
            | ["spin", "for", rest @ ..] => {
                let seconds =
                    duration(rest).ok_or_else(|| ParseError::BadDuration(clause.clone()))?;
                steps.push(Command::Arm);
                steps.push(Command::Wait { seconds });
                steps.push(Command::Disarm);
                implicit_disarm = true;
            }
            ["disarm"]
            | ["disarm", "motors"]
            | ["stop"]
            | ["stop", "motors"]
            | ["spin", "down"] => {
                if implicit_disarm {
                    implicit_disarm = false; // already disarmed at the end of "arm for N"
                } else {
                    steps.push(Command::Disarm);
                }
            }
            ["wait", rest @ ..]
            | ["hold", rest @ ..]
            | ["pause", rest @ ..]
            | ["sleep", rest @ ..] => {
                let rest = rest.strip_prefix(&["for"]).unwrap_or(rest);
                let seconds =
                    duration(rest).ok_or_else(|| ParseError::BadDuration(clause.clone()))?;
                steps.push(Command::Wait { seconds });
                implicit_disarm = false;
            }
            _ => return Err(ParseError::UnknownClause(clause)),
        }
    }
    if !any {
        return Err(ParseError::Empty);
    }
    Ok(Plan::new(steps))
}

/// Lower-cased clauses, split on `then` / `and` / `,` / `;`, with filler
/// words dropped and empty clauses skipped.
fn clauses(text: &str) -> Vec<String> {
    const FILLER: &[&str] = &["please", "the", "now", "motors,", "a", "an"];
    text.to_ascii_lowercase()
        .replace([',', ';', '.'], " then ")
        .split_whitespace()
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
        // An explicit disarm after "arm for N" is not doubled, a later one is kept.
        assert_eq!(
            parse("arm for 2 s then disarm then wait 1 s then disarm"),
            Ok(Plan::new(vec![Arm, wait(2.0), Disarm, wait(1.0), Disarm]))
        );
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
