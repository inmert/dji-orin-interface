//! Voice transcription on the Orin: a recording uploaded by the portal
//! (whatever the browser's `MediaRecorder` produced) is converted to 16 kHz
//! mono WAV with `ffmpeg` and transcribed by whisper.cpp's `whisper-cli` on
//! the GPU. One transcription runs at a time.
//!
//! `WHISPER_DIR` is the whisper.cpp checkout (`scripts/setup_whisper.sh`
//! builds it; default `~/repos/third_party/whisper.cpp`), and `WHISPER_CLI`
//! / `WHISPER_MODEL` override the binary and model paths individually.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;

use dji_common::logln;

/// Words the commands use, given to the model as context so it prefers
/// them over sound-alikes.
const PROMPT: &str = "Drone commands: arm, disarm, stop, wait, seconds, pan, tilt, \
                      camera, left, right, up, down, recenter, link, unlink.";

#[derive(Serialize)]
pub struct Transcript {
    pub text: String,
    /// Transcription time, ms (conversion included).
    pub ms: u64,
}

struct Config {
    cli: PathBuf,
    model: PathBuf,
}

fn config() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let base = std::env::var_os("WHISPER_DIR").map_or_else(
            || {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
                PathBuf::from(home).join("repos/third_party/whisper.cpp")
            },
            PathBuf::from,
        );
        let from_env =
            |var: &str, default: PathBuf| std::env::var_os(var).map_or(default, PathBuf::from);
        Config {
            cli: from_env("WHISPER_CLI", base.join("build/bin/whisper-cli")),
            model: from_env("WHISPER_MODEL", base.join("models/ggml-base.en.bin")),
        }
    })
}

/// File extension for the upload's media type (ffmpeg sniffs the content;
/// the extension only helps it along).
fn extension(content_type: &str) -> &'static str {
    let kind = content_type.split(';').next().unwrap_or("").trim();
    match kind {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/mp4" | "audio/x-m4a" | "video/mp4" => "mp4",
        "audio/mpeg" => "mp3",
        _ => "webm",
    }
}

/// Transcribe `audio` (an upload of type `content_type`).
pub fn transcribe(audio: &[u8], content_type: &str) -> Result<Transcript, String> {
    static LOCK: Mutex<()> = Mutex::new(());
    static N: AtomicU64 = AtomicU64::new(0);
    if audio.is_empty() {
        return Err("empty recording".into());
    }
    let cfg = config();
    if !cfg.cli.exists() {
        return Err(format!(
            "whisper-cli not found at {} (run scripts/setup_whisper.sh, or set WHISPER_CLI)",
            cfg.cli.display()
        ));
    }
    if !cfg.model.exists() {
        return Err(format!(
            "whisper model not found at {} (set WHISPER_MODEL)",
            cfg.model.display()
        ));
    }
    let _one_at_a_time = LOCK.lock().unwrap();
    let start = Instant::now();
    let stem = std::env::temp_dir().join(format!(
        "dji-voice-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let input = stem.with_extension(extension(content_type));
    let wav = stem.with_extension("wav");
    let cleanup = || {
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&wav);
    };
    std::fs::write(&input, audio).map_err(|e| format!("write recording: {e}"))?;
    let result = convert(&input, &wav).and_then(|()| run(cfg, &wav));
    cleanup();
    let text = result?;
    let ms = start.elapsed().as_millis() as u64;
    logln!("voice: {ms} ms, {} bytes: {text:?}", audio.len());
    Ok(Transcript { text, ms })
}

fn convert(input: &std::path::Path, wav: &std::path::Path) -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(["-y", "-nostdin", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-ar", "16000", "-ac", "1", "-f", "wav"])
        .arg(wav)
        .output()
        .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn run(cfg: &Config, wav: &std::path::Path) -> Result<String, String> {
    let out = Command::new(&cfg.cli)
        .arg("-m")
        .arg(&cfg.model)
        .arg("-f")
        .arg(wav)
        .args(["-l", "en", "-nt", "-np", "--prompt", PROMPT])
        .output()
        .map_err(|e| format!("cannot run whisper-cli: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "whisper-cli failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(clean(&String::from_utf8_lossy(&out.stdout)))
}

/// One line of plain text: whisper's bracketed non-speech tags and extra
/// whitespace removed.
fn clean(raw: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in raw.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_whisper_output() {
        assert_eq!(
            clean("\n [BLANK_AUDIO]  Arm for five seconds, (wind)  then disarm.\n"),
            "Arm for five seconds, then disarm."
        );
        assert_eq!(extension("audio/webm;codecs=opus"), "webm");
        assert_eq!(extension("audio/wav"), "wav");
        assert_eq!(extension("audio/mp4"), "mp4");
    }

    #[test]
    fn empty_recording_is_refused_before_any_process_runs() {
        assert!(transcribe(&[], "audio/webm").is_err());
    }
}
