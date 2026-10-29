//! The links to the aircraft, each a reader (and where it sends, a writer)
//! that runs on its own thread against a [`Vehicle`](crate::Vehicle):
//!
//! * [`rc`] — the RC's USB bulk endpoints on the Orin's gadget: telemetry and
//!   camera video in, gimbal/thermal/motor frames out.
//! * [`serial`] — the aircraft's own ports, read-only: the OSDK UART
//!   broadcast and the DUML reports on the USB API port.

pub mod rc;
pub mod serial;
