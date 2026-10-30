//! Voice input for the portal's hold-to-talk: the browser's recording is
//! forwarded to an online speech-recognition service and the text comes
//! back. Nothing is decoded on the Orin; it only validates and runs the
//! resulting command.
//!
//! Providers sit behind [`Provider`] so the service can be chosen or swapped
//! without touching the callers: [`openai`] (`gpt-4o-transcribe`) and
//! [`deepgram`] (`nova-3`). Keys come from [`crate::online`] (`secrets/`);
//! `STT_PROVIDER` picks the provider, by default the first configured of
//! `openai`, `deepgram`.
//!
//! [`statuses`] checks each key against its service and reports spend or
//! balance where the service exposes one.

pub mod deepgram;
pub mod openai;

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_common::logln;

pub use crate::online::{agent, clean, error_detail, read};

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
        let keys = crate::online::keys();
        let mut providers: Vec<Box<dyn Provider>> = Vec::new();
        if let Some(k) = keys.openai.clone() {
            providers.push(Box::new(openai::OpenAi::new(k)));
        }
        if let Some(k) = keys.deepgram.clone() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_extensions() {
        assert_eq!(extension("audio/webm;codecs=opus"), "webm");
        assert_eq!(extension("audio/mp4"), "mp4");
    }

    #[test]
    fn empty_recording_is_refused_before_any_request() {
        assert!(transcribe(&[], "audio/webm").is_err());
    }
}
