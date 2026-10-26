//! USB transport plumbing shared by the host and device sides.
//!
//! * [`sys`] — raw kernel ABI (libc calls + usbfs ioctl structs/codes).
//! * [`usbfs`] — host-side USB (device lookup, control/bulk transfers).
//! * [`ffs`] — device-side FunctionFS (descriptors, ep0 events, setup packets).
//!
//! Uses the modern path layout (`usb.rs` alongside `usb/`) rather than
//! `usb/mod.rs`.

pub mod ffs;
pub mod sys;
pub mod usbfs;
