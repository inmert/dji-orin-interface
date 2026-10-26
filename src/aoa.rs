//! Android Open Accessory (AOA) constants and the DJI accessory identity.
//!
//! The AOA host (the Cendence controller, or `aoa_capture` impersonating it)
//! sends vendor control requests to the device, which then (re-)enumerates
//! with Google's accessory VID/PID and two bulk endpoints.

/// `bmRequestType` for AOA requests: vendor, device recipient.
pub const REQUEST_TYPE_IN: u8 = 0xC0;
pub const REQUEST_TYPE_OUT: u8 = 0x40;

/// Device → host: 2-byte little-endian protocol version.
pub const GET_PROTOCOL: u8 = 51;
/// Host → device: one identity string, `wIndex` = field index.
pub const SEND_STRING: u8 = 52;
/// Host → device: switch to accessory mode.
pub const START: u8 = 53;

/// The protocol version we report when acting as the device.
pub const PROTOCOL_VERSION: u16 = 2;

pub const GOOGLE_VID: u16 = 0x18d1;
/// Accessory mode.
pub const ACCESSORY_PID: u16 = 0x2d00;
/// Accessory mode + ADB.
pub const ACCESSORY_ADB_PID: u16 = 0x2d01;

/// Names of the `SEND_STRING` fields, by `wIndex`.
pub const STRING_FIELDS: [&str; 6] = [
    "manufacturer",
    "model",
    "description",
    "version",
    "uri",
    "serial",
];

/// The accessory identity the real Cendence controller announces (captured);
/// `DJI` / `T600` is what DJI Pilot registers to handle.
pub const DJI_IDENTITY: [&str; 6] = [
    "DJI",
    "T600",
    "DJI Inspire 1",
    "v1.0.2.6",
    "www.dji.com",
    "000000000000000",
];

/// Name of an AOA vendor request, for logs.
pub fn request_name(request: u8) -> Option<&'static str> {
    match request {
        GET_PROTOCOL => Some("GET_PROTOCOL"),
        SEND_STRING => Some("SEND_STRING"),
        START => Some("START"),
        _ => None,
    }
}
