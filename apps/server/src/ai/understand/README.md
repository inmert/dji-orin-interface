# understanding providers

One file per model service, each implementing `understand::Provider`
(`interpret` → `Plan`, `status`). Every provider is asked for the same JSON
schema (`understand::schema`) and its answer goes through
`understand::plan_from_json` and then `Plan::validate` on the Orin.

| File | Service | Default model |
|------|---------|---------------|
| `anthropic.rs` | Anthropic Messages API, `output_config.format` structured output | `claude-opus-5-5` |
| `openai.rs` | OpenAI chat completions, strict JSON schema; its request/response helpers are shared | `gpt-4o-mini` |
| `local.rs` | an OpenAI-compatible server on the Orin (`llama-server`, started by `scripts/run_local_llm.sh`), schema → grammar | whatever the server loaded |
| `bench.rs` | ignored test: latency and accuracy of every configured provider on a fixed sentence set | |

The provider order (anthropic, openai, local) is the default preference;
`[understanding] provider` picks one explicitly.
