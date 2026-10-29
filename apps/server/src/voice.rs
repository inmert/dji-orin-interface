//! Voice transcription for the portal's hold-to-talk: the browser's
//! recording (whatever its `MediaRecorder` produced) is forwarded to an
//! online speech-recognition service and the text comes back. Nothing is
//! decoded on the Orin; it only validates and runs the resulting command.
//!
//! Providers ([`Provider`]): OpenAI's transcription API today. The key is
//! read from `STT_KEY_FILE` (default `secrets/openai.key`, one line) or
//! `OPENAI_API_KEY`; `STT_MODEL` picks the model (`gpt-4o-transcribe`).
//! The browser never sees the key: the server is the only caller.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_common::logln;

/// Words the commands use, given to the model as context so it prefers
/// them over sound-alikes.
const PROMPT: &str = "Drone commands: arm, disarm, stop, wait, seconds, pan, tilt, \
                      camera, left, right, up, down, recenter, link, unlink.";

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Serialize)]
pub struct Transcript {
    pub text: String,
    /// Round trip to the service, ms.
    pub ms: u64,
}

/// An online speech-recognition service.
pub enum Provider {
    OpenAi { key: String, model: String },
}

impl Provider {
    /// Transcribe one recording of media type `content_type`.
    fn transcribe(&self, audio: &[u8], content_type: &str) -> Result<String, String> {
        match self {
            Provider::OpenAi { key, model } => openai(key, model, audio, content_type),
        }
    }
}

fn provider() -> Option<&'static Provider> {
    static PROVIDER: OnceLock<Option<Provider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| {
            let key = std::env::var("OPENAI_API_KEY")
                .ok()
                .or_else(|| {
                    let file = std::env::var("STT_KEY_FILE")
                        .unwrap_or_else(|_| "secrets/openai.key".into());
                    std::fs::read_to_string(&file).ok()
                })
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())?;
            let model = std::env::var("STT_MODEL").unwrap_or_else(|_| "gpt-4o-transcribe".into());
            logln!("voice: online transcription with OpenAI {model}");
            Some(Provider::OpenAi { key, model })
        })
        .as_ref()
}

/// Transcribe `audio` (an upload of type `content_type`).
pub fn transcribe(audio: &[u8], content_type: &str) -> Result<Transcript, String> {
    if audio.is_empty() {
        return Err("empty recording".into());
    }
    let provider = provider().ok_or(
        "no speech-recognition key: put an OpenAI API key in secrets/openai.key (or set OPENAI_API_KEY) and restart",
    )?;
    let start = Instant::now();
    let text = clean(&provider.transcribe(audio, content_type)?);
    let ms = start.elapsed().as_millis() as u64;
    logln!("voice: {ms} ms, {} bytes: {text:?}", audio.len());
    Ok(Transcript { text, ms })
}

/// File extension for the upload's media type (the service sniffs the
/// content; the name only helps it along).
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

/// A `multipart/form-data` body with text fields and one file part.
fn multipart(boundary: &str, fields: &[(&str, &str)], file: (&str, &str, &str, &[u8])) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    let (name, filename, kind, data) = file;
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n\
             Content-Type: {kind}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// OpenAI `POST /v1/audio/transcriptions`.
fn openai(key: &str, model: &str, audio: &[u8], content_type: &str) -> Result<String, String> {
    let boundary = format!("dji-voice-{}", std::process::id());
    let kind = content_type
        .split(';')
        .next()
        .unwrap_or("audio/webm")
        .trim();
    let filename = format!("speech.{}", extension(content_type));
    let body = multipart(
        &boundary,
        &[
            ("model", model),
            ("language", "en"),
            ("prompt", PROMPT),
            ("response_format", "json"),
        ],
        ("file", &filename, kind, audio),
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let mut resp = agent
        .post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", &format!("Bearer {key}"))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send(&body[..])
        .map_err(|e| format!("transcription request failed: {e}"))?;
    let status = resp.status();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("transcription response unreadable: {e}"))?;
    if !status.is_success() {
        let detail = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or(text);
        return Err(format!("transcription service: {status}: {detail}"));
    }
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("transcription response: {e}"))?;
    json["text"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| format!("transcription response without text: {text}"))
}

/// One line of plain text: extra whitespace collapsed.
fn clean(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_multipart_body_and_cleans_text() {
        let body = multipart(
            "B",
            &[("model", "m")],
            ("file", "speech.webm", "audio/webm", b"\x1a\x45"),
        );
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(
            "--B\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nm\r\n--B\r\n"
        ));
        assert!(text.contains(
            "filename=\"speech.webm\"\r\nContent-Type: audio/webm\r\n\r\n\x1a\x45\r\n--B--\r\n"
        ));
        assert_eq!(
            clean("  Arm for five\n seconds.  "),
            "Arm for five seconds."
        );
        assert_eq!(extension("audio/webm;codecs=opus"), "webm");
        assert_eq!(extension("audio/mp4"), "mp4");
    }

    #[test]
    fn empty_recording_is_refused_before_any_request() {
        assert!(transcribe(&[], "audio/webm").is_err());
    }
}
