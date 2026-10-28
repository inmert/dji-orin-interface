//! Flight controller feature: the reports the FC pushes up over DUML.
//!
//! * [`osd`] — the `0x03/0x43` general-data report (attitude, flight mode,
//!   satellites), sent once a second on the aircraft's USB API port.
//!
//! No commands live here yet; flight commands actuate the aircraft and are
//! still to be captured (see `docs/protocol.md`).

pub mod osd;

pub use osd::Osd;

/// Flight controller command set.
pub const CMD_SET: u8 = 0x03;
/// General ("OSD") data report.
pub const CMD_ID_OSD: u8 = 0x43;
