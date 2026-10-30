# dji-vehicle

The aircraft as an object: typed capabilities over the links the Orin has to
it. Everything that controls the aircraft lives here; presentation (the web
portal, video) sits on top and only ever calls the typed methods.

| Module | Purpose |
|--------|---------|
| `lib.rs` | `Vehicle`: the telemetry snapshot plus one struct per feature |
| `gimbal.rs` | desired move per camera, the native camera lock, yaw recenters |
| `thermal.rs`, `thermal/` | XT2 settings queue; `auto.rs` fits the isotherm band to the scene |
| `motors.rs`, `motors/` | motor requests and status; `rc.rs` (RC link path), `uart.rs` (OSDK path, USB unlock), `live.rs` (armed and USB-lock state) |
| `executor.rs` | runs a `Plan` step by step: progress, cancel, motors stopped on failure |
| `mailbox.rs` | ACKs from the readers, matched by sequence for a waiting request |
| `links.rs`, `links/` | the workers that carry the features to the aircraft: `rc.rs` (gadget endpoints), `serial.rs` (OSDK UART, USB API port, read-only) |

Features hold desired state and requests; the link workers turn them into
frames and fold the replies back. Nothing is sent until a method is called.

SAFETY: these methods move the real camera mount and spin the real motors
through the powered aircraft. Props off unless flying.
