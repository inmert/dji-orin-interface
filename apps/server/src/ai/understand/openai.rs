//! OpenAI as the understanding model: `POST /v1/chat/completions` with the
//! plan schema as a strict JSON-schema response format. The status check
//! asks `GET /v1/models/{model}` to prove the key; OpenAI's API exposes no
//! balance.

use super::{Provider, Status, instructions, plan_from_json, schema};
use crate::ai::online::{agent, error_detail, read};
use dji_commands::Plan;

const API: &str = "https://api.openai.com/v1";

pub struct OpenAi {
    key: String,
    model: String,
}

impl OpenAi {
    /// With `model`, or the default `gpt-4o-mini`.
    pub fn new(key: String, model: Option<&str>) -> Self {
        let model = model.unwrap_or("gpt-4o-mini").to_string();
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
        let mut resp = agent()
            .post(&format!("{API}/chat/completions"))
            .header("Authorization", &self.auth())
            .header("Content-Type", "application/json")
            .send(body(&self.model, text).to_string().as_bytes())
            .map_err(|e| format!("OpenAI request failed: {e}"))?;
        let (status, text) = read(&mut resp)?;
        plan_from_response("OpenAI", status, &text)
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

/// The chat-completions request for `text`: the instructions as the system
/// message and the plan schema as a strict JSON-schema response format.
/// Shared with the local server ([`super::local`]), which speaks the same
/// API.
pub(super) fn body(model: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": instructions()},
            {"role": "user", "content": text}
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "plan", "strict": true, "schema": schema()}
        }
    })
}

/// The plan out of a chat-completions response (`who` names the service in
/// errors): the HTTP status, a refusal, then the message content as JSON.
pub(super) fn plan_from_response(who: &str, status: u16, text: &str) -> Result<Plan, String> {
    if status != 200 {
        return Err(format!("{who}: HTTP {status}: {}", error_detail(text)));
    }
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("{who} response: {e}"))?;
    if let Some(r) = v["choices"][0]["message"]["refusal"].as_str() {
        return Err(format!("{who} declined: {r}"));
    }
    let json = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| format!("{who} response without content"))?;
    plan_from_json(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dji_commands::Command;

    #[test]
    fn request_and_response_shapes() {
        let b = body("m", "arm");
        assert_eq!(b["model"], "m");
        assert_eq!(b["messages"][1]["content"], "arm");
        assert_eq!(b["response_format"]["json_schema"]["strict"], true);
        let ok = r#"{"choices":[{"message":{"content":"{\"steps\":[{\"cmd\":\"arm\",\"seconds\":null,\"camera\":null,\"direction\":null,\"on\":null}],\"problem\":null}"}}]}"#;
        assert_eq!(
            plan_from_response("X", 200, ok).unwrap().steps,
            [Command::Arm]
        );
        assert_eq!(
            plan_from_response("X", 401, r#"{"error":{"message":"bad key"}}"#).unwrap_err(),
            "X: HTTP 401: bad key"
        );
        assert_eq!(
            plan_from_response("X", 200, r#"{"choices":[{"message":{"refusal":"no"}}]}"#)
                .unwrap_err(),
            "X declined: no"
        );
        assert!(plan_from_response("X", 200, r#"{"choices":[]}"#).is_err());
    }
}
