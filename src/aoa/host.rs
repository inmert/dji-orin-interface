//! The Orin as the AOA **host**, impersonating the Cendence controller toward a
//! DJI tablet so DJI Pilot connects to us. Used by `aoa_capture` and `bridge`.

use std::io;
use std::thread::sleep;
use std::time::Duration;

use super::{
    ACCESSORY_ADB_PID, ACCESSORY_PID, DJI_IDENTITY, GET_PROTOCOL, GOOGLE_VID, REQUEST_TYPE_IN,
    REQUEST_TYPE_OUT, SEND_STRING, START, STRING_FIELDS,
};
use crate::logln;
use crate::usb::usbfs::{self, BulkEndpoints, BusAddr, Claim, Device};

/// Control-transfer timeout for the handshake requests, in milliseconds.
const TIMEOUT_MS: u32 = 1000;

fn context(what: &str, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// Find a device already enumerated in AOA accessory mode (ADB variant
/// preferred). Returns its address and PID.
pub fn find_accessory() -> Option<(BusAddr, u16)> {
    [ACCESSORY_ADB_PID, ACCESSORY_PID]
        .into_iter()
        .find_map(|pid| usbfs::find_device(GOOGLE_VID, pid).map(|a| (a, pid)))
}

/// Poll [`find_accessory`] for up to `tries` × 50 ms (both PIDs each round, so
/// a non-ADB accessory is found as quickly as an ADB one).
pub fn wait_for_accessory(tries: u32) -> Option<(BusAddr, u16)> {
    for _ in 0..tries {
        if let Some(found) = find_accessory() {
            return Some(found);
        }
        sleep(Duration::from_millis(50));
    }
    None
}

/// Drive the AOA handshake against `dev` with the DJI identity, so it
/// re-enumerates in accessory mode. Returns the device's protocol version.
///
/// `GET_PROTOCOL` and `START` failures are errors; a failed `SEND_STRING` is
/// logged and the handshake continues (as the real controller would).
pub fn handshake(dev: &Device) -> io::Result<u16> {
    let mut ver = [0u8; 2];
    dev.control(REQUEST_TYPE_IN, GET_PROTOCOL, 0, 0, &mut ver, TIMEOUT_MS)
        .map_err(|e| context("GET_PROTOCOL (no AOA support?)", e))?;
    let proto = u16::from_le_bytes(ver);
    logln!("aoa: protocol version = {proto}");
    if proto < 1 {
        return Err(io::Error::other(format!(
            "unsupported AOA protocol {proto}"
        )));
    }

    let mut fails = 0;
    for (i, (s, field)) in DJI_IDENTITY.iter().zip(STRING_FIELDS).enumerate() {
        let mut cbuf = s.as_bytes().to_vec();
        cbuf.push(0); // NUL-terminated
        match dev.control(
            REQUEST_TYPE_OUT,
            SEND_STRING,
            0,
            i as u16,
            &mut cbuf,
            TIMEOUT_MS,
        ) {
            Ok(_) => logln!("aoa: SEND_STRING[{i}] {field}=\"{s}\" -> ok"),
            Err(e) => {
                fails += 1;
                logln!("aoa: SEND_STRING[{i}] {field}=\"{s}\" -> {e}");
            }
        }
    }
    if fails > 0 {
        logln!("aoa: warning: {fails} SEND_STRING request(s) failed");
    }

    dev.control(REQUEST_TYPE_OUT, START, 0, 0, &mut [], TIMEOUT_MS)
        .map_err(|e| context("ACCESSORY_START", e))?;
    logln!("aoa: ACCESSORY_START -> ok");
    Ok(proto)
}

/// Open an enumerated accessory: read its descriptors, find the bulk endpoints
/// and claim interface 0. A failed claim is logged, not fatal (bulk transfers
/// may still work).
pub fn open_accessory(addr: BusAddr) -> io::Result<(Device, BulkEndpoints)> {
    let acc = Device::open(addr).map_err(|e| context("open accessory", e))?;
    let mut desc = [0u8; 2048];
    let n = acc
        .read_descriptors(&mut desc)
        .map_err(|e| context("read descriptors", e))?;
    if n == 0 {
        return Err(io::Error::other("empty descriptors"));
    }
    let eps = usbfs::find_bulk_endpoints(&desc[..n])
        .ok_or_else(|| io::Error::other("no bulk IN endpoint"))?;
    logln!(
        "aoa: bulk IN={:#04x} OUT={:#04x}",
        eps.in_addr,
        eps.out_addr
    );
    match acc.claim_interface(0) {
        Ok(Claim::DetachedDriver) => logln!("aoa: claimed interface 0 (detached kernel driver)"),
        Ok(Claim::Claimed) => logln!("aoa: claimed interface 0"),
        Err(e) => logln!("aoa: warning: could not claim interface 0: {e} (bulk I/O may fail)"),
    }
    Ok((acc, eps))
}
