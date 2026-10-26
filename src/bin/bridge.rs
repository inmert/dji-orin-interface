//! bridge — transparent DUML MITM between DJI Pilot and the real aircraft.
//!
//! The Orin plays both USB roles at once:
//!
//! ```text
//!  tablet (DJI Pilot)         Orin                  Cendence RC        M210
//!  ┌────────────┐  USB-A    ┌───────────┐  USB-C   ┌──────────┐ radio ┌─────┐
//!  │ CrystalSky │ ◀───────▶ │ host +    │ ◀──────▶ │   RC     │ ◀───▶ │drone│
//!  │  (device)  │ (Orin=    │ gadget    │ (Orin=   │ (host)   │       │     │
//!  └────────────┘  host)    └───────────┘  device) └──────────┘       └─────┘
//! ```
//!
//! * Toward the tablet the Orin is the AOA **host** (usbfs), impersonating the
//!   controller, so DJI Pilot talks to us as if we were the RC.
//! * Toward the real RC the Orin is the AOA **device** (FunctionFS gadget),
//!   impersonating the app, exactly as `ffs_capture` does.
//!
//! Two relay threads forward the raw bulk streams verbatim and log the DUML
//! frames they carry:
//!   * RC  → Orin (ep1 OUT) → tablet (bulk OUT): telemetry / video to Pilot.
//!   * tablet (bulk IN) → Orin → RC (ep2 IN): Pilot's commands to the aircraft.
//!
//! This milestone is pure passthrough: every byte is forwarded unchanged and
//! nothing is injected. The configfs gadget + UDC bind is done by
//! scripts/run_bridge.sh.
//!
//! SAFETY: the forwarded stream includes Pilot's flight commands, which the RC
//! relays to the powered aircraft. Run only with propellers removed / aircraft
//! secured.
//!
//! Usage: bridge <ffs_mount_dir>        (run as root)

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use dji_orin_interface::usb::ffs::{self, Event, Setup};
use dji_orin_interface::usb::sys;
use dji_orin_interface::usb::usbfs::{self, BulkEndpoints, Device};
use dji_orin_interface::{aoa, duml, logln};

const DJI_VID: u16 = 0x2ca3; // DJI CrystalSky (ZS600B) default mode
const DJI_PID: u16 = 0x4016;

fn die(msg: &str) -> ! {
    logln!("{msg}");
    std::process::exit(1);
}

/// Look for the tablet already enumerated in AOA accessory mode (ADB variant
/// preferred), e.g. because a previous run left it there. Returns its address
/// and PID if present.
fn find_accessory() -> Option<(usbfs::BusAddr, u16)> {
    usbfs::find_device(aoa::GOOGLE_VID, aoa::ACCESSORY_ADB_PID)
        .map(|a| (a, aoa::ACCESSORY_ADB_PID))
        .or_else(|| {
            usbfs::find_device(aoa::GOOGLE_VID, aoa::ACCESSORY_PID).map(|a| (a, aoa::ACCESSORY_PID))
        })
}

/// Drive the AOA host handshake against the DJI device so it re-enumerates in
/// accessory mode. Returns once the handshake has been sent; the caller polls
/// for the accessory device to appear. Dies if the device is absent or refuses.
fn aoa_handshake(addr: usbfs::BusAddr) {
    logln!("tablet: found DJI at bus {} dev {}", addr.bus, addr.dev);
    let dev = Device::open(addr).unwrap_or_else(|e| die(&format!("tablet: open: {e}")));

    let mut ver = [0u8; 2];
    if let Err(e) = dev.control(
        aoa::REQUEST_TYPE_IN,
        aoa::GET_PROTOCOL,
        0,
        0,
        &mut ver,
        1000,
    ) {
        die(&format!(
            "tablet: GET_PROTOCOL failed: {e} (no AOA support?)"
        ));
    }
    logln!("tablet: AOA protocol version = {}", u16::from_le_bytes(ver));

    for (i, s) in aoa::DJI_IDENTITY.iter().enumerate() {
        let mut cbuf = s.as_bytes().to_vec();
        cbuf.push(0);
        if let Err(e) = dev.control(
            aoa::REQUEST_TYPE_OUT,
            aoa::SEND_STRING,
            0,
            i as u16,
            &mut cbuf,
            1000,
        ) {
            logln!("tablet: SEND_STRING[{i}] failed: {e}");
        }
    }
    if let Err(e) = dev.control(aoa::REQUEST_TYPE_OUT, aoa::START, 0, 0, &mut [], 1000) {
        die(&format!("tablet: ACCESSORY_START failed: {e}"));
    }
    drop(dev);
    logln!("tablet: waiting for accessory-mode re-enumeration (tap OK on the screen if asked)");
}

/// Open an already-enumerated accessory device: read its descriptors, find the
/// bulk endpoints and claim interface 0.
fn open_accessory(addr: usbfs::BusAddr) -> (Device, BulkEndpoints) {
    let acc = Device::open(addr).unwrap_or_else(|e| die(&format!("tablet: open accessory: {e}")));
    let mut desc = [0u8; 2048];
    let dn = match acc.read_descriptors(&mut desc) {
        Ok(n) if n > 0 => n,
        Ok(_) => die("tablet: empty descriptors"),
        Err(e) => die(&format!("tablet: read descriptors: {e}")),
    };
    let eps =
        usbfs::find_bulk_endpoints(&desc[..dn]).unwrap_or_else(|| die("tablet: no bulk endpoints"));
    logln!(
        "tablet: bulk IN={:#04x} OUT={:#04x}",
        eps.in_addr,
        eps.out_addr
    );
    match acc.claim_interface(0) {
        Ok(_) => logln!("tablet: claimed interface 0"),
        Err(e) => logln!("tablet: warning: claim failed: {e}"),
    }
    (acc, eps)
}

/// Bring up the tablet side and return its claimed accessory + bulk endpoints.
///
/// Idempotent across runs: if the tablet is already in AOA accessory mode (a
/// previous run left it there — teardown does not reset it), skip the handshake
/// and open it directly. Otherwise run the AOA host handshake against the DJI
/// device and wait for it to re-enumerate in accessory mode.
fn open_tablet() -> (Device, BulkEndpoints) {
    if let Some((addr, apid)) = find_accessory() {
        logln!(
            "tablet: already in accessory mode {:04x}:{apid:04x}; skipping handshake",
            aoa::GOOGLE_VID
        );
        return open_accessory(addr);
    }

    let Some(addr) = usbfs::find_device(DJI_VID, DJI_PID) else {
        die(&format!(
            "DJI {DJI_VID:04x}:{DJI_PID:04x} not found and no accessory present; \
             plug the CrystalSky into a USB-A port (or replug it to reset accessory mode)."
        ));
    };
    aoa_handshake(addr);

    let mut found = None;
    for _ in 0..200 {
        if let Some(a) = find_accessory() {
            found = Some(a);
            break;
        }
        sleep(Duration::from_millis(50));
    }
    let (addr, apid) = found.unwrap_or_else(|| die("tablet: accessory device never appeared"));
    logln!(
        "tablet: accessory mode up {:04x}:{apid:04x}",
        aoa::GOOGLE_VID
    );
    sleep(Duration::from_millis(200));
    open_accessory(addr)
}

/// Answer one ep0 SETUP packet (the AOA handshake the RC drives against us).
fn handle_setup(ep0: BorrowedFd<'_>, s: Setup) {
    if s.is_vendor() && s.is_in() && s.request == aoa::GET_PROTOCOL {
        let ver = aoa::PROTOCOL_VERSION.to_le_bytes();
        let n = usize::from(s.length).min(ver.len());
        let _ = sys::write(ep0, &ver[..n]);
        return;
    }
    if s.is_vendor() && !s.is_in() && s.request == aoa::START {
        let _ = sys::read(ep0, &mut []);
        return;
    }
    if !s.is_in() && s.length > 0 {
        let mut data = vec![0u8; usize::from(s.length).min(4096)];
        let _ = sys::read(ep0, &mut data);
        return;
    }
    if !s.is_in() {
        let _ = sys::read(ep0, &mut []);
        return;
    }
    let _ = ffs::stall(ep0, true);
}

/// Forward one raw bulk stream verbatim, logging the CRC-valid DUML it carries.
/// `label` tags the log lines; `read`/`write` move the bytes.
/// Command sets whose payloads should be hex-dumped in the log, parsed once
/// from `BRIDGE_HEX_SETS` (comma/space-separated hex bytes, e.g. "04,03").
/// Empty (unset) keeps the log to the one-line frame summary. This is passive
/// logging only — the raw stream is still forwarded verbatim and unchanged.
fn hex_sets_from_env() -> Vec<u8> {
    std::env::var("BRIDGE_HEX_SETS")
        .unwrap_or_default()
        .split([',', ' '])
        .filter_map(|s| u8::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn relay(
    label: &'static str,
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
    write: impl Fn(&[u8]) -> std::io::Result<usize>,
) {
    let hex_sets = hex_sets_from_env();
    let mut decoder = duml::Decoder::new();
    let mut buf = vec![0u8; 16384];
    loop {
        match read(&mut buf) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
                // Forward the raw bytes first (lowest added latency), then log.
                let mut sent = 0;
                while sent < n {
                    match write(&buf[sent..n]) {
                        Ok(0) => break,
                        Ok(w) => sent += w,
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(e) => {
                            logln!("{label}: forward error: {e}");
                            break;
                        }
                    }
                }
                decoder.push(&buf[..n]);
                decoder.drain(|f| {
                    if f.crc_ok {
                        if hex_sets.contains(&f.cmd_set) {
                            logln!("{label} {f} | payload={}", hex(f.payload));
                        } else {
                            logln!("{label} {f}");
                        }
                    }
                });
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == sys::errno::ENODEV || code == sys::errno::ESHUTDOWN {
                    logln!("{label}: endpoint gone; relay stopping");
                    return;
                }
                logln!("{label}: read error: {e}");
                sleep(Duration::from_millis(50));
            }
        }
    }
}

fn open_ep(ffs_dir: &str, name: &str) -> OwnedFd {
    for _ in 0..200 {
        if let Ok(f) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("{ffs_dir}/{name}"))
        {
            return f.into();
        }
        sleep(Duration::from_millis(20));
    }
    die(&format!("gadget: could not open {name}"));
}

fn main() {
    dji_orin_interface::log::init(None);
    let args: Vec<String> = std::env::args().collect();
    let Some(ffs_dir) = args.get(1).cloned() else {
        eprintln!("usage: {} <ffs_mount_dir>", args[0]);
        std::process::exit(1);
    };

    // Fail fast if the tablet is not even present, before we touch the gadget.
    if usbfs::find_device(DJI_VID, DJI_PID).is_none()
        && usbfs::find_device(aoa::GOOGLE_VID, aoa::ACCESSORY_ADB_PID).is_none()
        && usbfs::find_device(aoa::GOOGLE_VID, aoa::ACCESSORY_PID).is_none()
    {
        die("DJI tablet not found; plug the CrystalSky into a USB-A port first.");
    }

    // Gadget side: write descriptors so run_bridge.sh can bind the UDC. The RC
    // then enumerates us; its BIND/ENABLE/SETUP events queue on ep0 until the
    // event loop below drains them, so bringing the tablet up first is fine.
    let ep0: OwnedFd = OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("{ffs_dir}/ep0"))
        .map(OwnedFd::from)
        .unwrap_or_else(|e| die(&format!("open ep0: {e}")));
    if let Err(e) =
        ffs::write_descriptors(ep0.as_fd()).and_then(|()| ffs::write_strings(ep0.as_fd()))
    {
        die(&format!("write descriptors/strings: {e}"));
    }
    logln!("descriptors+strings written; bringing up tablet side");

    // Tablet side (blocks through the AOA handshake, possibly a screen tap).
    let (tablet, eps) = open_tablet();
    let tablet = Arc::new(tablet);
    logln!("both sides staged; waiting for RC to enable the gadget");

    // ep0 event loop: on ENABLE, open the bulk endpoints and start the relays.
    let mut started = false;
    let mut last_errno = 0;
    let mut ev = [0u8; ffs::EVENT_SIZE * 8];
    loop {
        match sys::read(ep0.as_fd(), &mut ev) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
                last_errno = 0;
                for chunk in ev[..n].chunks_exact(ffs::EVENT_SIZE) {
                    let record: [u8; ffs::EVENT_SIZE] = chunk.try_into().expect("chunk size");
                    match Event::parse(&record) {
                        Event::Bind => logln!("gadget: BIND"),
                        Event::Unbind => logln!("gadget: UNBIND"),
                        Event::Enable => {
                            logln!("gadget: ENABLE");
                            if !started {
                                started = true;
                                start_relays(&ffs_dir, &tablet, eps);
                            }
                        }
                        Event::Disable => logln!("gadget: DISABLE"),
                        Event::Setup(s) => handle_setup(ep0.as_fd(), s),
                        Event::Other(_) => {}
                    }
                }
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                }
                if code != sys::errno::ESHUTDOWN && code != sys::errno::ENODEV && code != last_errno
                {
                    logln!("gadget: ep0 read error: {e}");
                }
                last_errno = code;
                sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Spawn the two relay threads once both sides are up.
fn start_relays(ffs_dir: &str, tablet: &Arc<Device>, eps: BulkEndpoints) {
    let ep1 = open_ep(ffs_dir, "ep1"); // OUT: RC -> Orin
    let ep2 = open_ep(ffs_dir, "ep2"); // IN:  Orin -> RC
    logln!("relays: ep1/ep2 open; forwarding (passthrough, no injection)");

    // RC -> tablet: read ep1, write tablet bulk OUT.
    {
        let tablet = Arc::clone(tablet);
        let out_ep = eps.out_addr;
        std::thread::spawn(move || {
            relay(
                "RC->TAB",
                |buf| sys::read(ep1.as_fd(), buf),
                |data| tablet.bulk_write(out_ep, data, 0),
            );
        });
    }
    // tablet -> RC: read tablet bulk IN, write ep2.
    {
        let tablet = Arc::clone(tablet);
        let in_ep = eps.in_addr;
        std::thread::spawn(move || {
            relay(
                "TAB->RC",
                |buf| tablet.bulk_read(in_ep, buf, 0),
                |data| sys::write(ep2.as_fd(), data),
            );
        });
    }
}
