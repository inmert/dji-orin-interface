//! Shared utilities for the dji-orin-interface crates.
//!
//! * [`log`] — timestamped line logging (the [`logln!`] macro).
//! * [`Hex`] / [`hex`] — byte formatting for logs and telemetry.

pub mod log;

pub use log::{Hex, hex};
