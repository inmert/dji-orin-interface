//! server — Orin as the DJI *app* toward the Cendence RC, with a web UI.
//!
//! The Orin presents the FunctionFS accessory gadget (exactly as `ffs_capture`
//! does), so no DJI tablet is in the loop: the RC relays to the real aircraft.
//! From the bulk streams it decodes telemetry ([`reader`]) and injects gimbal
//! commands ([`injector`]). A web front end ([`http`]) talks to it over a
//! WebSocket ([`ws`]): telemetry down, gimbal commands up ([`state`]).
//! Independently, [`flight`] reads what the aircraft pushes on its own ports:
//! the OSDK UART broadcast (attitude, velocity, GPS, battery, status) and the
//! DUML reports on its USB API port (attitude, flight mode). Those readers
//! never send; only [`motors`] does, on the UI's explicit request, over the
//! RC link first and else the UART (with the USB port for the flight unlock).
//! [`video`] decodes the cameras' H.264 on the Orin's hardware decoder and
//! streams it to the browser as MJPEG while someone watches: the gimbal
//! cameras from the RC's USB mux, the FPV camera over UDT on the aircraft's
//! USB network.
//!
//! ```text
//!   browser  ──HTTP :8080 / WS :8081──  server (Orin, gadget)  ──USB──  RC ── aircraft
//!                                              ├──── OSDK UART (rx; motors tx) ──┤
//!                                              ├──── USB API port (rx; unlock tx)┤
//!                                              └──── USB network, UDT video ─────┘
//! ```
//!
//! SAFETY: the gimbal commands move the real camera mount via the powered
//! aircraft. Nothing is injected until the UI sends a command; a WS disconnect
//! or the UI STOP releases every gimbal. Run props-off / aircraft secured.
//!
//! Usage: `server <ffs_mount_dir> [web_root]`        (run as root)
//!
//! `OSDK_UART` picks the flight-data port (default: a CP210x USB-TTL adapter if
//! one is plugged in, else `/dev/ttyTHS1`; `off` disables it) and `OSDK_BAUD`
//! its rate (default 230400). `USB_ACM` picks the aircraft's USB API serial
//! port (default: the first `ttyACM` that is not the CrystalSky; `off`
//! disables it). Defaults are chosen again on every reopen ([`ports`]).
//! `OSDK_APP_ID` is the Onboard SDK app the UI's motor start/stop activates
//! (unset disables those buttons' commands; see [`motors`]).
//! `FPV_UDT` is the FPV camera's UDT address (default `192.168.42.2:40003`;
//! `off` disables that feed). `RC_VIDEO` maps the gimbal-camera feeds to RC
//! mux channels (default `cam1=0x11,cam2=0x15`; `off` disables them; adding
//! `fpv=0x13` takes the FPV feed from the RC instead of UDT). `RC_DUMP=<file>` appends the raw RC→Orin bytes
//! to a file for offline analysis.

mod flight;
mod http;
mod injector;
mod motors;
mod ports;
mod reader;
mod state;
mod thermal_auto;
mod video;
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
use video::{Feed, Source};

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

/// The camera feeds from `FPV_UDT` and `RC_VIDEO` (see the module docs).
/// Nothing is opened until a viewer subscribes.
fn video_feeds() -> video::Feeds {
    let mut feeds = video::Feeds::new();
    let fpv = match std::env::var("FPV_UDT").as_deref() {
        Ok("off") => None,
        Ok(addr) => Some(
            addr.parse::<SocketAddrV4>()
                .unwrap_or_else(|e| die(&format!("FPV_UDT={addr}: {e}"))),
        ),
        Err(_) => Some(SocketAddrV4::new([192, 168, 42, 2].into(), 40003)),
    };
    if let Some(addr) = fpv {
        // 608×448 already: keep the stream's own size.
        feeds.insert("fpv", Feed::new("fpv", Source::Udt(addr), None));
    }
    let rc = std::env::var("RC_VIDEO").unwrap_or_else(|_| "cam1=0x11,cam2=0x15".into());
    if rc != "off" {
        for entry in rc.split(',') {
            let parsed = entry.split_once('=').and_then(|(name, ch)| {
                let name = match name.trim() {
                    "cam1" => "cam1",
                    "cam2" => "cam2",
                    "fpv" => "fpv",
                    _ => return None,
                };
                let ch = u8::from_str_radix(ch.trim().trim_start_matches("0x"), 16).ok()?;
                Some((name, ch))
            });
            let Some((name, ch)) = parsed else {
                die(&format!(
                    "RC_VIDEO: bad entry {entry:?} (want cam1=0x11,cam2=0x15[,fpv=0x13])"
                ));
            };
            // The gimbal cameras' 1280×720 streams are scaled down to keep the
            // MJPEG light; the FPV's 608×448 keeps its size. An RC FPV feed
            // replaces the UDT one.
            let scale = (name != "fpv").then_some((640, 360));
            feeds.insert(name, Feed::new(name, Source::Rc(ch), scale));
        }
    }
    feeds
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

    let feeds = Arc::new(video_feeds());
    let rc_feeds: Vec<_> = feeds
        .values()
        .filter(|f| f.rc_channel().is_some())
        .cloned()
        .collect();

    // Web front end + WebSocket come up immediately (independent of the USB link).
    {
        let feeds = Arc::clone(&feeds);
        std::thread::spawn(move || http::serve(web_root, feeds));
    }
    {
        let (cmds, tele) = (Arc::clone(&cmds), Arc::clone(&tele));
        std::thread::spawn(move || ws::serve(cmds, tele, feeds));
    }

    // Thermal auto range: idle until the UI turns it on.
    {
        let (tele, cmds) = (Arc::clone(&tele), Arc::clone(&cmds));
        std::thread::spawn(move || thermal_auto::run(tele, cmds));
    }

    // Motor start/stop (sends on the SDK UART only when the UI asks).
    {
        let app_id = std::env::var("OSDK_APP_ID").ok().map(|v| {
            v.parse::<u32>()
                .unwrap_or_else(|e| die(&format!("OSDK_APP_ID={v}: {e}")))
        });
        if app_id.is_none() {
            logln!("motors: OSDK_APP_ID not set; motor commands disabled");
        }
        let (cmds, tele) = (Arc::clone(&cmds), Arc::clone(&tele));
        std::thread::spawn(move || motors::run(cmds, tele, app_id));
    }

    // Direct aircraft ports: read here, independent of the RC link. An env
    // var fixes the port (`off` disables the reader); unset, [`ports`] picks
    // one on every open attempt.
    let fixed = |var| std::env::var(var).ok();
    let uart = fixed("OSDK_UART");
    if uart.as_deref() != Some("off") {
        let baud = std::env::var("OSDK_BAUD")
            .ok()
            .and_then(|b| b.parse().ok())
            .unwrap_or(230_400);
        let tele = Arc::clone(&tele);
        let pick = move || Some(uart.clone().unwrap_or_else(ports::osdk_uart));
        std::thread::spawn(move || flight::run::<flight::Osdk>(pick, baud, tele));
    }
    let acm = fixed("USB_ACM");
    if acm.as_deref() != Some("off") {
        // CDC-ACM ignores the line rate; any supported value works.
        let tele = Arc::clone(&tele);
        let pick = move || acm.clone().or_else(ports::usb_acm);
        std::thread::spawn(move || flight::run::<flight::Usb>(pick, 115_200, tele));
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
                        let (tele, rc_feeds) = (Arc::clone(&tele), rc_feeds.clone());
                        reader_h =
                            Some(std::thread::spawn(move || reader::run(ep1, tele, rc_feeds)));
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
