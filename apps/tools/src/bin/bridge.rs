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
//! Usage: `bridge <ffs_mount_dir>`        (run as root)
//!
//! `BRIDGE_DUMP_DIR=<dir>` also saves both raw streams for offline analysis
//! (e.g. of the video mux): `rc2tab.bin` / `tab2rc.bin`, each with an `.idx`
//! of `<f64 seconds since start, u32 byte offset>` (little-endian) per read.

use std::fs::File;
use std::io::{BufWriter, ErrorKind, Write};
use std::os::fd::AsFd;
use std::sync::{Arc, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

use dji_common::log::hex;
use dji_common::logln;
use dji_protocol::duml;
use dji_transport::aoa::device::answer_setup;
use dji_transport::aoa::{self, host};
use dji_transport::usb;
use dji_transport::usb::ffs::{self, Event};
use dji_transport::usb::usbfs::{self, BulkEndpoints, Device};

const VID: u16 = aoa::CRYSTALSKY_VID;
const PID: u16 = aoa::CRYSTALSKY_PID;

fn die(msg: &str) -> ! {
    logln!("{msg}");
    std::process::exit(1);
}

/// Bring up the tablet side and return its claimed accessory + bulk endpoints.
///
/// Idempotent across runs: if the tablet is already in AOA accessory mode (a
/// previous run left it there — teardown does not reset it), skip the handshake
/// and open it directly. Otherwise run the AOA host handshake against the DJI
/// device and wait for it to re-enumerate in accessory mode.
fn open_tablet() -> (Device, BulkEndpoints) {
    let open = |addr| host::open_accessory(addr).unwrap_or_else(|e| die(&format!("tablet: {e}")));

    if let Some((addr, apid)) = host::find_accessory() {
        logln!(
            "tablet: already in accessory mode {:04x}:{apid:04x}; skipping handshake",
            aoa::GOOGLE_VID
        );
        return open(addr);
    }

    let Some(addr) = usbfs::find_device(VID, PID) else {
        die(&format!(
            "DJI {VID:04x}:{PID:04x} not found and no accessory present; \
             plug the CrystalSky into a USB-A port (or replug it to reset accessory mode)."
        ));
    };
    logln!("tablet: found DJI at bus {} dev {}", addr.bus, addr.dev);
    let dev = Device::open(addr).unwrap_or_else(|e| die(&format!("tablet: open: {e}")));
    if let Err(e) = host::handshake(&dev) {
        die(&format!("tablet: AOA handshake failed: {e}"));
    }
    drop(dev);
    logln!("tablet: waiting for accessory-mode re-enumeration (tap OK on the screen if asked)");

    let (addr, apid) = host::wait_for_accessory(200)
        .unwrap_or_else(|| die("tablet: accessory device never appeared"));
    logln!(
        "tablet: accessory mode up {:04x}:{apid:04x}",
        aoa::GOOGLE_VID
    );
    sleep(Duration::from_millis(200));
    open(addr)
}

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

/// When the bridge started, for the dump indexes.
static START: OnceLock<Instant> = OnceLock::new();

/// Size limit for each direction's dump, so a forgotten dump can't fill the
/// disk (and every offset fits the index's u32).
const DUMP_CAP: u64 = 512 * 1024 * 1024;

/// Raw copy of one direction (`BRIDGE_DUMP_DIR`): the bytes plus an index of
/// when each read arrived. Write errors and the size cap stop the dump, never
/// the relay. An existing dump is never overwritten: use a new directory.
struct Dump {
    data: BufWriter<File>,
    idx: BufWriter<File>,
    offset: u64,
}

impl Dump {
    fn open(name: &str) -> Option<Self> {
        let dir = std::env::var("BRIDGE_DUMP_DIR").ok()?;
        let path = |ext| format!("{dir}/{name}.{ext}");
        if let Some(p) = [path("bin"), path("idx")]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
        {
            logln!("dump: {p} exists; not overwriting it (set a new BRIDGE_DUMP_DIR); no dump");
            return None;
        }
        let create = |ext| File::options().write(true).create_new(true).open(path(ext));
        match (create("bin"), create("idx")) {
            (Ok(data), Ok(idx)) => {
                logln!("dump: {dir}/{name}.bin");
                Some(Self {
                    data: BufWriter::new(data),
                    idx: BufWriter::new(idx),
                    offset: 0,
                })
            }
            (Err(e), _) | (_, Err(e)) => {
                logln!("dump: cannot create {dir}/{name}: {e}");
                None
            }
        }
    }

    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        if self.offset + chunk.len() as u64 > DUMP_CAP {
            let _ = self.data.flush().and_then(|()| self.idx.flush());
            return Err(std::io::Error::other(format!("reached {DUMP_CAP} bytes")));
        }
        let secs = START.get_or_init(Instant::now).elapsed().as_secs_f64();
        self.idx.write_all(&secs.to_le_bytes())?;
        self.idx.write_all(
            &u32::try_from(self.offset)
                .expect("offset below DUMP_CAP")
                .to_le_bytes(),
        )?;
        self.data.write_all(chunk)?;
        self.offset += chunk.len() as u64;
        Ok(())
    }
}

/// Forward one raw bulk stream verbatim, logging the CRC-valid DUML it carries.
/// `label` tags the log lines; `read`/`write` move the bytes; `dump` names the
/// raw copy written when `BRIDGE_DUMP_DIR` is set.
fn relay(
    label: &'static str,
    dump: &str,
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
    write: impl Fn(&[u8]) -> std::io::Result<usize>,
) {
    let hex_sets = hex_sets_from_env();
    let mut dump = Dump::open(dump);
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
                        Ok(0) => {
                            logln!("{label}: forward wrote 0 bytes; dropped {}", n - sent);
                            break;
                        }
                        Ok(w) => sent += w,
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(e) => {
                            logln!("{label}: forward error: {e}; dropped {}", n - sent);
                            break;
                        }
                    }
                }
                if let Some(d) = dump.as_mut()
                    && let Err(e) = d.write(&buf[..n])
                {
                    logln!("{label}: dump write failed: {e}; dump stopped");
                    dump = None;
                }
                decoder.push(&buf[..n]);
                decoder.drain(|f| {
                    if f.crc_ok {
                        if hex_sets.contains(&f.cmd_set) {
                            logln!(
                                "{label} {f} type=0x{:02x} | payload={}",
                                f.cmd_type,
                                hex(f.payload)
                            );
                        } else {
                            logln!("{label} {f}");
                        }
                    }
                });
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == usb::errno::ENODEV || code == usb::errno::ESHUTDOWN {
                    logln!("{label}: endpoint gone; relay stopping");
                    if let Some(d) = dump.as_mut() {
                        let _ = d.data.flush().and_then(|()| d.idx.flush());
                    }
                    return;
                }
                logln!("{label}: read error: {e}");
                sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Spawn the two relay threads once both sides are up.
fn start_relays(ffs_dir: &str, tablet: &Arc<Device>, eps: BulkEndpoints) {
    let open = |name| {
        ffs::open_endpoint(ffs_dir, name, 200).unwrap_or_else(|e| die(&format!("gadget: {e}")))
    };
    let ep1 = open("ep1"); // OUT: RC -> Orin
    let ep2 = open("ep2"); // IN:  Orin -> RC
    logln!("relays: ep1/ep2 open; forwarding (passthrough, no injection)");

    // RC -> tablet: read ep1, write tablet bulk OUT.
    {
        let tablet = Arc::clone(tablet);
        let out_ep = eps.out_addr;
        std::thread::spawn(move || {
            relay(
                "RC->TAB",
                "rc2tab",
                |buf| usb::read(ep1.as_fd(), buf),
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
                "tab2rc",
                |buf| tablet.bulk_read(in_ep, buf, 0),
                |data| usb::write(ep2.as_fd(), data),
            );
        });
    }
}

fn main() {
    dji_common::log::init(None);
    START.get_or_init(Instant::now);
    let args: Vec<String> = std::env::args().collect();
    let Some(ffs_dir) = args.get(1).cloned() else {
        eprintln!("usage: {} <ffs_mount_dir>", args[0]);
        std::process::exit(1);
    };

    // Fail fast if the tablet is not even present, before we touch the gadget.
    if usbfs::find_device(VID, PID).is_none() && host::find_accessory().is_none() {
        die("DJI tablet not found; plug the CrystalSky into a USB-A port first.");
    }

    // Gadget side: write descriptors so run_bridge.sh can bind the UDC. The RC
    // then enumerates us; its BIND/ENABLE/SETUP events queue on ep0 until the
    // event loop below drains them, so bringing the tablet up first is fine.
    let ep0 = ffs::init_ep0(&ffs_dir).unwrap_or_else(|e| die(&format!("gadget init: {e}")));
    logln!("descriptors+strings written; bringing up tablet side");

    // Tablet side (blocks through the AOA handshake, possibly a screen tap).
    let (tablet, eps) = open_tablet();
    let tablet = Arc::new(tablet);
    logln!("both sides staged; waiting for RC to enable the gadget");

    // ep0 event loop: on ENABLE, open the bulk endpoints and start the relays.
    let mut started = false;
    ffs::run_events(ep0.as_fd(), |event| match event {
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
        Event::Setup(s) => {
            let _ = answer_setup(ep0.as_fd(), &s);
        }
        Event::Other(_) => {}
    });
}
