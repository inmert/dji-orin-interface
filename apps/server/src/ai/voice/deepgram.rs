//! Deepgram speech recognition: `POST /v1/listen` with `nova-3`
//! (`[speech] model` overrides), the recording sent as the raw request body and
//! the command vocabulary as key terms. The status check lists the key's
//! projects to prove the key and reads the project balance.
//!
//! Written from Deepgram's published API shapes; not yet exercised against
//! the service from this project.

use super::{Provider, Status, VOCABULARY, agent, error_detail, read};

const API: &str = "https://api.deepgram.com/v1";

pub struct Deepgram {
    key: String,
    model: &'static str,
}

impl Deepgram {
    /// With `model`, or the default `nova-3`.
    pub fn new(key: String, model: Option<&str>) -> Self {
        let model: &'static str = match model {
            Some(m) => Box::leak(m.to_string().into_boxed_str()),
            None => "nova-3",
        };
        Self { key, model }
    }

    fn auth(&self) -> String {
        format!("Token {}", self.key)
    }

    fn get(&self, path: &str) -> Result<serde_json::Value, String> {
        let mut resp = agent()
            .get(&format!("{API}{path}"))
            .header("Authorization", &self.auth())
            .call()
            .map_err(|e| format!("unreachable: {e}"))?;
        let (status, text) = read(&mut resp)?;
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}: {}", error_detail(&text)));
        }
        serde_json::from_str(&text).map_err(|e| format!("response: {e}"))
    }
}

impl Provider for Deepgram {
    fn name(&self) -> &'static str {
        "deepgram"
    }

    fn transcribe(&self, audio: &[u8], content_type: &str) -> Result<String, String> {
        let kind = content_type
            .split(';')
            .next()
            .unwrap_or("audio/webm")
            .trim();
        let keyterms = VOCABULARY
            .iter()
            .map(|w| format!("&keyterm={w}"))
            .collect::<Vec<_>>()
            .join("");
        let url = format!(
            "{API}/listen?model={}&language=en&smart_format=true&punctuate=true{keyterms}",
            self.model
        );
        let mut resp = agent()
            .post(&url)
            .header("Authorization", &self.auth())
            .header("Content-Type", kind)
            .send(audio)
            .map_err(|e| format!("Deepgram request failed: {e}"))?;
        let (status, text) = read(&mut resp)?;
        if !(200..300).contains(&status) {
            return Err(format!("Deepgram: HTTP {status}: {}", error_detail(&text)));
        }
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Deepgram response: {e}"))?;
        json["results"]["channels"][0]["alternatives"][0]["transcript"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| format!("Deepgram response without transcript: {text}"))
    }

    fn status(&self) -> Status {
        let mut st = Status {
            provider: self.name(),
            active: false,
            model: self.model,
            key_ok: None,
            balance: None,
            note: String::new(),
        };
        let projects = match self.get("/projects") {
            Ok(v) => v,
            Err(e) => {
                st.key_ok = Some(!e.starts_with("unreachable"));
                st.note = e;
                if st.key_ok == Some(true) {
                    // Reached the service and it refused: the key is the problem.
                    st.key_ok = Some(false);
                }
                return st;
            }
        };
        st.key_ok = Some(true);
        let Some(id) = projects["projects"][0]["project_id"].as_str() else {
            st.note = "the key has no project".into();
            return st;
        };
        match self.get(&format!("/projects/{id}/balances")) {
            Ok(v) => {
                let total: f64 = v["balances"]
                    .as_array()
                    .map(|b| b.iter().filter_map(|x| x["amount"].as_f64()).sum())
                    .unwrap_or(0.0);
                st.balance = Some(format!("${total:.2} left"));
            }
            Err(e) => st.note = format!("balance: {e}"),
        }
        st
    }
}
