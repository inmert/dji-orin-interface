# gimbal

The camera mounts (gimbal device index 0 = Zenmuse XT2, index 2 = Zenmuse Z30).

| File | Purpose |
|------|---------|
| `../gimbal.rs` | `Camera` (1 or 2 ↔ device index), `GimbalMove`, addressing shared by both halves |
| `command.rs` | frames the server injects: move (pitch/yaw rates), camera lock on/off, yaw recenter; the command-type byte (`set_cmd_type`, from `[gimbal] cmd_type`) |
| `attitude.rs` | the attitude report decode (0.1° units; yaw verified on the aircraft) |

Commands and reports stay in separate files on purpose: one side is what we
are allowed to send, the other what the aircraft tells us.
