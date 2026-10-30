//! server — Orin as the DJI *app* toward the Cendence RC, with a web UI.
//!
//! The Orin presents the FunctionFS accessory gadget (exactly as `ffs_capture`
//! does), so no DJI tablet is in the loop: the RC relays to the real aircraft.
//! The aircraft itself is a [`Vehicle`] from `dji-vehicle`: its RC link
//! reads telemetry and writes gimbal/thermal/motor frames on the bulk
//! endpoints, and its serial links read what the aircraft pushes on its own
//! ports. This binary is the presentation layer:
//!
//! * [`config`] — the settings (`config.toml`, with the environment as
//!   per-run overrides); the only place the environment is read;
//! * [`gadget`] — the gadget's ep0 event loop and the RC link workers;
//! * [`ports`] — the choice of serial ports for the direct readers;
//! * [`web`] — the HTTP(S) front end, its WebSocket and the portal's
//!   command messages, mapped onto the vehicle's typed operations;
//! * [`video`] — the cameras' H.264, decoded on the Orin's hardware decoder
//!   and streamed to the browser as MJPEG while someone watches;
//! * [`ai`] — the online services: speech recognition for the portal's
//!   microphone, and the model that turns a sentence into a plan.
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
//! Usage: `server <ffs_mount_dir>`        (run as root; `CONFIG=<file>`)

mod ai;
mod config;
mod gadget;
mod ports;
mod video;
mod web;

use std::sync::Arc;

use dji_common::logln;
use dji_protocol::gimbal::command::set_cmd_type;
use dji_vehicle::links::serial;
use dji_vehicle::{Vehicle, motors, thermal};

use config::Config;

fn die(msg: &str) -> ! {
    logln!("{msg}");
    std::process::exit(1);
}

fn main() {
    dji_common::log::init(None);
    let args: Vec<String> = std::env::args().collect();
    let Some(ffs_dir) = args.get(1).cloned() else {
        eprintln!("usage: {} <ffs_mount_dir>", args[0]);
        std::process::exit(1);
    };
    let cfg = Config::load().unwrap_or_else(|e| die(&format!("config: {e}")));

    set_cmd_type(cfg.gimbal.cmd_type);
    let vehicle = Vehicle::with_limits(cfg.limits.limits(), cfg.gimbal.plan_speed);
    ai::online::init(&cfg.secrets.dir);
    ai::voice::init(&cfg.speech);
    ai::understand::init(&cfg.understanding);

    let feeds = Arc::new(video::feeds(&cfg.video).unwrap_or_else(|e| die(&e)));
    let rc_feeds: Vec<_> = feeds
        .values()
        .filter(|f| f.rc_channel().is_some())
        .cloned()
        .collect();

    // Web front end + WebSocket come up immediately (independent of the USB
    // link): plain HTTP, and HTTPS for the browser's microphone.
    if !cfg.web.root.join("index.html").is_file() {
        logln!(
            "http: no index.html under {}; build the web UI first (cd web && npm ci && npm run build)",
            cfg.web.root.display()
        );
    }
    let local = web::tls::local_hosts();
    let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"].map(String::from).into();
    hosts.extend(local.iter().cloned());
    hosts.extend(cfg.web.hosts.iter().cloned());
    logln!("ws: accepting Host {hosts:?}");
    let cx = Arc::new(web::Context {
        web_root: cfg.web.root.clone(),
        feeds: Arc::clone(&feeds),
        vehicle: Arc::clone(&vehicle),
        hosts,
    });
    {
        let (cx, addr) = (Arc::clone(&cx), cfg.web.http.clone());
        std::thread::spawn(move || web::http::serve(&addr, None, cx));
    }
    if let Some(addr) = cfg.https() {
        match web::tls::acceptor(&cfg.web.cert_dir, &local) {
            Ok(acceptor) => {
                let (cx, addr) = (Arc::clone(&cx), addr.to_string());
                std::thread::spawn(move || web::http::serve(&addr, Some(acceptor), cx));
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
        let app_id = cfg.motors.osdk_app_id;
        if app_id.is_none() {
            logln!("motors: no osdk_app_id; motor commands go over the RC link only");
        }
        let vehicle = Arc::clone(&vehicle);
        std::thread::spawn(move || motors::run(&vehicle, app_id));
    }

    // Direct aircraft ports: read here, independent of the RC link. `auto`
    // lets [`ports`] choose on every open attempt; `off` disables a reader.
    let fixed = |choice: &str| -> Option<Option<String>> {
        match choice {
            "off" => None,
            "auto" | "" => Some(None),
            path => Some(Some(path.to_string())),
        }
    };
    if let Some(uart) = fixed(&cfg.ports.osdk_uart) {
        let (baud, vehicle) = (cfg.ports.osdk_baud, Arc::clone(&vehicle));
        let pick = move || Some(uart.clone().unwrap_or_else(ports::osdk_uart));
        std::thread::spawn(move || serial::run::<serial::Osdk>(pick, baud, &vehicle));
    }
    if let Some(acm) = fixed(&cfg.ports.usb_acm) {
        // CDC-ACM ignores the line rate; any supported value works.
        let vehicle = Arc::clone(&vehicle);
        let pick = move || acm.clone().or_else(ports::usb_acm);
        std::thread::spawn(move || serial::run::<serial::Usb>(pick, 115_200, &vehicle));
    }

    gadget::run(&ffs_dir, vehicle, rc_feeds, cfg.video.rc_dump.clone());
}
