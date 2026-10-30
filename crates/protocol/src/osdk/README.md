# osdk

DJI's Onboard SDK "OpenProtocol" (`0xAA` frames) on the aircraft's SDK UART.
A different wire format from DUML.

| File | Purpose |
|------|---------|
| `../osdk.rs` | framing: header/frame CRCs, `parse`, the streaming `Decoder`, ACK frames |
| `broadcast.rs` | the flight-data broadcast (0x02/0x00) the FC pushes unasked: time, attitude, velocity, position, GPS, RC, gimbal, status, battery |
| `command.rs` | the requests the motor path sends (get version, activate, control authority, task start/stop) and their ACK codes |

Only what the server needs is implemented; the full command table is in
`~/repos/Onboard-SDK-3.9/osdk-core/api/src/dji_command.cpp`.
