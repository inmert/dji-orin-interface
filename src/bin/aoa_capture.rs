//! aoa_capture — Orin as the *controller* toward a DJI CrystalSky / DJI Pilot.
//!
//! Acts as a USB host and performs the Android Open Accessory (AOA) handshake
//! using the exact accessory identity the real Cendence controller advertises,
//! so DJI Pilot connects to the Orin and begins emitting its DUML command
//! stream. All DUML the tablet sends is parsed with the `duml` library and
//! logged.
//!
//! Uses only the kernel usbfs ioctl interface (via the `sys` module).
//!
//! Usage: aoa_capture [logfile]        (run as root)

use core::ffi::c_void;
use dji_orin_interface::{duml, sys};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const DJI_VID: u32 = 0x2ca3; // DJI CrystalSky (ZS600B) default mode
const DJI_PID: u32 = 0x4016;
const AOA_VID: u32 = 0x18d1; // Google — Android Open Accessory mode

// AOA accessory identity captured from the real Cendence controller.
const AOA_STRINGS: [&str; 6] = ["DJI", "T600", "DJI Inspire 1", "v1.0.2.6", "www.dji.com", "000000000000000"];

static START: OnceLock<Instant> = OnceLock::new();
static OUT: OnceLock<Mutex<Box<dyn Write + Send>>> = OnceLock::new();

fn logln(s: &str) {
    let t = START.get_or_init(Instant::now).elapsed();
    let line = format!("[{}.{:06}] {}", t.as_secs(), t.subsec_micros(), s);
    if let Some(m) = OUT.get() {
        if let Ok(mut w) = m.lock() {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    } else {
        println!("{line}");
    }
}

fn read_radix(path: &Path, radix: u32) -> Option<u32> {
    let s = std::fs::read_to_string(path).ok()?;
    u32::from_str_radix(s.trim(), radix).ok()
}

/// Find a connected device by VID/PID; returns (busnum, devnum).
fn find_dev(vid: u32, pid: u32) -> Option<(u32, u32)> {
    let rd = std::fs::read_dir("/sys/bus/usb/devices").ok()?;
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.contains(':') {
            continue;
        }
        let p = entry.path();
        if read_radix(&p.join("idVendor"), 16) == Some(vid)
            && read_radix(&p.join("idProduct"), 16) == Some(pid)
        {
            if let (Some(bus), Some(dev)) =
                (read_radix(&p.join("busnum"), 10), read_radix(&p.join("devnum"), 10))
            {
                return Some((bus, dev));
            }
        }
    }
    None
}

fn open_dev(bus: u32, dev: u32) -> std::io::Result<OwnedFd> {
    sys::open(&format!("/dev/bus/usb/{bus:03}/{dev:03}"), sys::O_RDWR)
}

#[allow(clippy::too_many_arguments)]
fn ctrl(fd: i32, rtype: u8, req: u8, val: u16, idx: u16, data: *mut c_void, len: u16, tmo: u32) -> std::io::Result<i32> {
    let mut t = sys::CtrlTransfer {
        request_type: rtype,
        request: req,
        value: val,
        index: idx,
        length: len,
        timeout: tmo,
        data,
    };
    // SAFETY: `t` is a correctly-sized usbfs ctrltransfer for USBDEVFS_CONTROL,
    // and `data` (the caller's buffer) is valid for `len` bytes.
    unsafe { sys::ioctl(fd, sys::USBDEVFS_CONTROL, &mut t as *mut _ as *mut c_void) }
}

fn die(msg: &str) -> ! {
    logln(msg);
    std::process::exit(1);
}

fn main() {
    START.get_or_init(Instant::now);
    let args: Vec<String> = std::env::args().collect();
    let sink: Box<dyn Write + Send> = if args.len() > 1 {
        match File::create(&args[1]) {
            Ok(f) => Box::new(f),
            Err(e) => {
                eprintln!("logfile {}: {e}", args[1]);
                std::process::exit(1);
            }
        }
    } else {
        Box::new(std::io::stdout())
    };
    let _ = OUT.set(Mutex::new(sink));

    let (bus, dev) = match find_dev(DJI_VID, DJI_PID) {
        Some(x) => x,
        None => {
            eprintln!("DJI {DJI_VID:04x}:{DJI_PID:04x} not found. Plug the CrystalSky into a Orin USB-A port.");
            std::process::exit(1);
        }
    };
    logln(&format!("found DJI {DJI_VID:04x}:{DJI_PID:04x} at bus {bus} dev {dev}"));
    let devfd = match open_dev(bus, dev) {
        Ok(f) => f,
        Err(e) => die(&format!("open device: {e}")),
    };
    let fd = devfd.as_raw_fd();

    // GET_PROTOCOL
    let mut ver = [0u8; 2];
    if let Err(e) = ctrl(fd, 0xC0, 51, 0, 0, ver.as_mut_ptr() as *mut c_void, 2, 1000) {
        die(&format!("GET_PROTOCOL failed: {e} (no AOA support?)"));
    }
    let proto = ver[0] as u16 | ((ver[1] as u16) << 8);
    logln(&format!("AOA protocol version = {proto}"));
    if proto < 1 {
        die("protocol < 1, aborting");
    }

    // SEND_STRING x6
    let mut fails = 0;
    for (i, s) in AOA_STRINGS.iter().enumerate() {
        let mut cbuf = s.as_bytes().to_vec();
        cbuf.push(0); // NUL-terminated
        let r = ctrl(fd, 0x40, 52, 0, i as u16, cbuf.as_mut_ptr() as *mut c_void, cbuf.len() as u16, 1000);
        match r {
            Ok(_) => logln(&format!("SEND_STRING[{i}]=\"{s}\" -> ok")),
            Err(e) => {
                fails += 1;
                logln(&format!("SEND_STRING[{i}]=\"{s}\" -> {e}"));
            }
        }
    }
    if fails > 0 {
        logln(&format!("warning: {fails} SEND_STRING request(s) failed"));
    }

    // ACCESSORY_START
    match ctrl(fd, 0x40, 53, 0, 0, std::ptr::null_mut(), 0, 1000) {
        Ok(_) => logln("ACCESSORY_START -> ok"),
        Err(e) => die(&format!("ACCESSORY_START failed: {e}; aborting")),
    }
    drop(devfd);
    logln("waiting for accessory-mode re-enumeration...");
    logln("** tap OK on any USB-accessory prompt on the CrystalSky screen **");

    // Wait for accessory device (18d1:2d00 / 2d01).
    let mut acc = None;
    let mut apid = 0;
    for _ in 0..200 {
        if let Some(x) = find_dev(AOA_VID, 0x2d01) {
            acc = Some(x);
            apid = 0x2d01;
            break;
        }
        if let Some(x) = find_dev(AOA_VID, 0x2d00) {
            acc = Some(x);
            apid = 0x2d00;
            break;
        }
        sys::usleep(50_000);
    }
    let (bus, dev) = match acc {
        Some(x) => x,
        None => die("accessory device never appeared; giving up."),
    };
    logln(&format!("accessory mode up: {AOA_VID:04x}:{apid:04x} bus {bus} dev {dev}"));
    sys::usleep(200_000);
    let accfd = match open_dev(bus, dev) {
        Ok(f) => f,
        Err(e) => die(&format!("open accessory: {e}")),
    };
    let fd = accfd.as_raw_fd();

    // Find interface 0's bulk endpoints from the device's descriptors.
    let mut desc = [0u8; 2048];
    let dn = match sys::read(fd, &mut desc) {
        Ok(n) if n > 0 => n,
        Ok(_) => die("failed to read descriptors: empty"),
        Err(e) => die(&format!("failed to read descriptors: {e}")),
    };
    let (mut ep_in, mut ep_out) = (0u8, 0u8);
    let mut i = 0;
    while i + 1 < dn {
        let blen = desc[i] as usize;
        let btype = desc[i + 1];
        if blen < 2 {
            break;
        }
        if btype == 0x05 && desc[i + 3] & 0x03 == 0x02 {
            let addr = desc[i + 2];
            if addr & 0x80 != 0 {
                if ep_in == 0 {
                    ep_in = addr;
                }
            } else if ep_out == 0 {
                ep_out = addr;
            }
        }
        i += blen;
    }
    logln(&format!("endpoints: bulk IN={ep_in:#04x} OUT={ep_out:#04x}"));
    if ep_in == 0 {
        die("no bulk IN endpoint");
    }

    // Claim interface 0 (detach any kernel driver first).
    let mut dc = sys::DisconnectClaim { interface: 0, flags: 0, driver: [0; 256] };
    // SAFETY: each arg points to a correctly-sized struct/int for its request.
    let claimed = unsafe { sys::ioctl(fd, sys::USBDEVFS_DISCONNECT_CLAIM, &mut dc as *mut _ as *mut c_void) };
    if claimed.is_ok() {
        logln("claimed interface 0 (detached kernel driver)");
    } else {
        let mut ifnum: u32 = 0;
        match unsafe { sys::ioctl(fd, sys::USBDEVFS_CLAIMINTERFACE, &mut ifnum as *mut _ as *mut c_void) } {
            Ok(_) => logln("claimed interface 0"),
            Err(e) => logln(&format!("warning: could not claim interface 0: {e} (bulk reads may fail)")),
        }
    }

    // Capture the bulk IN endpoint (CrystalSky/Pilot -> Orin).
    logln(&format!("capturing DUML from DJI Pilot on EP {ep_in:#04x} ..."));
    let mut buf = vec![0u8; 16384];
    loop {
        let mut bt = sys::BulkTransfer {
            ep: ep_in as u32,
            len: buf.len() as u32,
            timeout: 500,
            data: buf.as_mut_ptr() as *mut c_void,
        };
        // SAFETY: `bt` is a valid usbfs bulktransfer and `buf` is valid for `len`.
        match unsafe { sys::ioctl(fd, sys::USBDEVFS_BULK, &mut bt as *mut _ as *mut c_void) } {
            Ok(n) if n > 0 => {
                let n = n as usize;
                logln(&format!("BULK-IN {n} bytes"));
                let count = duml::scan(&buf[..n], |f| logln(&format!("  DUML {}", f.describe())));
                if count == 0 {
                    let hex: String = buf[..n.min(16)].iter().map(|b| format!(" {b:02x}")).collect();
                    logln(&format!("  (no DUML frame; first bytes:{hex})"));
                }
            }
            Ok(_) => {}
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == sys::ETIMEDOUT || code == sys::EINTR {
                    continue;
                }
                if code == sys::ENODEV || code == sys::ESHUTDOWN {
                    logln("device disconnected; stopping capture");
                    break;
                }
                logln(&format!("bulk read error: {e}"));
                sys::usleep(100_000);
            }
        }
    }
}
