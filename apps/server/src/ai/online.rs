//! What the online services share: the API keys in `secrets/` and a few HTTP
//! helpers. Speech ([`super::voice`]) and understanding
//! ([`super::understand`]) build on this.
//!
//! Keys are read from the secrets directory (`config.toml` `[secrets] dir`,
//! `SECRETS_DIR`), one key per file,
//! and recognised by prefix (`sk-ant-admin…` Anthropic admin, `sk-ant-…`
//! Anthropic, `sk-…` OpenAI) or else by file name (`deepgram.key`), so a
//! key lands with its service whatever the file is called.
//! `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` and `DEEPGRAM_API_KEY` override.
//! The browser never sees a key: the server is the only caller.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use dji_common::logln;

/// How long one service call may take.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The keys found, by service.
#[derive(Default, Clone)]
pub struct Keys {
    pub openai: Option<String>,
    pub deepgram: Option<String>,
    pub anthropic: Option<String>,
    /// An Anthropic Admin API key, for the organisation's cost report.
    pub anthropic_admin: Option<String>,
}

static KEYS: OnceLock<Keys> = OnceLock::new();

/// Read the keys in `dir` once, at start. Without this call there are none
/// (tests), unless the environment names some.
pub fn init(dir: &Path) {
    let _ = KEYS.set(Keys::load(dir));
}

/// The keys [`init`] found.
pub fn keys() -> &'static Keys {
    KEYS.get_or_init(|| Keys::load(Path::new("")))
}

impl Keys {
    fn load(dir: &Path) -> Self {
        let mut keys = Self::default();
        let mut files: Vec<_> = std::fs::read_dir(dir)
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
                Some(service) => {
                    logln!("keys: {service} key from {}", path.display());
                    keys.set(service, key);
                }
                None => logln!(
                    "keys: {}: not a recognised key (name the file after its service); ignored",
                    path.display()
                ),
            }
        }
        for (var, service) in [
            ("ANTHROPIC_API_KEY", "anthropic"),
            ("OPENAI_API_KEY", "openai"),
            ("DEEPGRAM_API_KEY", "deepgram"),
        ] {
            if let Ok(k) = std::env::var(var)
                && !k.trim().is_empty()
            {
                keys.set(classify(k.trim(), service).unwrap_or(service), k.trim());
            }
        }
        keys
    }

    fn set(&mut self, service: &str, key: &str) {
        let slot = match service {
            "anthropic-admin" => &mut self.anthropic_admin,
            "anthropic" => &mut self.anthropic,
            "openai" => &mut self.openai,
            "deepgram" => &mut self.deepgram,
            _ => return,
        };
        *slot = Some(key.to_string());
    }
}

/// Which service a key belongs to: by prefix when the vendor has one, else
/// by the file (or variable) it came from.
fn classify(key: &str, hint: &str) -> Option<&'static str> {
    if key.starts_with("sk-ant-admin") {
        return Some("anthropic-admin");
    }
    if key.starts_with("sk-ant-") {
        return Some("anthropic");
    }
    if key.starts_with("sk-") {
        return Some("openai");
    }
    ["anthropic-admin", "anthropic", "openai", "deepgram"]
        .into_iter()
        .find(|s| hint.contains(s))
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

/// One line of plain text: extra whitespace collapsed.
pub fn clean(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_keys_by_prefix_then_file_name() {
        assert_eq!(
            classify("sk-ant-admin01-xyz", "openai"),
            Some("anthropic-admin")
        );
        assert_eq!(classify("sk-ant-api03-xyz", "openai"), Some("anthropic"));
        assert_eq!(classify("sk-proj-xyz", "anthropic"), Some("openai"));
        assert_eq!(classify("0123abcd", "deepgram"), Some("deepgram"));
        assert_eq!(classify("0123abcd", "mystery"), None);
    }

    #[test]
    fn helpers() {
        assert_eq!(
            clean("  Arm for five\n seconds.  "),
            "Arm for five seconds."
        );
        assert_eq!(
            error_detail(r#"{"error":{"message":"bad key"}}"#),
            "bad key"
        );
        assert_eq!(error_detail(r#"{"err_msg":"nope"}"#), "nope");
        assert_eq!(error_detail("plain"), "plain");
    }
}
