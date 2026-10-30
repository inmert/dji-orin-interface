# motors

Motor start/stop and the USB flight unlock.

| File | Purpose |
|------|---------|
| `../motors.rs` | `Request` queue rules (a stop beats a start), `Status` for the UI, `perform` for the executor, the worker loop and the path choice |
| `rc.rs` | over the RC link: ask the RC writer to send 0x03/0x2a, match the FC's ACK by sequence; refused vs silent |
| `uart.rs` | over the OSDK UART: get version, activate `osdk_app_id`, obtain authority, act (start/stop, or the USB unlock on the API port), release authority |
| `live.rs` | the live armed state from the UART status or the RC's motor RPM, and whether the FC's USB lock is on |

The RC link is tried first; the UART only when the link is down or the FC
stays silent there. A refusal on either path is final.
