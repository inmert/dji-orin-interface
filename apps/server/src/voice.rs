//! Voice input for the portal's hold-to-talk: the browser's recording is
//! forwarded to an online speech-recognition service and the text comes
//! back. Nothing is decoded on the Orin; it only validates and runs the
//! resulting command.
//!
//! Providers sit behind [`Provider`] so the service can be chosen or swapped
//! without touching the callers: [`openai`] (`gpt-4o-transcribe`) and
//! [`deepgram`] (`nova-3`). Keys are read from `secrets/*.key`
//! (`SECRETS_DIR`), one key per file, recognised by prefix (`sk-…` is
//! OpenAI) or else by file name (`deepgram.key`); `OPENAI_API_KEY` and
//! `DEEPGRAM_API_KEY` override. `STT_PROVIDER` picks the provider; by
//! default the first configured of `openai`, `deepgram`. The browser never
//! sees a key: the server is the only caller.
//!
//! [`statuses`] checks each key against its service and reports spend or
//! balance where the service exposes one.

pub mod deepgram;
pub mod openai;

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_common::logln;

/// How long one service call may take.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// How long a status check (key, balance) is reused before asking again.
const STATUS_TTL: Duration = Duration::from_secs(60);

/// Words the commands use, given to the service as context so it prefers
/// them over sound-alikes.
pub const VOCABULARY: [&str; 16] = [
    "arm", "disarm", "stop", "wait", "seconds", "pan", "tilt", "camera", "left", "right", "up",
    "down", "recenter", "link", "unlink", "gimbal",
];

#[derive(Serialize)]
pub struct Transcript {
    pub text: String,
    /// Round trip to the service, ms.
    pub ms: u64,
    pub provider: &'static str,
}

/// What the UI shows about one provider.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub provider: &'static str,
    /// The one hold-to-talk uses.
    pub active: bool,
    pub model: &'static str,
    /// Whether the service accepted the key; `None` when it could not be asked.
    pub key_ok: Option<bool>,
    /// Balance or spend, when the service exposes it (e.g. `$4.12 left`).
    pub balance: Option<String>,
    /// Anything else worth knowing (errors, what balance would need).
    pub note: String,
}

/// One online speech-recognition service.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Transcribe one recording of media type `content_type`.
    fn transcribe(&self, audio: &[u8], content_type: &str) -> Result<String, String>;
    /// Check the key and, where offered, the balance.
    fn status(&self) -> Status;
}

/// The configured providers and which one is in use.
pub struct Registry {
    providers: Vec<Box<dyn Provider>>,
    active: Option<usize>,
    status_cache: Mutex<Option<(Instant, Vec<Status>)>>,
}

impl Registry {
    fn from_env() -> Self {
        let keys = Keys::load();
        let mut providers: Vec<Box<dyn Provider>> = Vec::new();
        if let Some(k) = keys.openai {
            providers.push(Box::new(openai::OpenAi::new(k)));
        }
        if let Some(k) = keys.deepgram {
            providers.push(Box::new(deepgram::Deepgram::new(k)));
        }
        let wanted = std::env::var("STT_PROVIDER").ok();
        let active = providers
            .iter()
            .position(|p| wanted.as_deref().is_none_or(|w| w == p.name()));
        if let (Some(w), None) = (&wanted, active) {
            logln!("voice: STT_PROVIDER={w} is not configured (no key)");
        }
        logln!(
            "voice: speech providers [{}]; using {}",
            providers
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", "),
            active.map_or("none", |i| providers[i].name())
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

    /// Every provider's status, checked at most once a minute.
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

/// Transcribe `audio` (an upload of type `content_type`) with the active
/// provider.
pub fn transcribe(audio: &[u8], content_type: &str) -> Result<Transcript, String> {
    if audio.is_empty() {
        return Err("empty recording".into());
    }
    let provider = registry().active().ok_or(
        "no speech-recognition key: put an OpenAI or Deepgram API key in secrets/ (openai.key / deepgram.key) and restart",
    )?;
    let start = Instant::now();
    let text = clean(&provider.transcribe(audio, content_type)?);
    let ms = start.elapsed().as_millis() as u64;
    logln!(
        "voice: {} {ms} ms, {} bytes: {text:?}",
        provider.name(),
        audio.len()
    );
    Ok(Transcript {
        text,
        ms,
        provider: provider.name(),
    })
}

/// Status of every configured provider (`GET /voice/status`).
pub fn statuses() -> Vec<Status> {
    registry().statuses()
}

/// The keys found, by provider.
#[derive(Default)]
struct Keys {
    openai: Option<String>,
    deepgram: Option<String>,
}

impl Keys {
    fn load() -> Self {
        let mut keys = Self::default();
        let dir = std::env::var("SECRETS_DIR").unwrap_or_else(|_| "secrets".into());
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "key"))
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let key = text.trim();
            if key.is_empty() {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match classify(key, &stem) {
                Ok(provider) => {
                    logln!("voice: {provider} key from {}", path.display());
                    keys.set(provider, key);
                }
                Err(why) => logln!("voice: {}: {why}; ignored", path.display()),
            }
        }
        for (var, provider) in [
            ("OPENAI_API_KEY", "openai"),
            ("DEEPGRAM_API_KEY", "deepgram"),
        ] {
            if let Ok(k) = std::env::var(var)
                && !k.trim().is_empty()
            {
                keys.set(provider, k.trim());
            }
        }
        keys
    }

    fn set(&mut self, provider: &str, key: &str) {
        match provider {
            "openai" => self.openai = Some(key.to_string()),
            "deepgram" => self.deepgram = Some(key.to_string()),
            _ => {}
        }
    }
}

/// Which speech provider a key belongs to: by prefix when the vendor has
/// one, else by the file it came from. Keys of services that do not
/// transcribe speech are named so the log says why they are ignored.
fn classify(key: &str, hint: &str) -> Result<&'static str, &'static str> {
    if key.starts_with("sk-ant-") {
        return Err("an Anthropic key; Claude does not transcribe speech");
    }
    if key.starts_with("sk-") {
        return Ok("openai");
    }
    if hint.contains("deepgram") {
        return Ok("deepgram");
    }
    if hint.contains("openai") {
        return Ok("openai");
    }
    Err("not a recognised speech-recognition key (name the file openai.key or deepgram.key)")
}

/// One line of plain text: extra whitespace collapsed.
pub fn clean(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// File extension for an upload's media type.
pub fn extension(content_type: &str) -> &'static str {
    let kind = content_type.split(';').next().unwrap_or("").trim();
    match kind {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/mp4" | "audio/x-m4a" | "video/mp4" => "mp4",
        "audio/mpeg" => "mp3",
        _ => "webm",
    }
}

/// An HTTP agent with the service timeout; status codes come back as
/// responses (with their error bodies), not as errors.
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .build()
        .into()
}

/// A response's status and body text.
pub fn read(resp: &mut ureq::http::Response<ureq::Body>) -> Result<(u16, String), String> {
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("response unreadable: {e}"))?;
    Ok((status, text))
}

/// The service's error message out of a JSON error body, else the body.
pub fn error_detail(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["err_msg"].as_str())
                .or_else(|| v["message"].as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| text.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_keys_by_prefix_then_file_name() {
        assert_eq!(classify("sk-proj-xyz", "anthropic"), Ok("openai"));
        assert_eq!(classify("0123abcd", "deepgram"), Ok("deepgram"));
        assert_eq!(classify("0123abcd", "openai"), Ok("openai"));
        assert!(
            classify("sk-ant-api03-xyz", "openai")
                .unwrap_err()
                .contains("Anthropic")
        );
        assert!(classify("0123abcd", "mystery").is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(
            clean("  Arm for five\n seconds.  "),
            "Arm for five seconds."
        );
        assert_eq!(extension("audio/webm;codecs=opus"), "webm");
        assert_eq!(extension("audio/mp4"), "mp4");
        assert_eq!(
            error_detail(r#"{"error":{"message":"bad key"}}"#),
            "bad key"
        );
        assert_eq!(error_detail(r#"{"err_msg":"nope"}"#), "nope");
        assert_eq!(error_detail("plain"), "plain");
    }

    #[test]
    fn empty_recording_is_refused_before_any_request() {
        assert!(transcribe(&[], "audio/webm").is_err());
    }
}
