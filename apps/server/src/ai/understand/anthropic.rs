//! Claude as the understanding model: `POST /v1/messages` with the plan
//! schema as structured output (`output_config.format`), low effort, and
//! Anthropic's server-side refusal fallback. The status check asks
//! `GET /v1/models/{model}` to prove the key; month-to-date spend comes
//! from the organisation's cost report, which needs an Admin API key
//! (`sk-ant-admin…`, in `secrets/` too) — a workspace key is refused there.

use super::{INSTRUCTIONS, Provider, Status, plan_from_json, schema};
use crate::ai::online::{agent, error_detail, read};
use dji_commands::Plan;

const API: &str = "https://api.anthropic.com/v1";
const VERSION: &str = "2026-10-21";

pub struct Anthropic {
    key: String,
    admin_key: Option<String>,
    model: String,
}

impl Anthropic {
    /// With `model`, or the default `claude-opus-5-5`.
    pub fn new(key: String, admin_key: Option<String>, model: Option<&str>) -> Self {
        let model = model.unwrap_or("claude-opus-5-5").to_string();
        Self {
            key,
            admin_key,
            model,
        }
    }

    fn get(&self, key: &str, url: &str) -> Result<(u16, String), String> {
        let mut resp = agent()
            .get(url)
            .header("x-api-key", key)
            .header("anthropic-version", VERSION)
            .call()
            .map_err(|e| format!("unreachable: {e}"))?;
        read(&mut resp)
    }

    /// Month-to-date cost in USD from the cost report (admin key).
    fn spend(&self, admin_key: &str) -> Result<String, String> {
        let date = |args: &[&str]| -> Result<String, String> {
            let out = std::process::Command::new("date")
                .args(args)
                .output()
                .map_err(|e| e.to_string())?;
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        let from = date(&["-u", "+%Y-%m-01T00:00:00Z"])?;
        let until = date(&["-u", "-d", "tomorrow", "+%Y-%m-%dT00:00:00Z"])?;
        let url = format!("{API}/organizations/cost_report?starting_at={from}&ending_at={until}");
        let (status, text) = self.get(admin_key, &url)?;
        if status != 200 {
            return Err(format!("HTTP {status}: {}", error_detail(&text)));
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        // Amounts are decimal strings in cents.
        let cents: f64 = v["data"]
            .as_array()
            .map(|buckets| {
                buckets
                    .iter()
                    .flat_map(|b| b["results"].as_array().cloned().unwrap_or_default())
                    .filter_map(|r| r["amount"].as_str().and_then(|a| a.parse::<f64>().ok()))
                    .sum()
            })
            .unwrap_or(0.0);
        Ok(format!("${:.2} this month", cents / 100.0))
    }
}

impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn interpret(&self, text: &str) -> Result<Plan, String> {
        let body = serde_json::json!({
            "model": self.model,
            // Thinking tokens count against this; a tight cap truncates the plan.
            "max_tokens": 8192,
            "system": INSTRUCTIONS,
            "messages": [{"role": "user", "content": text}],
            "output_config": {"effort": "low", "format": {"type": "json_schema", "schema": schema()}},
            "fallbacks": "default"
        });
        let mut resp = agent()
            .post(&format!("{API}/messages"))
            .header("x-api-key", &self.key)
            .header("anthropic-version", VERSION)
            .header("anthropic-beta", "server-side-fallback-2026-07-01")
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| format!("Claude request failed: {e}"))?;
        let (status, text) = read(&mut resp)?;
        if status != 200 {
            return Err(format!("Claude: HTTP {status}: {}", error_detail(&text)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Claude response: {e}"))?;
        if v["stop_reason"] == "refusal" {
            return Err(format!(
                "Claude declined: {}",
                v["stop_details"]["explanation"]
                    .as_str()
                    .unwrap_or("no explanation")
            ));
        }
        if v["stop_reason"] != "end_turn" {
            return Err(format!(
                "Claude stopped early ({}); the plan would be incomplete",
                v["stop_reason"].as_str().unwrap_or("unknown")
            ));
        }
        let json = v["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .ok_or("Claude response without text")?;
        plan_from_json(json)
    }

    fn status(&self) -> Status {
        let mut st = Status {
            provider: self.name(),
            active: false,
            model: self.model.clone(),
            key_ok: None,
            balance: None,
            note: String::new(),
        };
        match self.get(&self.key, &format!("{API}/models/{}", self.model)) {
            Ok((200, _)) => st.key_ok = Some(true),
            Ok((status, text)) => {
                st.key_ok = Some(false);
                st.note = format!("HTTP {status}: {}", error_detail(&text));
                return st;
            }
            Err(e) => {
                st.note = e;
                return st;
            }
        }
        match &self.admin_key {
            Some(k) => match self.spend(k) {
                Ok(s) => st.balance = Some(s),
                Err(e) => st.note = format!("spend: {e}"),
            },
            None => {
                st.note = "spend needs an Admin API key (sk-ant-admin…) in secrets/; prepaid balance is not exposed by the API".into()
            }
        }
        st
    }
}
