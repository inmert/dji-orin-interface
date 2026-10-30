# aoa

The Android Open Accessory handshake that precedes DUML on USB.

| File | Purpose |
|------|---------|
| `../aoa.rs` | vendor requests, Google's accessory VID/PID, DJI's accessory strings (`DJI` / `T600` / `DJI Inspire 1`) |
| `host.rs` | Orin as the controller toward a DJI tablet: handshake, wait for re-enumeration, open the accessory |
| `device.rs` | Orin as the app toward the RC: answer the RC's setup requests on ep0 |
