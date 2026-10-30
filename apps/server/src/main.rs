//! server — Orin as the DJI *app* toward the Cendence RC, with a web UI.
//!
//! The Orin presents the FunctionFS accessory gadget (exactly as `ffs_capture`
//! does), so no DJI tablet is in the loop: the RC relays to the real aircraft.
//! The aircraft itself is a [`Vehicle`] from `dji-vehicle`: its RC link
//! reads telemetry and writes gimbal/thermal/motor frames on the bulk
//! endpoints, and its serial links read what the aircraft pushes on its own
//! ports. This binary is the presentation layer: the gadget's ep0 event
//! loop, the choice of serial ports ([`ports`]), a web front end ([`http`],
//! plain on :8080 and TLS on :8443 with a self-signed certificate
//! ([`tls`])) and its WebSocket ([`ws`]) that streams the snapshot down and
//! maps the portal's messages ([`portal`]) onto the vehicle's typed
//! operations, plus voice input for the portal's microphone ([`voice`],
//! forwarded to an online speech-recognition service).
//! [`video`] decodes the cameras' H.264 on the Orin's hardware decoder and
//! streams it to the browser as MJPEG while someone watches: the gimbal
//! cameras from the RC's USB mux, the FPV camera over UDT on the aircraft's
//! USB network.
//!
//! ```text
//!   browser  ──HTTP :8080 / HTTPS :8443──  server (Orin, gadget)  ──USB──  RC ── aircraft
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
//! on the UART path (unset leaves only the RC link; see
//! [`dji_vehicle::motors`]).
//! `FPV_UDT` is the FPV camera's UDT address (default `192.168.42.2:40003`;
//! `off` disables that feed). `RC_VIDEO` maps the gimbal-camera feeds to RC
//! mux channels (default `cam1=0x11,cam2=0x15`; `off` disables them; adding
//! `fpv=0x13` takes the FPV feed from the RC instead of UDT). `RC_DUMP=<file>` appends the raw RC→Orin bytes
//! to a file for offline analysis. `TLS_CERT_DIR` is where the self-signed
//! certificate lives (default `certs`; `TLS=off` disables the TLS listener).
//! Keys for the online services live in `secrets/*.key` (`SECRETS_DIR`;
//! or `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `DEEPGRAM_API_KEY`, see
//! [`online`]). `STT_PROVIDER` / `STT_MODEL` choose the speech service
//! ([`voice`]); `LLM_PROVIDER` / `LLM_MODEL` the model that interprets
//! commands ([`understand`]), with the local grammar as the fallback.

mod http;
mod online;
mod portal;
mod ports;
mod tls;
mod understand;
mod video;
mod voice;
mod ws;

use std::net::SocketAddrV4;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{JoinHandle, sleep};
use std::time::Duration;

use dji_common::logln;
use dji_transport::aoa::device::answer_setup;
use dji_transport::usb::ffs::{self, Event};
use dji_vehicle::links::{rc, serial};
use dji_vehicle::{Vehicle, motors, thermal};

use video::{Feed, Source};

const HTTP_ADDR: &str = "0.0.0.0:8080";
const HTTPS_ADDR: &str = "0.0.0.0:8443";

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

    let vehicle = Vehicle::new();

    let feeds = Arc::new(video_feeds());
    let rc_feeds: Vec<_> = feeds
        .values()
        .filter(|f| f.rc_channel().is_some())
        .cloned()
        .collect();

    // Web front end + WebSocket come up immediately (independent of the USB
    // link): plain HTTP, and HTTPS for the browser's microphone.
    let cx = Arc::new(http::Context {
        web_root,
        feeds: Arc::clone(&feeds),
        vehicle: Arc::clone(&vehicle),
    });
    {
        let cx = Arc::clone(&cx);
        std::thread::spawn(move || http::serve(HTTP_ADDR, None, cx));
    }
    if std::env::var("TLS").as_deref() != Ok("off") {
        let dir = PathBuf::from(std::env::var("TLS_CERT_DIR").unwrap_or_else(|_| "certs".into()));
        match tls::acceptor(&dir, &tls::local_hosts()) {
            Ok(acceptor) => {
                let cx = Arc::clone(&cx);
                std::thread::spawn(move || http::serve(HTTPS_ADDR, Some(acceptor), cx));
            }
            Err(e) => logln!("tls: {e}; no HTTPS listener (voice input needs it)"),
        }
    }

    // Thermal auto range: idle until the UI turns it on.
    {
        let vehicle = Arc::clone(&vehicle);
        std::thread::spawn(move || thermal::auto::run(&vehicle));
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
        let vehicle = Arc::clone(&vehicle);
        std::thread::spawn(move || motors::run(&vehicle, app_id));
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
        let vehicle = Arc::clone(&vehicle);
        let pick = move || Some(uart.clone().unwrap_or_else(ports::osdk_uart));
        std::thread::spawn(move || serial::run::<serial::Osdk>(pick, baud, &vehicle));
    }
    let acm = fixed("USB_ACM");
    if acm.as_deref() != Some("off") {
        // CDC-ACM ignores the line rate; any supported value works.
        let vehicle = Arc::clone(&vehicle);
        let pick = move || acm.clone().or_else(ports::usb_acm);
        std::thread::spawn(move || serial::run::<serial::Usb>(pick, 115_200, &vehicle));
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
    let mut writer_h: Option<JoinHandle<()>> = None;
    ffs::run_events(ep0.as_fd(), |event| match event {
        Event::Enable => {
            logln!("gadget: ENABLE");
            let open = |name| ffs::open_endpoint(&ffs_dir, name, 200);
            if needs_spawn(&reader_h) {
                match open("ep1") {
                    Ok(ep1) => {
                        let (vehicle, rc_feeds) = (Arc::clone(&vehicle), rc_feeds.clone());
                        reader_h = Some(std::thread::spawn(move || {
                            rc::read(ep1, &vehicle, |ch, payload| {
                                if let Some(f) =
                                    rc_feeds.iter().find(|f| f.rc_channel() == Some(ch))
                                {
                                    f.offer(payload);
                                }
                            })
                        }));
                        logln!("link up: telemetry reader running");
                    }
                    Err(e) => logln!("gadget: {e}"),
                }
            }
            if needs_spawn(&writer_h) {
                match open("ep2") {
                    Ok(ep2) => {
                        let vehicle = Arc::clone(&vehicle);
                        writer_h = Some(std::thread::spawn(move || rc::write(ep2, &vehicle)));
                        logln!("link up: RC writer running");
                    }
                    Err(e) => logln!("gadget: {e}"),
                }
            }
        }
        Event::Disable => {
            logln!("gadget: DISABLE");
            vehicle.telemetry.lock().unwrap().link_up = false;
        }
        Event::Setup(s) => {
            let _ = answer_setup(ep0.as_fd(), &s);
        }
        Event::Bind => logln!("gadget: BIND"),
        Event::Unbind => logln!("gadget: UNBIND"),
        Event::Other(_) => {}
    });
}
