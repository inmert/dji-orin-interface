//! dji-orin-interface — talk the DJI DUML protocol to a Cendence controller
//! from a Jetson Orin over USB.
//!
//! Organized by layer, with one module per aircraft feature:
//!
//! * Transport — [`usb`] (`sys`, `usbfs`, `ffs`) and [`aoa`] (the Android Open
//!   Accessory handshake, as `host` or `device`).
//! * Protocol — [`duml`]: transport-agnostic framing, checksums, reassembly.
//! * Features — [`gimbal`] (camera-mount commands + attitude reports). New
//!   aircraft features (e.g. props/motors, camera settings) get a sibling
//!   module of their own with the same `command` / report split.
//! * [`telemetry`] — the serializable link snapshot for the web UI, folding
//!   in each feature's decoded reports.
//! * [`log`] — timestamped line logging shared by the binaries.

pub mod aoa;
pub mod duml;
pub mod gimbal;
pub mod log;
pub mod telemetry;
pub mod usb;
