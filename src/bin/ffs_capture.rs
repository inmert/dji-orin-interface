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
//! Usage: ffs_capture <ffs_mount_dir>        (run as root)

use std::fs::OpenOptions;
use std::os::fd::{AsFd, OwnedFd};
use std::time::Duration;

use dji_orin_interface::ffs::{self, Event, Setup};
use dji_orin_interface::log::Hex;
use dji_orin_interface::{aoa, duml, logln, sys};

/// Handle one ep0 SETUP packet.
fn handle_setup(ep0: &OwnedFd, s: Setup) {
    let dir = if s.is_in() { "IN" } else { "OUT" };
    let tag = if s.is_vendor() {
        aoa::request_name(s.request).map_or(" [vendor]", |n| match n {
            "GET_PROTOCOL" => " [AOA GET_PROTOCOL]",
            "SEND_STRING" => " [AOA SEND_STRING]",
            "START" => " [AOA START]",
            _ => " [vendor]",
        })
    } else {
        ""
    };
    logln!(
        "SETUP {dir} bmRequestType={:#04x} bRequest={:#04x} wValue={:#06x} wIndex={:#06x} wLength={}{tag}",
        s.request_type,
        s.request,
        s.value,
        s.index,
        s.length,
    );

    let fd = ep0.as_fd();

    // AOA GET_PROTOCOL: report our protocol version.
    if s.is_vendor() && s.is_in() && s.request == aoa::GET_PROTOCOL {
        let ver = aoa::PROTOCOL_VERSION.to_le_bytes();
        let n = usize::from(s.length).min(ver.len());
        if let Err(e) = sys::write(fd, &ver[..n]) {
            logln!("  ep0 write failed: {e}");
        }
        return;
    }
    // AOA ACCESSORY_START: ack the (no-data) status stage.
    if s.is_vendor() && !s.is_in() && s.request == aoa::START {
        if let Err(e) = sys::read(fd, &mut []) {
            logln!("  ep0 ack read failed: {e}");
        }
        return;
    }

    if !s.is_in() && s.length > 0 {
        // OUT data stage we do accept (e.g. AOA SEND_STRING): read and log it.
        let mut data = vec![0u8; usize::from(s.length).min(4096)];
        match sys::read(fd, &mut data) {
            Ok(n) if n > 0 => logln!("  OUT-DATA ({n}):{}", Hex(&data[..n])),
            Ok(_) => {}
            Err(e) => logln!("  ep0 OUT-DATA read failed: {e}"),
        }
        return;
    }
    if !s.is_in() {
        // No-data OUT we do not specifically handle: ack it.
        if let Err(e) = sys::read(fd, &mut []) {
            logln!("  ep0 ack read failed: {e}");
        }
        return;
    }
    // An IN request we do not support: stall it rather than returning a bogus
    // zero-length reply.
    if let Err(e) = ffs::stall(fd, true) {
        logln!("  ep0 stall failed: {e}");
    }
}

fn open_retry(path: &str, tries: u32) -> Option<OwnedFd> {
    for _ in 0..tries {
        if let Ok(f) = OpenOptions::new().read(true).write(true).open(path) {
            return Some(f.into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Read ep1 (OUT, controller -> Orin); reassemble DUML and log.
fn bulk_reader(ffs_dir: String) {
    let path = format!("{ffs_dir}/ep1");
    let mut ep1 = match open_retry(&path, 200) {
        Some(f) => f,
        None => {
            logln!("bulk: could not open ep1");
            return;
        }
    };
    logln!("bulk: ep1 (OUT, controller->Orin) open; decoding DUML");
    let mut decoder = duml::Decoder::new();
    let mut buf = vec![0u8; 16384];
    loop {
        match sys::read(ep1.as_fd(), &mut buf) {
            Ok(0) => std::thread::sleep(Duration::from_millis(2)),
            Ok(n) => {
                logln!("BULK-IN {n} bytes");
                decoder.push(&buf[..n]);
                let count = decoder.drain(|f| logln!("  DUML {f}"));
                if count == 0 {
                    logln!(
                        "  (no complete DUML frame; buffered {} bytes)",
                        decoder.buffered()
                    );
                }
            }
            Err(e) => {
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                logln!("bulk: ep1 error {e}; reopening");
                // Re-open with a backoff so a persistently-failing ep1 cannot
                // spin this thread at 100% CPU.
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                    if let Some(f) = open_retry(&path, 1) {
                        ep1 = f;
                        break;
                    }
                }
            }
        }
    }
}

fn main() {
    dji_orin_interface::log::init(None);
    let args: Vec<String> = std::env::args().collect();
    let Some(ffs_dir) = args.get(1).cloned() else {
        eprintln!("usage: {} <ffs_mount_dir>", args[0]);
        std::process::exit(1);
    };

    let ep0: OwnedFd = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("{ffs_dir}/ep0"))
    {
        Ok(f) => f.into(),
        Err(e) => {
            eprintln!("open ep0: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) =
        ffs::write_descriptors(ep0.as_fd()).and_then(|()| ffs::write_strings(ep0.as_fd()))
    {
        eprintln!("write descriptors/strings: {e}");
        std::process::exit(1);
    }
    logln!("descriptors+strings written; waiting for controller");

    let mut started = false;
    let mut last_errno = 0;
    let mut ev = [0u8; ffs::EVENT_SIZE * 8];
    loop {
        match sys::read(ep0.as_fd(), &mut ev) {
            Ok(0) => std::thread::sleep(Duration::from_millis(2)),
            Ok(n) => {
                last_errno = 0;
                for chunk in ev[..n].chunks_exact(ffs::EVENT_SIZE) {
                    let record: [u8; ffs::EVENT_SIZE] = chunk.try_into().expect("chunk size");
                    match Event::parse(&record) {
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
                        Event::Setup(s) => handle_setup(&ep0, s),
                        Event::Other(_) => {}
                    }
                }
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                // ESHUTDOWN/ENODEV is normal with no host attached; back off
                // quietly. Log any other error once (not every spin).
                if code != sys::errno::ESHUTDOWN && code != sys::errno::ENODEV && code != last_errno
                {
                    logln!("ep0 read error: {e}");
                }
                last_errno = code;
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}
