# dji-protocol

The wire formats and the aircraft features built on them. Pure logic: no I/O,
no dependencies (`serde` is an optional feature so the server can serialize
telemetry). Everything here is testable from captured bytes.

| Module | Purpose |
|--------|---------|
| `duml.rs` | DUML (`0x55`) framing: CRCs, `parse` / `scan` / `build`, the streaming `Decoder`, device addressing |
| `rcmux.rs` | the RC's USB packet mux: outer frames and mux packets carrying DUML and the camera video channels |
| `gimbal.rs`, `gimbal/` | camera-mount feature: ids and addressing; `command.rs` (frames we inject), `attitude.rs` (reports) |
| `flyc.rs`, `flyc/` | flight controller: motor start/stop and USB-flight frames, `osd.rs` (0x03/0x43 report) |
| `osdk.rs`, `osdk/` | Onboard SDK OpenProtocol (`0xAA`) on the aircraft's UART: framing, `broadcast.rs`, `command.rs` |
| `thermal.rs` | Zenmuse XT2 thermal core: settings frames and the measurement report |
| `telemetry.rs` | the link snapshot the UI sees, folding in every feature's reports |
| `tests/`, `benches/`, `examples/` | integration tests on captures, CRC/scan benchmarks, decode tools (`rcmux_split`) |

Rule: a feature that goes both ways keeps the frames we send apart from the
reports the aircraft pushes (`gimbal/command.rs` vs `gimbal/attitude.rs`). A
new feature is a sibling `src/<feature>.rs` plus `src/<feature>/`. Units and
byte offsets that were verified on the aircraft say so in their doc comment;
unverified fields are marked provisional.
