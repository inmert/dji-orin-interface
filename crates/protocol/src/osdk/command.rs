//! OpenProtocol requests the server sends on the OSDK UART, and the ACK codes
//! they come back with (values from Onboard SDK 3.9 `dji_error.cpp`).
//!
//! Every request goes out unencrypted on session 2 (the FC ACKs it with the
//! same sequence number). The sequence for a motor command is: get version,
//! activate, obtain control authority, start/stop motors, release authority.
//!
//! **Every one of these but [`get_version`] changes the aircraft's state, and
//! [`motors`] spins the motors.** Send them only on an explicit request, props
//! off unless flying.

use super::{HEADER_LEN, SOF, crc16, crc32};

/// The session that asks the FC for an ACK.
pub const SESSION_ACK: u8 = 2;

/// Command set / id pairs.
pub const GET_VERSION: (u8, u8) = (0x00, 0x00);
pub const ACTIVATE: (u8, u8) = (0x00, 0x01);
pub const SET_CONTROL: (u8, u8) = (0x01, 0x00);
pub const TASK: (u8, u8) = (0x01, 0x01);

/// Task codes for [`TASK`] (M210 takes a single byte).
pub const START_MOTOR: u8 = 7;
pub const STOP_MOTOR: u8 = 8;

/// [`SET_CONTROL`] ACK codes.
pub const RELEASE_CONTROL_SUCCESS: u16 = 1;
pub const OBTAIN_CONTROL_SUCCESS: u16 = 2;
pub const OBTAIN_CONTROL_IN_PROGRESS: u16 = 3;
pub const RELEASE_CONTROL_IN_PROGRESS: u16 = 4;

/// One request frame: header (version 0, no encryption), set/id, payload,
/// CRC-32.
pub fn frame(session: u8, seq: u16, (cmd_set, cmd_id): (u8, u8), payload: &[u8]) -> Vec<u8> {
    let len = HEADER_LEN + 2 + payload.len() + 4;
    debug_assert!(len <= super::MAX_LEN);
    let [s0, s1] = seq.to_le_bytes();
    let mut f = vec![
        SOF,
        len as u8,
        (len >> 8) as u8 & 0x03,
        session & 0x1F,
        0,
        0,
        0,
        0,
        s0,
        s1,
    ];
    f.extend_from_slice(&crc16(&f).to_le_bytes());
    f.extend_from_slice(&[cmd_set, cmd_id]);
    f.extend_from_slice(payload);
    f.extend_from_slice(&crc32(&f).to_le_bytes());
    f
}

/// Ask for the FC's version string (read-only).
pub fn get_version(seq: u16) -> Vec<u8> {
    frame(SESSION_ACK, seq, GET_VERSION, &[0])
}

/// Activate app `app_id` against firmware `fw` (from [`fw_version`]). No key
/// is sent: the key only matters for encrypted frames, which we never use.
pub fn activate(seq: u16, app_id: u32, fw: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(44);
    p.extend_from_slice(&app_id.to_le_bytes());
    p.extend_from_slice(&2u32.to_le_bytes()); // "reserved", always 2 in the SDK
    p.extend_from_slice(&fw.to_le_bytes());
    p.extend_from_slice(&[b'0'; 32]); // iOS id, unused
    frame(SESSION_ACK, seq, ACTIVATE, &p)
}

/// Obtain (`true`) or release (`false`) SDK control authority.
pub fn set_control(seq: u16, obtain: bool) -> Vec<u8> {
    frame(SESSION_ACK, seq, SET_CONTROL, &[u8::from(obtain)])
}

/// Start (`true`) or stop (`false`) all motors.
pub fn motors(seq: u16, on: bool) -> Vec<u8> {
    frame(
        SESSION_ACK,
        seq,
        TASK,
        &[if on { START_MOTOR } else { STOP_MOTOR }],
    )
}

/// The 16-bit code at the start of an ACK (a lone byte counts as the code).
pub fn ack_code(data: &[u8]) -> Option<u16> {
    match data {
        [] => None,
        [b] => Some(u16::from(*b)),
        [a, b, ..] => Some(u16::from_le_bytes([*a, *b])),
    }
}

/// The firmware version from a [`get_version`] ACK, packed as the SDK does
/// (`a.b.c.d` → `a<<24 | b<<16 | c<<8 | d`). The ACK is a 2-byte code, a
/// NUL-terminated id, then a name like `SDK-v1.1 BETA PM410-03.03.10.18`.
pub fn fw_version(data: &[u8]) -> Option<u32> {
    let rest = data.get(2..)?;
    let rest = &rest[rest.iter().position(|&b| b == 0)? + 1..];
    let name = &rest[..rest.iter().position(|&b| b == 0).unwrap_or(rest.len())];
    let name = std::str::from_utf8(name).ok()?;
    let (_, ver) = name.rsplit_once('-')?;
    let mut parts = ver.split('.').map(|p| p.parse::<u8>().ok());
    let mut v = 0u32;
    for _ in 0..4 {
        v = v << 8 | u32::from(parts.next()??);
    }
    parts.next().is_none().then_some(v)
}

/// Activation ACK codes.
pub fn activation_message(code: u16) -> &'static str {
    match code {
        0 => "success",
        1 => "parameter error",
        2 => "encode error",
        3 => "new device (needs one online activation)",
        4 => "DJI app not connected",
        5 => "network error",
        6 => "server refused access",
        7 => "access level error",
        8 => "OSDK version error",
        _ => "unknown activation error",
    }
}

/// [`SET_CONTROL`] ACK codes.
pub fn control_message(code: u16) -> &'static str {
    match code {
        0 => "RC mode error (switch the RC to P)",
        RELEASE_CONTROL_SUCCESS => "control released",
        OBTAIN_CONTROL_SUCCESS => "control obtained",
        OBTAIN_CONTROL_IN_PROGRESS => "obtaining control",
        RELEASE_CONTROL_IN_PROGRESS => "releasing control",
        5 => "RC needs mode P",
        6 => "RC needs mode F",
        0xC9 => "IOC mode blocks control",
        0xFF01 => "not activated",
        _ => "unknown control error",
    }
}

/// [`TASK`] ACK codes.
pub fn task_message(code: u16) -> &'static str {
    match code {
        0 => "success",
        1 => "motors already on",
        2 => "motors already off",
        3 => "in the air",
        4 => "not in the air",
        5 => "no home point",
        6 => "bad GPS",
        7 => "in simulator mode",
        8 => "already running",
        9 => "not running",
        10 => "invalid command",
        11 => "no landing gear",
        12 => "gimbal mounted",
        0xFF01 => "not activated",
        0xFF02 => "no control authority",
        _ => "refused (see the FC's motor-start failure reason)",
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    #[test]
    fn frames_parse_back() {
        let f = motors(0x0102, true);
        let (p, n) = parse(&f).unwrap();
        assert_eq!(n, f.len());
        assert_eq!((p.session, p.ack, p.seq), (2, false, 0x0102));
        assert_eq!((p.cmd_set, p.cmd_id, p.payload), (0x01, 0x01, &[7u8][..]));
        assert_eq!(parse(&motors(1, false)).unwrap().0.payload, &[8]);
        assert_eq!(parse(&set_control(1, false)).unwrap().0.payload, &[0]);
        let a = activate(3, 12_345, 0x0303_0A12);
        let (p, _) = parse(&a).unwrap();
        assert_eq!((p.cmd_set, p.cmd_id), ACTIVATE);
        assert_eq!(p.payload.len(), 44);
        assert_eq!(&p.payload[8..12], &[0x12, 0x0A, 0x03, 0x03]);
    }

    #[test]
    fn reads_the_firmware_version() {
        let mut ack = vec![0x01, 0xFF];
        ack.extend_from_slice(b"095XE9X00200W8\0SDK-v1.1 BETA PM410-03.03.10.18\0\0");
        assert_eq!(fw_version(&ack), Some(0x0303_0A12));
        assert_eq!(fw_version(b"\x00\x00id\0no version"), None);
        assert_eq!(fw_version(b"\x00"), None);
    }

    #[test]
    fn ack_codes() {
        assert_eq!(ack_code(&[2, 0]), Some(OBTAIN_CONTROL_SUCCESS));
        assert_eq!(ack_code(&[1]), Some(1));
        assert_eq!(ack_code(&[]), None);
    }
}
