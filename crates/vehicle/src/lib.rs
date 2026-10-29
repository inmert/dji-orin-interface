//! The aircraft as an object: typed capabilities over the links the Orin has
//! to it. Everything that *controls* the aircraft lives here; presentation
//! (the web portal, video) and the capture tools sit on top.
//!
//! * [`Vehicle`] — the shared telemetry snapshot plus one struct per feature:
//!   [`gimbal::Gimbal`] (moves, camera lock, recenter), [`thermal::Thermal`]
//!   (XT2 settings queue and auto range) and [`motors::Motors`] (start/stop,
//!   the USB flight unlock, live motor state).
//! * [`executor::Executor`] — runs a [`dji_commands::Plan`] (arm, wait,
//!   disarm…) against those features, one step at a time, with cancel and a
//!   motor stop on failure.
//! * [`links`] — the readers and writers that carry those features to the
//!   aircraft: the RC gadget's bulk endpoints ([`links::rc`]) and the serial
//!   ports ([`links::serial`]: OSDK UART and USB API port).
//!
//! Features hold *desired state* and *requests*; the link workers turn them
//! into frames and fold what comes back into the snapshot. A caller (the
//! portal today, a command executor later) only ever uses the typed methods.
//!
//! SAFETY: the gimbal methods move the real camera mount and the motor
//! requests spin the real motors, via the powered aircraft. Nothing is sent
//! until a method is called; run props-off / aircraft secured.

pub mod executor;
pub mod gimbal;
pub mod links;
pub mod mailbox;
pub mod motors;
pub mod thermal;

use std::sync::{Arc, Mutex};

use dji_protocol::Telemetry;

/// The shared link snapshot, written by the link readers and read by the UI.
pub type Tele = Arc<Mutex<Telemetry>>;

/// The aircraft and everything the Orin knows and wants of it.
#[derive(Default)]
pub struct Vehicle {
    pub telemetry: Tele,
    pub gimbal: gimbal::Gimbal,
    pub thermal: thermal::Thermal,
    pub motors: motors::Motors,
    pub executor: executor::Executor,
}

impl Vehicle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A copy of the telemetry snapshot.
    pub fn snapshot(&self) -> Telemetry {
        self.telemetry.lock().unwrap().clone()
    }
}
