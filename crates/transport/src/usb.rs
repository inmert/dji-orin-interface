//! USB transport plumbing shared by the host and device sides.
//!
//! * [`usbfs`] — host-side USB (device lookup, control/bulk transfers).
//! * [`ffs`] — device-side FunctionFS (descriptors, ep0 events, setup packets).
//! * [`read()`] / [`write()`] / [`errno`] — raw fd I/O for endpoint files.
//!
//! The raw kernel ABI (`sys`: libc externs, usbfs ioctl structs/codes) is
//! private; only the safe wrappers above are exported.
//!
//! Uses the modern path layout (`usb.rs` alongside `usb/`) rather than
//! `usb/mod.rs`.

pub mod ffs;
pub(crate) mod sys;
pub mod usbfs;

pub use sys::{errno, read, write};
