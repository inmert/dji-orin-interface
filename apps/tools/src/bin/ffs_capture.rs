//! ffs_capture — Orin as the DJI *app* toward the Cendence controller.
//!
//! Presents a FunctionFS USB gadget that answers the controller's Android Open
//! Accessory (AOA) handshake and then exposes the two bulk endpoints over which
//! the controller streams DUML (video + telemetry). Everything the controller
//! sends on the OUT endpoint is reassembled with [`duml::Decoder`] and logged.
//!
//! The configfs plumbing + UDC binding is done by scripts/run_capture.sh; this
//! program drives ep0 (control) and ep1 (bulk OUT).
//!
//! Usage: `ffs_capture <ffs_mount_dir>`        (run as root)

use std::io::ErrorKind;
use std::os::fd::{AsFd, BorrowedFd};
use std::thread::sleep;
use std::time::Duration;

use dji_common::log::Hex;
use dji_common::logln;
use dji_protocol::duml;
use dji_transport::aoa;
use dji_transport::aoa::device::{Answer, answer_setup};
use dji_transport::usb;
use dji_transport::usb::ffs::{self, Event, Setup};

/// Log one ep0 SETUP packet, then answer it.
fn handle_setup(ep0: BorrowedFd<'_>, s: Setup) {
    let dir = if s.is_in() { "IN" } else { "OUT" };
    let tag = match (s.is_vendor(), aoa::request_name(s.request)) {
        (true, Some(name)) => format!(" [AOA {name}]"),
        (true, None) => " [vendor]".to_owned(),
        (false, _) => String::new(),
    };
    logln!(
        "SETUP {dir} bmRequestType={:#04x} bRequest={:#04x} wValue={:#06x} wIndex={:#06x} wLength={}{tag}",
        s.request_type,
        s.request,
        s.value,
        s.index,
        s.length,
    );
    match answer_setup(ep0, &s) {
        Ok(Answer::Data(d)) if !d.is_empty() => logln!("  OUT-DATA ({}):{}", d.len(), Hex(&d)),
        Ok(_) => {}
        Err(e) => logln!("  ep0 reply failed: {e}"),
    }
}

/// Read ep1 (OUT, controller -> Orin); reassemble DUML and log.
fn bulk_reader(ffs_dir: String) {
    let mut ep1 = match ffs::open_endpoint(&ffs_dir, "ep1", 200) {
        Ok(f) => f,
        Err(e) => {
            logln!("bulk: could not open ep1: {e}");
            return;
        }
    };
    logln!("bulk: ep1 (OUT, controller->Orin) open; decoding DUML");
    let log_payloads = std::env::var_os("CAPTURE_HEX").is_some();
    let mut decoder = duml::Decoder::new();
    let mut buf = vec![0u8; 16384];
    loop {
        match usb::read(ep1.as_fd(), &mut buf) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
                logln!("BULK-IN {n} bytes");
                decoder.push(&buf[..n]);
                let count = decoder.drain(|f| {
                    if log_payloads && f.crc_ok {
                        logln!(
                            "  DUML {f} type=0x{:02x} | payload={}",
                            f.cmd_type,
                            dji_common::hex(f.payload)
                        );
                    } else {
                        logln!("  DUML {f}");
                    }
                });
                if count == 0 {
                    logln!(
                        "  (no complete DUML frame; buffered {} bytes)",
                        decoder.buffered()
                    );
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
                sleep(Duration::from_millis(2));
            }
            Err(e) => {
                logln!("bulk: ep1 error {e}; reopening");
                // Re-open with a backoff so a persistently-failing ep1 cannot
                // spin this thread at 100% CPU.
                loop {
                    sleep(Duration::from_millis(50));
                    if let Ok(f) = ffs::open_endpoint(&ffs_dir, "ep1", 1) {
                        ep1 = f;
                        break;
                    }
                }
            }
        }
    }
}

fn main() {
    dji_common::log::init(None);
    let args: Vec<String> = std::env::args().collect();
    let Some(ffs_dir) = args.get(1).cloned() else {
        eprintln!("usage: {} <ffs_mount_dir>", args[0]);
        std::process::exit(1);
    };

    let ep0 = ffs::init_ep0(&ffs_dir).unwrap_or_else(|e| {
        eprintln!("gadget init: {e}");
        std::process::exit(1);
    });
    logln!("descriptors+strings written; waiting for controller");

    let mut started = false;
    ffs::run_events(ep0.as_fd(), |event| match event {
        Event::Bind => logln!("EVENT BIND"),
        Event::Unbind => logln!("EVENT UNBIND"),
        Event::Enable => {
            logln!("EVENT ENABLE");
            if !started {
                started = true;
                let d = ffs_dir.clone();
                std::thread::spawn(move || bulk_reader(d));
            }
        }
        Event::Disable => logln!("EVENT DISABLE"),
        Event::Setup(s) => handle_setup(ep0.as_fd(), s),
        Event::Other(_) => {}
    });
}
