//! The DJI DUML protocol and the aircraft features built on it. Pure logic: no
//! I/O, no external dependencies (enable the `serde` feature to serialize
//! telemetry).
//!
//! * [`duml`] — transport-agnostic framing, checksums, reassembly, addressing.
//! * [`gimbal`] — camera-mount commands we inject + attitude reports.
//! * [`flyc`] — flight controller reports (attitude, flight mode).
//! * [`osdk`] — the Onboard SDK OpenProtocol (0xAA) the aircraft's OSDK UART
//!   speaks; decode-only, for its flight-data broadcast.
//! * [`telemetry`] — the link snapshot, folding in each feature's reports.
//!
//! New aircraft features (e.g. props/motors, camera settings) get a sibling
//! module with the same `command` / report split as [`gimbal`].

pub mod duml;
pub mod flyc;
pub mod gimbal;
pub mod osdk;
pub mod telemetry;

pub use duml::{Decoder, Device, Frame, Header};
pub use telemetry::{FlightLink, Telemetry, UsbLink};
