//! dji-orin-interface — talk the DJI DUML protocol to a Cendence controller
//! from a Jetson Orin over USB, with no external crates.
//!
//! * [`duml`] — transport-agnostic DUML framing, checksums and reassembly.
//! * [`aoa`] — Android Open Accessory constants and the DJI accessory identity.
//! * [`usb`] — USB transport plumbing (`sys`, `usbfs`, `ffs` submodules).
//! * [`log`] — timestamped line logging shared by the binaries.

pub mod aoa;
pub mod duml;
pub mod log;
pub mod usb;
