//! ffs_capture — Orin as the DJI *app* toward the Cendence controller.
//!
//! Presents a FunctionFS USB gadget that answers the controller's Android Open
//! Accessory (AOA) handshake and then exposes the two bulk endpoints over which
//! the controller streams DUML (video + telemetry). Everything the controller
//! sends on the OUT endpoint is parsed with the `duml` library and logged.
//!
//! The configfs plumbing + UDC binding is done by scripts/run_capture.sh; this
//! program drives ep0 (control) and ep1 (bulk OUT).
//!
//! Usage: ffs_capture <ffs_mount_dir>        (run as root)

use dji_orin_interface::{duml, sys};
use std::os::fd::AsRawFd;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

fn logln(s: &str) {
    let t = START.get_or_init(Instant::now).elapsed();
    println!("[{}.{:06}] {}", t.as_secs(), t.subsec_micros(), s);
}

// FunctionFS descriptor/string blob magics and flags.
const DESCRIPTORS_MAGIC_V2: u32 = 3;
const STRINGS_MAGIC: u32 = 2;
const HAS_FS_DESC: u32 = 1;
const HAS_HS_DESC: u32 = 2;
const ALL_CTRL_RECIP: u32 = 1 << 6;

// FunctionFS event types.
const EV_BIND: u8 = 0;
const EV_UNBIND: u8 = 1;
const EV_ENABLE: u8 = 2;
const EV_DISABLE: u8 = 3;
const EV_SETUP: u8 = 4;

// One vendor-specific interface + 2 bulk endpoints (OUT, IN).
const FS_DESC: [u8; 23] = [
    9, 4, 0, 0, 2, 0xff, 0xff, 0x00, 1, // interface
    7, 5, 0x01, 0x02, 64, 0, 0, // ep1 OUT bulk, 64
    7, 5, 0x81, 0x02, 64, 0, 0, // ep2 IN  bulk, 64
];
const HS_DESC: [u8; 23] = [
    9, 4, 0, 0, 2, 0xff, 0xff, 0x00, 1,
    7, 5, 0x01, 0x02, 0x00, 0x02, 0, // 512
    7, 5, 0x81, 0x02, 0x00, 0x02, 0,
];

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn write_descriptors(fd: i32) -> std::io::Result<()> {
    let mut b = Vec::new();
    let total = 20 + FS_DESC.len() + HS_DESC.len();
    put32(&mut b, DESCRIPTORS_MAGIC_V2);
    put32(&mut b, total as u32);
    put32(&mut b, HAS_FS_DESC | HAS_HS_DESC | ALL_CTRL_RECIP);
    put32(&mut b, 3); // fs descriptor count
    put32(&mut b, 3); // hs descriptor count
    b.extend_from_slice(&FS_DESC);
    b.extend_from_slice(&HS_DESC);
    sys::write(fd, &b).map(|_| ())
}

fn write_strings(fd: i32) -> std::io::Result<()> {
    let s = b"DJI-Capture\0";
    let mut b = Vec::new();
    put32(&mut b, STRINGS_MAGIC);
    put32(&mut b, (16 + 2 + s.len()) as u32);
    put32(&mut b, 1); // str count
    put32(&mut b, 1); // lang count
    b.extend_from_slice(&[0x09, 0x04]); // language 0x0409, LE
    b.extend_from_slice(s);
    sys::write(fd, &b).map(|_| ())
}

/// Checked ep0 write; a failure is usually a benign host cancellation on
/// disconnect, but we log it rather than swallow it.
fn ep0_write(fd: i32, data: &[u8]) {
    if let Err(e) = sys::write(fd, data) {
        logln(&format!("  ep0 write failed ({} bytes): {e}", data.len()));
    }
}

fn ep0_ack_read(fd: i32) {
    if let Err(e) = sys::read(fd, &mut []) {
        logln(&format!("  ep0 ack read failed: {e}"));
    }
}

fn handle_setup(fd: i32, s: &[u8]) {
    let bm = s[0];
    let req = s[1];
    let wval = u16::from_le_bytes([s[2], s[3]]);
    let widx = u16::from_le_bytes([s[4], s[5]]);
    let wlen = u16::from_le_bytes([s[6], s[7]]);
    let dir_in = bm & 0x80 != 0;
    let is_vendor = bm & 0x60 == 0x40;
    let aoa = if is_vendor {
        match req {
            51 => " [AOA GET_PROTOCOL]",
            52 => " [AOA SEND_STRING]",
            53 => " [AOA START]",
            _ => " [vendor]",
        }
    } else {
        ""
    };
    logln(&format!(
        "SETUP {} bmRequestType={bm:#04x} bRequest={req:#04x} wValue={wval:#06x} wIndex={widx:#06x} wLength={wlen}{aoa}",
        if dir_in { "IN" } else { "OUT" },
    ));

    // AOA GET_PROTOCOL: report protocol version 2.
    if is_vendor && dir_in && req == 51 {
        let ver = [0x02u8, 0x00];
        ep0_write(fd, &ver[..(wlen as usize).min(2)]);
        return;
    }
    // AOA ACCESSORY_START: ack the status stage.
    if is_vendor && !dir_in && req == 53 {
        ep0_ack_read(fd);
        return;
    }

    if dir_in {
        ep0_write(fd, &[]); // nothing to return; ZLP status
    } else if wlen > 0 {
        let mut data = vec![0u8; (wlen as usize).min(4096)];
        match sys::read(fd, &mut data) {
            Ok(n) if n > 0 => {
                let hex: String = data[..n].iter().map(|b| format!(" {b:02x}")).collect();
                logln(&format!("  OUT-DATA ({n}):{hex}"));
            }
            Ok(_) => {}
            Err(e) => logln(&format!("  ep0 OUT-DATA read failed: {e}")),
        }
    } else {
        ep0_ack_read(fd); // ack no-data OUT
    }
}

fn open_retry(path: &str, tries: u32) -> Option<std::os::fd::OwnedFd> {
    for _ in 0..tries {
        if let Ok(f) = sys::open(path, sys::O_RDONLY) {
            return Some(f);
        }
        sys::usleep(20_000);
    }
    None
}

/// Read ep1 (OUT, controller -> Orin); decode DUML and log.
fn bulk_reader(ffs_dir: String) {
    let path = format!("{ffs_dir}/ep1");
    let mut ep1 = match open_retry(&path, 200) {
        Some(f) => f,
        None => {
            logln("bulk: could not open ep1");
            return;
        }
    };
    logln("bulk: ep1 (OUT, controller->Orin) open; decoding DUML");
    let mut buf = vec![0u8; 16384];
    loop {
        match sys::read(ep1.as_raw_fd(), &mut buf) {
            Ok(0) => sys::usleep(2000),
            Ok(n) => {
                logln(&format!("BULK-IN {n} bytes"));
                let count = duml::scan(&buf[..n], |f| logln(&format!("  DUML {}", f.describe())));
                if count == 0 {
                    let hex: String = buf[..n.min(16)].iter().map(|b| format!(" {b:02x}")).collect();
                    logln(&format!("  (no DUML frame; first bytes:{hex})"));
                }
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == sys::EINTR || code == sys::EAGAIN {
                    sys::usleep(2000);
                    continue;
                }
                logln(&format!("bulk: ep1 error {e}; reopening"));
                loop {
                    if let Some(f) = open_retry(&path, 1) {
                        ep1 = f;
                        break;
                    }
                    sys::usleep(50_000);
                }
            }
        }
    }
}

fn main() {
    START.get_or_init(Instant::now);
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <ffs_mount_dir>", args[0]);
        std::process::exit(1);
    }
    let ffs_dir = args[1].clone();

    let ep0 = sys::open(&format!("{ffs_dir}/ep0"), sys::O_RDWR).unwrap_or_else(|e| {
        eprintln!("open ep0: {e}");
        std::process::exit(1);
    });
    let fd = ep0.as_raw_fd();

    if let Err(e) = write_descriptors(fd).and_then(|_| write_strings(fd)) {
        eprintln!("write descriptors/strings: {e}");
        std::process::exit(1);
    }
    logln("descriptors+strings written; waiting for controller");

    let mut started = false;
    let mut last_errno = 0;
    let mut ev = [0u8; 12 * 8];
    loop {
        match sys::read(fd, &mut ev) {
            Ok(0) => sys::usleep(2000),
            Ok(n) => {
                last_errno = 0;
                let count = n / 12;
                for i in 0..count {
                    let e = &ev[i * 12..i * 12 + 12];
                    match e[8] {
                        EV_BIND => logln("EVENT BIND"),
                        EV_UNBIND => logln("EVENT UNBIND"),
                        EV_ENABLE => {
                            logln("EVENT ENABLE");
                            if !started {
                                started = true;
                                let d = ffs_dir.clone();
                                std::thread::spawn(move || bulk_reader(d));
                            }
                        }
                        EV_DISABLE => logln("EVENT DISABLE"),
                        EV_SETUP => handle_setup(fd, &e[0..8]),
                        _ => {}
                    }
                }
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == sys::EINTR {
                    continue;
                }
                // ESHUTDOWN/ENODEV is normal with no host attached; back off
                // quietly. Log any other error once (not every spin).
                if code != sys::ESHUTDOWN && code != sys::ENODEV && code != last_errno {
                    logln(&format!("ep0 read error: {e}"));
                }
                last_errno = code;
                sys::usleep(50_000);
            }
        }
    }
}
