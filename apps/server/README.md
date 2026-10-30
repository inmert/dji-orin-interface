# dji-server

The web control server: the Orin as the DJI app toward the RC, with a browser
UI. Presentation only; the aircraft is `dji-vehicle`.

| Module | Purpose |
|--------|---------|
| `main.rs` | start-up: load settings, build the `Vehicle`, start the workers and listeners, then the gadget loop |
| `config.rs` | `config.toml` and the environment overrides; the only place the environment is read |
| `gadget.rs` | the FunctionFS ep0 event loop; starts the RC link workers on ENABLE, respawns ones that exited |
| `ports.rs` | default serial ports for the direct readers, chosen again on every reopen |
| `video.rs` | camera video: RC mux / UDT H.264 → NVDEC (`gst-launch-1.0`) → MJPEG, decoding only while watched |
| `web.rs`, `web/` | the browser side: HTTP(S), the WebSocket, the portal's messages, the TLS certificate |
| `ai.rs`, `ai/` | the command bar's services: speech recognition and sentence-to-plan models |

Run it through `scripts/run_server.sh` (root, sets up the gadget); settings
in `config.toml` at the repository root. Tests: `cargo test -p dji-server`;
the ignored ones need hardware or the online services and say so.
