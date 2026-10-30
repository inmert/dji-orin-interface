# dji-commands

The command model: what a caller may ask the aircraft to do, independent of
how it is carried out or who asked (the portal, a voice front end, a model).

| File | Purpose |
|------|---------|
| `src/lib.rs` | `Command` (arm, disarm, wait, pan, tilt, recenter, link), `Plan`, `Limits` and `Plan::validate`, the JSON shape |
| `src/grammar.rs` | the deterministic sentence parser (`arm for 5 seconds then disarm`), the fallback behind every model |

Rule: every plan, whatever produced it, is checked against `Limits` before it
runs. New capabilities (flight steps) are new `Command` variants here first;
the vehicle then learns to carry them out.
