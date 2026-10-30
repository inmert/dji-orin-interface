//! Latency and accuracy of the understanding providers on a fixed set of
//! sentences, through the same [`Provider::interpret`] the server uses.
//! Needs the services, so it is ignored by default:
//!
//! ```text
//! SECRETS_DIR=$PWD/secrets LOCAL_LLM_URL=http://127.0.0.1:8081 \
//!   cargo test -p dji-server --release bench -- --ignored --nocapture
//! ```
//!
//! `BENCH_PROVIDERS` limits the run (`local`, `anthropic`, `openai`,
//! comma-separated); `BENCH_ROUNDS` repeats every sentence (default 1).

use std::time::Instant;

use dji_commands::{Command, Horizontal, Limits, Plan, Vertical};

use super::{Provider, anthropic, local, openai};

/// A sentence and the plans accepted for it; empty means it must be declined.
fn cases() -> Vec<(&'static str, Vec<Vec<Command>>)> {
    use Command::*;
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
    let wait = |seconds| Wait { seconds };
    vec![
        // The two sentences of the first live run (commit e8a4c56).
        (
            "Could you spin the motors up, keep them going for about four seconds, and shut \
             them down again? After that swing the second camera to the right for a second \
             and a half, then put everything back to centre.",
            vec![vec![
                Arm,
                wait(4.0),
                Disarm,
                pan(2, Horizontal::Right, 1.5),
                Recenter { camera: None },
            ]],
        ),
        ("take off and fly to the parking lot", vec![]),
        // The grammar's own forms, as typed and as Deepgram writes them.
        (
            "arm for 5 seconds then disarm",
            vec![vec![Arm, wait(5.0), Disarm]],
        ),
        (
            "Arm for five seconds, then pan camera two, left for two seconds, then disarm.",
            vec![vec![Arm, wait(5.0), Disarm, pan(2, Horizontal::Left, 2.0)]],
        ),
        (
            "look up for three seconds on camera 2",
            vec![vec![tilt(2, Vertical::Up, 3.0)]],
        ),
        (
            "tilt the first camera down",
            vec![vec![tilt(1, Vertical::Down, 1.0)]],
        ),
        (
            "link the cameras and recenter",
            vec![vec![Link { on: true }, Recenter { camera: None }]],
        ),
        ("wait half a minute", vec![vec![wait(30.0)]]),
        (
            "start the motors, hold for ten seconds, then stop them",
            vec![vec![Arm, wait(10.0), Disarm]],
        ),
        ("pan left", vec![vec![pan(1, Horizontal::Left, 1.0)]]),
        ("land the drone", vec![]),
        (
            "unlink and point camera one to the right for two and a half seconds",
            vec![vec![Link { on: false }, pan(1, Horizontal::Right, 2.5)]],
        ),
    ]
}

fn providers() -> Vec<Box<dyn Provider>> {
    let wanted: Vec<String> = std::env::var("BENCH_PROVIDERS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let want = |name: &str| wanted.is_empty() || wanted.iter().any(|w| w == name);
    if let Ok(dir) = std::env::var("SECRETS_DIR") {
        crate::ai::online::init(std::path::Path::new(&dir));
    }
    let keys = crate::ai::online::keys();
    let mut out: Vec<Box<dyn Provider>> = Vec::new();
    if want("anthropic")
        && let Some(k) = keys.anthropic.clone()
    {
        out.push(Box::new(anthropic::Anthropic::new(k, None, None)));
    }
    if want("openai")
        && let Some(k) = keys.openai.clone()
    {
        out.push(Box::new(openai::OpenAi::new(k, None)));
    }
    if want("local")
        && let Ok(url) = std::env::var("LOCAL_LLM_URL")
    {
        out.push(Box::new(local::Local::new(
            url,
            std::env::var("LOCAL_LLM_MODEL").ok(),
        )));
    }
    out
}

#[test]
#[ignore = "calls the configured model services; see the module docs"]
fn providers_on_the_test_sentences() {
    let providers = providers();
    assert!(
        !providers.is_empty(),
        "no provider configured (keys, LOCAL_LLM_URL)"
    );
    let rounds: usize = std::env::var("BENCH_ROUNDS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(1);
    let limits = Limits::default();
    for p in &providers {
        println!("\n== {} {} ==", p.name(), p.model());
        let (mut right, mut total) = (0, 0);
        let mut ms: Vec<u128> = Vec::new();
        for _ in 0..rounds {
            for (text, accepted) in cases() {
                let start = Instant::now();
                let result = p.interpret(text).and_then(|plan| {
                    plan.validate(&limits)
                        .map(|()| plan)
                        .map_err(|e| e.to_string())
                });
                let took = start.elapsed().as_millis();
                ms.push(took);
                total += 1;
                let (ok, shown) = match &result {
                    Ok(plan) => (
                        accepted.iter().any(|a| Plan::new(a.clone()) == *plan),
                        plan.to_string(),
                    ),
                    Err(e) => (accepted.is_empty(), format!("declined: {e}")),
                };
                right += usize::from(ok);
                println!(
                    "{} {took:>6} ms  {text:?}\n            -> {shown}",
                    if ok { "ok  " } else { "MISS" }
                );
            }
        }
        ms.sort_unstable();
        println!(
            "{} {}: {right}/{total} right; latency median {} ms, min {} ms, max {} ms",
            p.name(),
            p.model(),
            ms[ms.len() / 2],
            ms[0],
            ms[ms.len() - 1]
        );
    }
}
