//! The DJI DUML protocol and the aircraft features built on it. Pure logic: no
//! I/O, no external dependencies (enable the `serde` feature to serialize
//! telemetry).
//!
//! * [`duml`] — transport-agnostic framing, checksums, reassembly, addressing.
//! * [`gimbal`] — camera-mount commands we inject + attitude reports.
//! * [`flyc`] — flight controller reports (attitude, flight mode) and the
//!   motor-control / USB-flight frames.
//! * [`osdk`] — the Onboard SDK OpenProtocol (0xAA) the aircraft's OSDK UART
//!   speaks: its flight-data broadcast, and the few requests the motor path
//!   sends (activation, control authority, start/stop).
//! * [`rcmux`] — the RC's USB packet mux (DUML + plain H.264 camera video).
//! * [`thermal`] — the thermal camera's spot meter, display mode and report.
//! * [`telemetry`] — the link snapshot, folding in each feature's reports.
//!
//! New aircraft features (e.g. props/motors, camera settings) get a sibling
//! module with the same `command` / report split as [`gimbal`].

pub mod duml;
pub mod flyc;
pub mod gimbal;
pub mod osdk;
pub mod rcmux;
pub mod telemetry;
pub mod thermal;

pub use duml::{Decoder, Device, Frame, Header};
pub use telemetry::{FlightLink, Telemetry, UsbLink};
