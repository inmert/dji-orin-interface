//! OpenAI as the understanding model: `POST /v1/chat/completions` with the
//! plan schema as a strict JSON-schema response format. The status check
//! asks `GET /v1/models/{model}` to prove the key; OpenAI's API exposes no
//! balance.

use super::{INSTRUCTIONS, Provider, Status, plan_from_json, schema};
use crate::online::{agent, error_detail, read};
use dji_commands::Plan;

const API: &str = "https://api.openai.com/v1";

pub struct OpenAi {
    key: String,
    model: String,
}

impl OpenAi {
    pub fn new(key: String) -> Self {
        let model = std::env::var("LLM_MODEL")
            .ok()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "gpt-4o-mini".into());
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

    fn model(&self) -> &str {
        &self.model
    }

    fn interpret(&self, text: &str) -> Result<Plan, String> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": INSTRUCTIONS},
                {"role": "user", "content": text}
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "plan", "strict": true, "schema": schema()}
            }
        });
        let mut resp = agent()
            .post(&format!("{API}/chat/completions"))
            .header("Authorization", &self.auth())
            .header("Content-Type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| format!("OpenAI request failed: {e}"))?;
        let (status, text) = read(&mut resp)?;
        if status != 200 {
            return Err(format!("OpenAI: HTTP {status}: {}", error_detail(&text)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("OpenAI response: {e}"))?;
        if let Some(r) = v["choices"][0]["message"]["refusal"].as_str() {
            return Err(format!("OpenAI declined: {r}"));
        }
        let json = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("OpenAI response without content")?;
        plan_from_json(json)
    }

    fn status(&self) -> Status {
        let mut st = Status {
            provider: self.name(),
            active: false,
            model: self.model.clone(),
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
