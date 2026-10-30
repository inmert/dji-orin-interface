# links

The readers and writers between the `Vehicle` and the aircraft, one thread
each, started by the server.

| File | Purpose |
|------|---------|
| `rc.rs` | the RC gadget: `read` ep1 (DUML telemetry, motor ACKs, camera video channels to a sink, optional raw dump), `write` ep2 at 10 Hz (gimbal moves, lock, recenters, motor frames, thermal settings) |
| `serial.rs` | the aircraft's own ports, opened read-only: the OSDK broadcast on the UART and the DUML reports on the USB API port; reopened when a device goes away |

The RC link is the primary command path; the serial links are observers
whose ACKs feed the motors' mailbox when the UART path is used.
