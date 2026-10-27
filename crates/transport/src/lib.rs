//! USB transport for talking DUML to DJI hardware from a Jetson Orin.
//!
//! * [`usb`] — host side ([`usb::usbfs`]) and gadget side ([`usb::ffs`]), plus
//!   the raw [`read`](usb::read)/[`write`](usb::write) used on endpoint fds.
//! * [`aoa`] — the Android Open Accessory handshake, as host or device.
//!
//! The kernel ABI (libc externs, ioctl structs) stays private to this crate.

pub mod aoa;
pub mod usb;
