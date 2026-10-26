//! aoa_capture — Orin as the *controller* toward a DJI CrystalSky / DJI Pilot.
//!
//! Acts as a USB host and performs the Android Open Accessory (AOA) handshake
//! using the exact accessory identity the real Cendence controller advertises,
//! so DJI Pilot connects to the Orin and begins emitting its DUML command
//! stream. All DUML the tablet sends is reassembled with [`duml::Decoder`] and
//! logged.
//!
//! Uses only the kernel usbfs interface (via the `usbfs` module).
//!
//! Usage: aoa_capture [logfile]        (run as root)

use std::fs::File;
use std::io::ErrorKind;
use std::thread::sleep;
use std::time::Duration;

use dji_orin_interface::log::Hex;
use dji_orin_interface::usbfs::{self, BusAddr, Device};
use dji_orin_interface::{aoa, duml, logln, sys};

const DJI_VID: u16 = 0x2ca3; // DJI CrystalSky (ZS600B) default mode
const DJI_PID: u16 = 0x4016;

fn die(msg: &str) -> ! {
    logln!("{msg}");
    std::process::exit(1);
}

/// Poll for a device by VID/PID for up to `tries` × 50 ms.
fn wait_for(vid: u16, pid: u16, tries: u32) -> Option<BusAddr> {
    for _ in 0..tries {
        if let Some(a) = usbfs::find_device(vid, pid) {
            return Some(a);
        }
        sleep(Duration::from_millis(50));
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sink: Option<Box<dyn std::io::Write + Send>> = match args.get(1) {
        Some(path) => match File::create(path) {
            Ok(f) => Some(Box::new(f)),
            Err(e) => {
                eprintln!("logfile {path}: {e}");
                std::process::exit(1);
            }
        },
        None => None,
    };
    dji_orin_interface::log::init(sink);

    let Some(addr) = usbfs::find_device(DJI_VID, DJI_PID) else {
        eprintln!(
            "DJI {DJI_VID:04x}:{DJI_PID:04x} not found. Plug the CrystalSky into a Orin USB-A port."
        );
        std::process::exit(1);
    };
    logln!(
        "found DJI {DJI_VID:04x}:{DJI_PID:04x} at bus {} dev {}",
        addr.bus,
        addr.dev
    );
    let dev = match Device::open(addr) {
        Ok(d) => d,
        Err(e) => die(&format!("open device: {e}")),
    };

    // GET_PROTOCOL (IN): the device reports its AOA protocol version.
    let mut ver = [0u8; 2];
    if let Err(e) = dev.control(
        aoa::REQUEST_TYPE_IN,
        aoa::GET_PROTOCOL,
        0,
        0,
        &mut ver,
        1000,
    ) {
        die(&format!("GET_PROTOCOL failed: {e} (no AOA support?)"));
    }
    let proto = u16::from_le_bytes(ver);
    logln!("AOA protocol version = {proto}");
    if proto < 1 {
        die("protocol < 1, aborting");
    }

    // SEND_STRING x6 (OUT): announce the DJI accessory identity.
    let mut fails = 0;
    for (i, s) in aoa::DJI_IDENTITY.iter().enumerate() {
        let mut cbuf = s.as_bytes().to_vec();
        cbuf.push(0); // NUL-terminated
        let field = aoa::STRING_FIELDS[i];
        match dev.control(
            aoa::REQUEST_TYPE_OUT,
            aoa::SEND_STRING,
            0,
            i as u16,
            &mut cbuf,
            1000,
        ) {
            Ok(_) => logln!("SEND_STRING[{i}] {field}=\"{s}\" -> ok"),
            Err(e) => {
                fails += 1;
                logln!("SEND_STRING[{i}] {field}=\"{s}\" -> {e}");
            }
        }
    }
    if fails > 0 {
        logln!("warning: {fails} SEND_STRING request(s) failed");
    }

    // ACCESSORY_START (OUT, no data).
    match dev.control(aoa::REQUEST_TYPE_OUT, aoa::START, 0, 0, &mut [], 1000) {
        Ok(_) => logln!("ACCESSORY_START -> ok"),
        Err(e) => die(&format!("ACCESSORY_START failed: {e}; aborting")),
    }
    drop(dev);
    logln!("waiting for accessory-mode re-enumeration...");
    logln!("** tap OK on any USB-accessory prompt on the CrystalSky screen **");

    // Wait for the accessory device (18d1:2d01 preferred, else 2d00).
    let (acc_addr, apid) = wait_for(aoa::GOOGLE_VID, aoa::ACCESSORY_ADB_PID, 200)
        .map(|a| (a, aoa::ACCESSORY_ADB_PID))
        .or_else(|| {
            wait_for(aoa::GOOGLE_VID, aoa::ACCESSORY_PID, 1).map(|a| (a, aoa::ACCESSORY_PID))
        })
        .unwrap_or_else(|| die("accessory device never appeared; giving up."));
    logln!(
        "accessory mode up: {:04x}:{apid:04x} bus {} dev {}",
        aoa::GOOGLE_VID,
        acc_addr.bus,
        acc_addr.dev
    );
    sleep(Duration::from_millis(200));
    let acc = match Device::open(acc_addr) {
        Ok(d) => d,
        Err(e) => die(&format!("open accessory: {e}")),
    };

    // Find the bulk endpoints from the device's configuration descriptor.
    let mut desc = [0u8; 2048];
    let dn = match acc.read_descriptors(&mut desc) {
        Ok(n) if n > 0 => n,
        Ok(_) => die("failed to read descriptors: empty"),
        Err(e) => die(&format!("failed to read descriptors: {e}")),
    };
    let eps = match usbfs::find_bulk_endpoints(&desc[..dn]) {
        Some(e) => e,
        None => die("no bulk IN endpoint"),
    };
    logln!(
        "endpoints: bulk IN={:#04x} OUT={:#04x}",
        eps.in_addr,
        eps.out_addr
    );

    // Claim interface 0 (detach any kernel driver first).
    match acc.claim_interface(0) {
        Ok(usbfs::Claim::DetachedDriver) => logln!("claimed interface 0 (detached kernel driver)"),
        Ok(usbfs::Claim::Claimed) => logln!("claimed interface 0"),
        Err(e) => logln!("warning: could not claim interface 0: {e} (bulk reads may fail)"),
    }

    // Capture the bulk IN endpoint (CrystalSky/Pilot -> Orin). A blocking read
    // (timeout 0) means an in-flight transfer is never discarded by a timeout.
    logln!(
        "capturing DUML from DJI Pilot on EP {:#04x} ...",
        eps.in_addr
    );
    let mut decoder = duml::Decoder::new();
    let mut buf = vec![0u8; 16384];
    loop {
        match acc.bulk_read(eps.in_addr, &mut buf, 0) {
            Ok(0) => {}
            Ok(n) => {
                logln!("BULK-IN {n} bytes");
                decoder.push(&buf[..n]);
                let count = decoder.drain(|f| logln!("  DUML {f}"));
                if count == 0 {
                    logln!(
                        "  (no complete DUML frame; buffered {} bytes)",
                        decoder.buffered()
                    );
                    let head = &buf[..n.min(16)];
                    logln!("  first bytes:{}", Hex(head));
                }
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                }
                if code == sys::errno::ENODEV || code == sys::errno::ESHUTDOWN {
                    logln!("device disconnected; stopping capture");
                    break;
                }
                logln!("bulk read error: {e}");
                sleep(Duration::from_millis(100));
            }
        }
    }
}
