//! OpenAI speech recognition: `POST /v1/audio/transcriptions` with
//! `gpt-4o-transcribe` (`STT_MODEL` overrides), given the command vocabulary
//! as a prompt. OpenAI's API exposes no balance; the status check asks
//! `GET /v1/models` to prove the key.

use super::{Provider, Status, VOCABULARY, agent, error_detail, extension, read};

const API: &str = "https://api.openai.com/v1";

pub struct OpenAi {
    key: String,
    model: &'static str,
}

impl OpenAi {
    pub fn new(key: String) -> Self {
        let model: &'static str = match std::env::var("STT_MODEL") {
            Ok(m) if !m.is_empty() => Box::leak(m.into_boxed_str()),
            _ => "gpt-4o-transcribe",
        };
        Self { key, model }
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.key)
    }
}

impl Provider for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn transcribe(&self, audio: &[u8], content_type: &str) -> Result<String, String> {
        let boundary = format!("dji-voice-{}", std::process::id());
        let kind = content_type
            .split(';')
            .next()
            .unwrap_or("audio/webm")
            .trim();
        let filename = format!("speech.{}", extension(content_type));
        let prompt = format!("Drone commands: {}.", VOCABULARY.join(", "));
        let body = multipart(
            &boundary,
            &[
                ("model", self.model),
                ("language", "en"),
                ("prompt", &prompt),
                ("response_format", "json"),
            ],
            ("file", &filename, kind, audio),
        );
        let mut resp = agent()
            .post(&format!("{API}/audio/transcriptions"))
            .header("Authorization", &self.auth())
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body[..])
            .map_err(|e| format!("OpenAI request failed: {e}"))?;
        let (status, text) = read(&mut resp)?;
        if !(200..300).contains(&status) {
            return Err(format!("OpenAI: HTTP {status}: {}", error_detail(&text)));
        }
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("OpenAI response: {e}"))?;
        json["text"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| format!("OpenAI response without text: {text}"))
    }

    fn status(&self) -> Status {
        let mut st = Status {
            provider: self.name(),
            active: false,
            model: self.model,
            key_ok: None,
            balance: None,
            note: "OpenAI's API exposes no balance; see platform.openai.com/usage".into(),
        };
        match agent()
            .get(&format!("{API}/models/{}", self.model))
            .header("Authorization", &self.auth())
            .call()
        {
            Ok(mut resp) => match read(&mut resp) {
                Ok((200, _)) => st.key_ok = Some(true),
                Ok((status, text)) => {
                    st.key_ok = Some(false);
                    st.note = format!("HTTP {status}: {}", error_detail(&text));
                }
                Err(e) => st.note = e,
            },
            Err(e) => st.note = format!("unreachable: {e}"),
        }
        st
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_multipart_body() {
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
    }
}
