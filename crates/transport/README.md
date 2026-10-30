# dji-transport

Getting bytes to and from the hardware. Linux only, no crates: the kernel
ABIs (usbfs, FunctionFS, termios) are declared by hand in `usb/sys.rs` and
`serial.rs`. All `unsafe` lives here, wrapped in safe types that own their
file descriptors.

| Module | Purpose |
|--------|---------|
| `usb.rs`, `usb/` | endpoint read/write and errno; `usbfs.rs` (Orin as USB host), `ffs.rs` (Orin as gadget via FunctionFS), `sys.rs` (raw ABI, private) |
| `aoa.rs`, `aoa/` | Android Open Accessory: constants and DJI's identity; `host.rs` (toward a tablet), `device.rs` (toward the RC) |
| `serial.rs` | serial ports: `RxPort` (opened read-only, cannot send) and `TxPort` (one command at a time) |

Rule: no protocol knowledge. This crate moves bytes; `dji-protocol` gives
them meaning and `dji-vehicle` decides what to send.
