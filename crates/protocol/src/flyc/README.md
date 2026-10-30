# flyc

The flight controller (DUML device `0x03`) as seen on the RC link and the USB
API port.

| File | Purpose |
|------|---------|
| `../flyc.rs` | command set/ids; `function_control_frame` (0x03/0x2a motor start/stop), `usb_flight_frame` (arm with USB connected), `motor_rpm` |
| `osd.rs` | the 0x03/0x43 OSD report: attitude, flight mode, satellites; position fields marked unverified |

Flight commands (take off, land, go home) are not built here yet; they are
single-byte task codes on the OSDK side (see `../osdk/command.rs`).
