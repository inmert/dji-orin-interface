# ai

The services behind the portal's command bar. Each role is a trait with
several providers behind it, chosen in `config.toml`; the Orin only ever
validates and runs what comes back.

| File | Purpose |
|------|---------|
| `online.rs` | API keys from `secrets/*.key` (classified by prefix or file name; env overrides), the HTTP agent and response helpers |
| `voice.rs`, `voice/` | speech recognition for hold-to-talk: the recording goes to an online service, text comes back |
| `understand.rs`, `understand/` | sentence → plan: a model asked for structured output against a fixed schema; the grammar in `dji-commands` is the fallback |

Keys never leave the server and are never logged.
