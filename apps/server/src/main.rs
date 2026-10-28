//! server — Orin as the DJI *app* toward the Cendence RC, with a web UI.
//!
//! The Orin presents the FunctionFS accessory gadget (exactly as `ffs_capture`
//! does), so no DJI tablet is in the loop: the RC relays to the real aircraft.
//! From the bulk streams it decodes telemetry ([`reader`]) and injects gimbal
//! commands ([`injector`]). A web front end ([`http`]) talks to it over a
//! WebSocket ([`ws`]): telemetry down, gimbal commands up ([`state`]).
//! Independently, [`flight`] reads what the aircraft pushes on its own ports,
//! receive-only: the OSDK UART broadcast (attitude, velocity, GPS, battery,
//! status) and the DUML reports on its USB API port (attitude, flight mode).
//! [`fpv`] streams the FPV camera's H.264 (UDT over the aircraft's USB network)
//! to the browser as MJPEG, connecting only while someone watches.
//!
//! ```text
//!   browser  ──HTTP :8080 / WS :8081──  server (Orin, gadget)  ──USB──  RC ── aircraft
//!                                              ├──── OSDK UART (rx only) ────────┤
//!                                              ├──── USB API port (rx only) ─────┤
//!                                              └──── USB network, UDT video ─────┘
//! ```
//!
//! SAFETY: the gimbal commands move the real camera mount via the powered
//! aircraft. Nothing is injected until the UI sends a command; a WS disconnect
//! or the UI STOP releases every gimbal. Run props-off / aircraft secured.
//!
//! Usage: `server <ffs_mount_dir> [web_root]`        (run as root)
//!
//! `OSDK_UART` picks the flight-data port (default `/dev/ttyTHS1`; `off`
//! disables it) and `OSDK_BAUD` its rate (default 230400). `USB_ACM` picks the
//! aircraft's USB API serial port (default `/dev/ttyACM0`; `off` disables it).
//! `FPV_UDT` is the FPV camera's UDT address (default `192.168.42.2:40003`;
//! `off` disables the video).

mod flight;
mod fpv;
mod http;
mod injector;
mod reader;
mod state;
mod ws;

use std::net::SocketAddrV4;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{JoinHandle, sleep};
use std::time::Duration;

use dji_common::logln;
use dji_protocol::Telemetry;
use dji_transport::aoa::device::answer_setup;
use dji_transport::usb::ffs::{self, Event};

use reader::Tele;
use state::{Commands, Shared};

const HTTP_ADDR: &str = "0.0.0.0:8080";
const WS_ADDR: &str = "0.0.0.0:8081";
/// Command/telemetry rate, matching the ~10 Hz Pilot drove the gimbal at.
const INJECT_HZ: u64 = 10;

fn die(msg: &str) -> ! {
    logln!("{msg}");
    std::process::exit(1);
}

/// Park the main thread forever, keeping the spawned web/WS threads alive.
fn park() -> ! {
    loop {
        sleep(Duration::from_secs(3600));
    }
}

/// Whether a worker slot needs (re)spawning: never started, or exited.
fn needs_spawn(h: &Option<JoinHandle<()>>) -> bool {
    h.as_ref().is_none_or(JoinHandle::is_finished)
}

fn main() {
    dji_common::log::init(None);
    let args: Vec<String> = std::env::args().collect();
    let Some(ffs_dir) = args.get(1).cloned() else {
        eprintln!("usage: {} <ffs_mount_dir> [web_root]", args[0]);
        std::process::exit(1);
    };
    let web_root = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "web/dist".into()));

    let cmds: Shared = Arc::new(Mutex::new(Commands::default()));
    let tele: Tele = Arc::new(Mutex::new(Telemetry::default()));

    // FPV video: nothing connects until a viewer opens the stream.
    let fpv = match std::env::var("FPV_UDT").as_deref() {
        Ok("off") => None,
        Ok(addr) => Some(
            addr.parse::<SocketAddrV4>()
                .unwrap_or_else(|e| die(&format!("FPV_UDT={addr}: {e}"))),
        ),
        Err(_) => Some(SocketAddrV4::new([192, 168, 42, 2].into(), 40003)),
    }
    .map(fpv::Fpv::new);

    // Web front end + WebSocket come up immediately (independent of the USB link).
    {
        let fpv = fpv.clone();
        std::thread::spawn(move || http::serve(web_root, fpv));
    }
    {
        let (cmds, tele) = (Arc::clone(&cmds), Arc::clone(&tele));
        std::thread::spawn(move || ws::serve(cmds, tele, fpv));
    }

    // Direct aircraft ports: receive-only, independent of the RC link.
    let port = |var, default: &str| {
        let p = std::env::var(var).unwrap_or_else(|_| default.into());
        (p != "off").then_some(p)
    };
    if let Some(uart) = port("OSDK_UART", "/dev/ttyTHS1") {
        let baud = std::env::var("OSDK_BAUD")
            .ok()
            .and_then(|b| b.parse().ok())
            .unwrap_or(230_400);
        let tele = Arc::clone(&tele);
        std::thread::spawn(move || flight::run::<flight::Osdk>(uart, baud, tele));
    }
    if let Some(acm) = port("USB_ACM", "/dev/ttyACM0") {
        // CDC-ACM ignores the line rate; any supported value works.
        let tele = Arc::clone(&tele);
        std::thread::spawn(move || flight::run::<flight::Usb>(acm, 115_200, tele));
    }

    // USB gadget side: descriptors, then the ep0 event loop. run_server.sh binds
    // the UDC after we log "gadget ready". If the gadget isn't present (e.g. run
    // without run_server.sh), keep serving the web UI instead of exiting.
    let ep0 = match ffs::init_ep0(&ffs_dir) {
        Ok(fd) => fd,
        Err(e) => {
            logln!("gadget: init failed ({e}); serving web UI only, no USB link");
            park();
        }
    };
    logln!("gadget ready; waiting for the RC to enable it");

    // The workers survive a DISABLE/ENABLE cycle (RC power-cycle) on their own;
    // one that exited (endpoint gone) is respawned on the next ENABLE.
    let mut reader_h: Option<JoinHandle<()>> = None;
    let mut injector_h: Option<JoinHandle<()>> = None;
    ffs::run_events(ep0.as_fd(), |event| match event {
        Event::Enable => {
            logln!("gadget: ENABLE");
            let open = |name| ffs::open_endpoint(&ffs_dir, name, 200);
            if needs_spawn(&reader_h) {
                match open("ep1") {
                    Ok(ep1) => {
                        let tele = Arc::clone(&tele);
                        reader_h = Some(std::thread::spawn(move || reader::run(ep1, tele)));
                        logln!("link up: telemetry reader running");
                    }
                    Err(e) => logln!("gadget: {e}"),
                }
            }
            if needs_spawn(&injector_h) {
                match open("ep2") {
                    Ok(ep2) => {
                        let cmds = Arc::clone(&cmds);
                        injector_h = Some(std::thread::spawn(move || injector::run(ep2, cmds)));
                        logln!("link up: injector running");
                    }
                    Err(e) => logln!("gadget: {e}"),
                }
            }
        }
        Event::Disable => {
            logln!("gadget: DISABLE");
            tele.lock().unwrap().link_up = false;
        }
        Event::Setup(s) => {
            let _ = answer_setup(ep0.as_fd(), &s);
        }
        Event::Bind => logln!("gadget: BIND"),
        Event::Unbind => logln!("gadget: UNBIND"),
        Event::Other(_) => {}
    });
}
