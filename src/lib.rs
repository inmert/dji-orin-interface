//! dji-orin-interface — talk the DJI DUML protocol to a Cendence controller
//! from a Jetson Orin over USB.
//!
//! * [`duml`] — transport-agnostic DUML framing and the two DJI checksums.
//! * [`sys`] — thin, dependency-free libc wrappers (RAII fds, usbfs ioctls).

pub mod duml;
pub mod sys;
