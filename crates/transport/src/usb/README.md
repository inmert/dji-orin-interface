# usb

| File | Purpose |
|------|---------|
| `../usb.rs` | `read` / `write` on an endpoint fd, `errno` constants the link workers match on (`ENODEV`, `ESHUTDOWN`) |
| `usbfs.rs` | host side: find a device by VID/PID, control and bulk transfers, interface claims |
| `ffs.rs` | gadget side: FunctionFS descriptors and strings, ep0 events (bind/enable/setup), endpoint opening |
| `sys.rs` | the raw ioctl/struct ABI both sides use; private to the crate |

Which side runs is a role choice: the server and `ffs_capture` are the
gadget (the Orin pretends to be the DJI app), `aoa_capture` and the bridge
are also a host toward a tablet.
