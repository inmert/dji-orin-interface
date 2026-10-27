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
//! Usage: `aoa_capture [logfile]`        (run as root)

use std::fs::File;
use std::io::ErrorKind;
use std::thread::sleep;
use std::time::Duration;

use dji_common::log::Hex;
use dji_common::logln;
use dji_protocol::duml;
use dji_transport::aoa::{self, host};
use dji_transport::usb;
use dji_transport::usb::usbfs::{self, Device};

const VID: u16 = aoa::CRYSTALSKY_VID;
const PID: u16 = aoa::CRYSTALSKY_PID;

fn die(msg: &str) -> ! {
    logln!("{msg}");
    std::process::exit(1);
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
    dji_common::log::init(sink);

    let Some(addr) = usbfs::find_device(VID, PID) else {
        eprintln!("DJI {VID:04x}:{PID:04x} not found. Plug the CrystalSky into a Orin USB-A port.");
        std::process::exit(1);
    };
    logln!(
        "found DJI {VID:04x}:{PID:04x} at bus {} dev {}",
        addr.bus,
        addr.dev
    );
    let dev = Device::open(addr).unwrap_or_else(|e| die(&format!("open device: {e}")));
    if let Err(e) = host::handshake(&dev) {
        die(&format!("AOA handshake failed: {e}; aborting"));
    }
    drop(dev);
    logln!("waiting for accessory-mode re-enumeration...");
    logln!("** tap OK on any USB-accessory prompt on the CrystalSky screen **");

    let (acc_addr, apid) = host::wait_for_accessory(200)
        .unwrap_or_else(|| die("accessory device never appeared; giving up."));
    logln!(
        "accessory mode up: {:04x}:{apid:04x} bus {} dev {}",
        aoa::GOOGLE_VID,
        acc_addr.bus,
        acc_addr.dev
    );
    sleep(Duration::from_millis(200));
    let (acc, eps) = host::open_accessory(acc_addr).unwrap_or_else(|e| die(&format!("{e}")));

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
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == usb::errno::ENODEV || code == usb::errno::ESHUTDOWN {
                    logln!("device disconnected; stopping capture");
                    break;
                }
                logln!("bulk read error: {e}");
                sleep(Duration::from_millis(100));
            }
        }
    }
}
