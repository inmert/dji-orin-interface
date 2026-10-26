//! dji-orin-interface — talk the DJI DUML protocol to a Cendence controller
//! from a Jetson Orin over USB, with no external crates.
//!
//! * [`duml`] — transport-agnostic DUML framing, checksums and reassembly.
//! * [`aoa`] — Android Open Accessory constants and the DJI accessory identity.
//! * [`usbfs`] — host-side USB (device lookup, control/bulk transfers).
//! * [`ffs`] — device-side FunctionFS (descriptors, ep0 events, setup packets).
//! * [`sys`] — raw kernel ABI (libc calls + usbfs ioctl structs/codes).
//! * [`log`] — timestamped line logging shared by the binaries.

pub mod aoa;
pub mod duml;
pub mod ffs;
pub mod log;
pub mod sys;
pub mod usbfs;
