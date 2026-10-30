# dji-commands

The command model: what a caller may ask the aircraft to do, independent of
how it is carried out or who asked (the portal, a voice front end, a model).

| File | Purpose |
|------|---------|
| `src/lib.rs` | `Command` (arm, disarm, wait, pan, tilt, recenter, link), `Plan`, `Limits` and `Plan::validate`, the JSON shape |
| `src/capability.rs` | the registry: one `Capability` per step kind (name, description, typed parameters, duration limit, how to build the `Command`); derives the models' JSON schema, their instructions and the decoding of their steps |
| `src/grammar.rs` | the deterministic sentence parser (`arm for 5 seconds then disarm`), the fallback behind every model |

Rule: every plan, whatever produced it, is checked against `Limits` before it
runs. **Adding a command** is one `Capability` entry in `capability.rs`, one
`Command` variant (with `Display` and `Command::capability`), and the arm in
the vehicle's executor that carries it out; the schema, the instructions and
the validation follow from the entry. The grammar is optional for new
commands: the models are the primary path, the grammar the offline fallback.
