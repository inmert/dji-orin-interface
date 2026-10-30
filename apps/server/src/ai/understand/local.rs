//! A model running on the Orin as the understanding model, behind an
//! OpenAI-compatible server (llama.cpp's `llama-server`, Ollama): the same
//! `POST /v1/chat/completions` request as [`super::openai`], with its plan
//! schema as the response format, which `llama-server` turns into a grammar
//! so the output is always valid JSON of that shape. No key, nothing leaves
//! the Orin; the model loaded in the server is the model.
//!
//! `config.toml` `[understanding.local]` (`LOCAL_LLM_URL`, `LOCAL_LLM_MODEL`)
//! gives the server's base URL (default `http://127.0.0.1:8081`) and the
//! model name sent (default: the first the server lists, so a renamed GGUF
//! needs no configuration). The status check asks
//! `GET /v1/models` to prove the server is up and shows the model it holds.
//! `scripts/run_local_llm.sh` starts `llama-server` with the settings used
//! for the measurements in the README.

use super::{Provider, Status, openai, schema_compact};
use crate::ai::online::{agent, error_detail, read};
use dji_commands::Plan;

/// Steps the schema allows fit in far fewer tokens; a cap keeps a runaway
/// generation from holding the GPU.
const MAX_TOKENS: u32 = 512;

pub struct Local {
    url: String,
    model: String,
}

impl Local {
    /// The server at `url`, with `model` or, unset, the model it reports
    /// (`"local"` when it cannot be asked yet).
    pub fn new(url: String, model: Option<String>) -> Self {
        let url = url.trim_end_matches('/').to_string();
        let model = model
            .filter(|m| !m.is_empty())
            .or_else(|| first_model(&url).ok())
            .unwrap_or_else(|| "local".into());
        Self { url, model }
    }
}

/// The id of the first model `GET /v1/models` lists.
fn first_model(url: &str) -> Result<String, String> {
    let mut resp = agent()
        .get(&format!("{url}/v1/models"))
        .call()
        .map_err(|e| format!("unreachable: {e}"))?;
    let (status, text) = read(&mut resp)?;
    if status != 200 {
        return Err(format!("HTTP {status}: {}", error_detail(&text)));
    }
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    v["data"][0]["id"]
        .as_str()
        .map(|id| {
            // llama-server reports the GGUF path; its file name is the model.
            id.rsplit('/').next().unwrap_or(id).to_string()
        })
        .ok_or("the server lists no model".into())
}

impl Provider for Local {
    fn name(&self) -> &'static str {
        "local"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn interpret(&self, text: &str) -> Result<Plan, String> {
        let mut body = openai::body(&self.model, text);
        // The grammar lets a step omit its unused fields: fewer tokens.
        body["response_format"]["json_schema"]["schema"] = schema_compact();
        // Deterministic and bounded; the schema does the rest.
        body["temperature"] = serde_json::json!(0);
        body["max_tokens"] = serde_json::json!(MAX_TOKENS);
        let mut resp = agent()
            .post(&format!("{}/v1/chat/completions", self.url))
            .header("Content-Type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| format!("local model request failed: {e}"))?;
        let (status, text) = read(&mut resp)?;
        openai::plan_from_response("local model", status, &text)
    }

    fn status(&self) -> Status {
        let mut st = Status {
            provider: self.name(),
            active: false,
            model: self.model.clone(),
            key_ok: None,
            balance: None,
            note: format!("on the Orin at {}; no key, no cost", self.url),
        };
        match first_model(&self.url) {
            Ok(loaded) => {
                st.key_ok = Some(true);
                if loaded != self.model {
                    st.note = format!("{}; the server holds {loaded}", st.note);
                }
            }
            Err(e) => {
                st.key_ok = Some(false);
                st.note = format!("{e} (start it with scripts/run_local_llm.sh)");
            }
        }
        st
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_model_from_the_environment_or_the_server() {
        // Nothing listens on this port: the name falls back.
        let l = Local::new("http://127.0.0.1:9/".into(), None);
        assert_eq!((l.url.as_str(), l.model()), ("http://127.0.0.1:9", "local"));
        let l = Local::new("http://127.0.0.1:9".into(), Some("llama".into()));
        assert_eq!(l.model(), "llama");
        let st = l.status();
        assert_eq!(st.key_ok, Some(false));
        assert!(st.note.contains("unreachable"), "{}", st.note);
        assert!(l.interpret("arm").is_err());
    }
}
