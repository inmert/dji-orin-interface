//! server — Orin as the DJI *app* toward the Cendence RC, with a web UI.
//!
//! The Orin presents the FunctionFS accessory gadget (exactly as `ffs_capture`
//! does), so no DJI tablet is in the loop: the RC relays to the real aircraft.
//! From the bulk streams it decodes [`telemetry`] and injects gimbal move
//! commands ([`control`]). A web front end (served static) talks to it over a
//! WebSocket: telemetry down, gimbal commands up.
//!
//! ```text
//!   browser  ──HTTP :8080 / WS :8081──  server (Orin, gadget)  ──USB──  RC ── aircraft
//! ```
//!
//! SAFETY: the gimbal commands move the real camera mount via the powered
//! aircraft. Nothing is injected until the UI sends a command; a WS disconnect
//! or the UI STOP releases every gimbal. Run props-off until calibrated.
//!
//! Usage: server <ffs_mount_dir> [web_root]        (run as root)

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tungstenite::Message;

use dji_orin_interface::control::{Camera, GimbalMove, gimbal_move_frame};
use dji_orin_interface::telemetry::Telemetry;
use dji_orin_interface::usb::ffs::{self, Event, Setup};
use dji_orin_interface::usb::sys;
use dji_orin_interface::{aoa, duml, logln};

const HTTP_ADDR: &str = "0.0.0.0:8080";
const WS_ADDR: &str = "0.0.0.0:8081";
const INJECT_HZ: u64 = 10;

/// Shared desired gimbal state, one slot per camera, updated by the WS clients
/// and streamed to ep2 by the injector thread.
#[derive(Default)]
struct Commands {
    cam1: GimbalMove,
    cam2: GimbalMove,
}

impl Commands {
    fn set(&mut self, cam: Camera, mv: GimbalMove) {
        match cam {
            Camera::One => self.cam1 = mv,
            Camera::Two => self.cam2 = mv,
        }
    }
    fn release_all(&mut self) {
        self.cam1.active = false;
        self.cam2.active = false;
    }
}

/// A command message from the UI.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
enum WsCommand {
    /// Set one camera's gimbal move (pitch/yaw, active while held).
    Gimbal {
        camera: u8,
        #[serde(default)]
        pitch: i16,
        #[serde(default)]
        yaw: i16,
        #[serde(default)]
        active: bool,
    },
    /// Release every gimbal immediately.
    Stop,
}

type Shared = Arc<Mutex<Commands>>;
type Tele = Arc<Mutex<Telemetry>>;

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

/// Answer one ep0 SETUP packet (the AOA handshake the RC drives against us).
fn handle_setup(ep0: &OwnedFd, s: Setup) {
    let fd = ep0.as_fd();
    if s.is_vendor() && s.is_in() && s.request == aoa::GET_PROTOCOL {
        let ver = aoa::PROTOCOL_VERSION.to_le_bytes();
        let n = usize::from(s.length).min(ver.len());
        let _ = sys::write(fd, &ver[..n]);
        return;
    }
    if s.is_vendor() && !s.is_in() && s.request == aoa::START {
        let _ = sys::read(fd, &mut []);
        return;
    }
    if !s.is_in() && s.length > 0 {
        let mut data = vec![0u8; usize::from(s.length).min(4096)];
        let _ = sys::read(fd, &mut data);
        return;
    }
    if !s.is_in() {
        let _ = sys::read(fd, &mut []);
        return;
    }
    let _ = ffs::stall(fd, true);
}

fn open_ep(ffs_dir: &str, name: &str) -> OwnedFd {
    for _ in 0..200 {
        if let Ok(f) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("{ffs_dir}/{name}"))
        {
            return f.into();
        }
        sleep(Duration::from_millis(20));
    }
    die(&format!("gadget: could not open {name}"));
}

/// Read ep1 (OUT, RC → Orin): reassemble DUML and fold into the telemetry.
fn telemetry_reader(ep1: OwnedFd, tele: Tele) {
    let mut decoder = duml::Decoder::new();
    let mut buf = vec![0u8; 16384];
    loop {
        match sys::read(ep1.as_fd(), &mut buf) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
                decoder.push(&buf[..n]);
                let mut t = tele.lock().unwrap();
                decoder.drain(|f| {
                    if f.crc_ok {
                        t.update(f);
                    }
                });
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == sys::errno::ENODEV || code == sys::errno::ESHUTDOWN {
                    logln!("telemetry: ep1 gone; reader stopping");
                    return;
                }
                sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Stream gimbal commands to ep2 (IN, Orin → RC) at a fixed rate. Sends a move
/// frame per active camera each tick, and a single release frame when a camera
/// transitions from active to released.
fn injector(ep2: OwnedFd, cmds: Shared) {
    let period = Duration::from_millis(1000 / INJECT_HZ);
    let mut seq: u16 = 0;
    let mut was_active = [false; 2];
    loop {
        let (c1, c2) = {
            let c = cmds.lock().unwrap();
            (c.cam1, c.cam2)
        };
        for (slot, (cam, mv)) in [(Camera::One, c1), (Camera::Two, c2)]
            .into_iter()
            .enumerate()
        {
            let send = if mv.active {
                Some(mv)
            } else if was_active[slot] {
                Some(GimbalMove {
                    active: false,
                    ..Default::default()
                })
            } else {
                None
            };
            if let Some(m) = send {
                let frame = gimbal_move_frame(cam, m, seq);
                seq = seq.wrapping_add(1);
                if let Err(e) = sys::write(ep2.as_fd(), &frame) {
                    let code = e.raw_os_error().unwrap_or(0);
                    if code == sys::errno::ENODEV || code == sys::errno::ESHUTDOWN {
                        logln!("injector: ep2 gone; stopping");
                        return;
                    }
                }
            }
            was_active[slot] = mv.active;
        }
        sleep(period);
    }
}

fn handle_ws_command(text: &str, cmds: &Shared) {
    match serde_json::from_str::<WsCommand>(text) {
        Ok(WsCommand::Gimbal {
            camera,
            pitch,
            yaw,
            active,
        }) => {
            if let Some(cam) = Camera::from_number(camera) {
                cmds.lock()
                    .unwrap()
                    .set(cam, GimbalMove { pitch, yaw, active });
            }
        }
        Ok(WsCommand::Stop) => cmds.lock().unwrap().release_all(),
        Err(e) => logln!("ws: bad command: {e}"),
    }
}

/// One WebSocket client: stream telemetry down, apply commands up. On any exit
/// (disconnect, error) release every gimbal — a dead-man stop.
fn ws_client(stream: TcpStream, cmds: Shared, tele: Tele) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut ws = match tungstenite::accept(stream) {
        Ok(ws) => ws,
        Err(_) => return,
    };
    logln!("ws: client {peer} connected");
    let mut last_send = Instant::now() - Duration::from_secs(1);
    let interval = Duration::from_millis(1000 / INJECT_HZ);
    loop {
        if last_send.elapsed() >= interval {
            let json = {
                let t = tele.lock().unwrap();
                serde_json::to_string(&*t).unwrap_or_else(|_| "{}".into())
            };
            if ws.send(Message::text(json)).is_err() {
                break;
            }
            last_send = Instant::now();
        }
        match ws.read() {
            Ok(Message::Text(t)) => handle_ws_command(t.as_str(), &cmds),
            Ok(Message::Close(_)) => break,
            Ok(_) => {} // ping/pong/binary: ignore
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    cmds.lock().unwrap().release_all();
    logln!("ws: client {peer} gone; gimbals released");
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// Serve the built front end. Maps `/` to `index.html` and rejects any path
/// that escapes `web_root` (no `..` traversal).
fn http_server(web_root: PathBuf) {
    let server = match tiny_http::Server::http(HTTP_ADDR) {
        Ok(s) => s,
        Err(e) => die(&format!("http: bind {HTTP_ADDR}: {e}")),
    };
    logln!("http: serving {} on {HTTP_ADDR}", web_root.display());
    for request in server.incoming_requests() {
        let rel = request
            .url()
            .split('?')
            .next()
            .unwrap_or("/")
            .trim_start_matches('/');
        let rel = if rel.is_empty() { "index.html" } else { rel };
        let safe = !rel.split('/').any(|seg| seg == ".." || seg == ".");
        let path = web_root.join(rel);
        let resp = match (safe, std::fs::read(&path)) {
            (true, Ok(bytes)) => {
                let ct = content_type(&path);
                let header =
                    tiny_http::Header::from_bytes(b"Content-Type".as_slice(), ct.as_bytes())
                        .expect("valid header");
                tiny_http::Response::from_data(bytes).with_header(header)
            }
            _ => tiny_http::Response::from_string("not found").with_status_code(404),
        };
        let _ = request.respond(resp);
    }
}

fn main() {
    dji_orin_interface::log::init(None);
    let args: Vec<String> = std::env::args().collect();
    let Some(ffs_dir) = args.get(1).cloned() else {
        eprintln!("usage: {} <ffs_mount_dir> [web_root]", args[0]);
        std::process::exit(1);
    };
    let web_root = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "web/dist".into()));

    let cmds: Shared = Arc::new(Mutex::new(Commands::default()));
    let tele: Tele = Arc::new(Mutex::new(Telemetry::default()));

    // Web front end + WebSocket come up immediately (independent of the USB link).
    {
        let web_root = web_root.clone();
        std::thread::spawn(move || http_server(web_root));
    }
    {
        let cmds = Arc::clone(&cmds);
        let tele = Arc::clone(&tele);
        std::thread::spawn(move || {
            let listener = match TcpListener::bind(WS_ADDR) {
                Ok(l) => l,
                Err(e) => die(&format!("ws: bind {WS_ADDR}: {e}")),
            };
            logln!("ws: listening on {WS_ADDR}");
            for stream in listener.incoming().flatten() {
                let cmds = Arc::clone(&cmds);
                let tele = Arc::clone(&tele);
                std::thread::spawn(move || ws_client(stream, cmds, tele));
            }
        });
    }

    // USB gadget side: descriptors, then the ep0 event loop. run_server.sh binds
    // the UDC after we log "gadget ready". If the gadget isn't present (e.g. run
    // without run_server.sh), keep serving the web UI instead of exiting.
    let ep0: OwnedFd = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("{ffs_dir}/ep0"))
    {
        Ok(f) => f.into(),
        Err(e) => {
            logln!("gadget: cannot open {ffs_dir}/ep0 ({e}); serving web UI only, no USB link");
            park();
        }
    };
    if let Err(e) =
        ffs::write_descriptors(ep0.as_fd()).and_then(|()| ffs::write_strings(ep0.as_fd()))
    {
        logln!("gadget: write descriptors/strings failed ({e}); serving web UI only");
        park();
    }
    logln!("gadget ready; waiting for the RC to enable it");

    let mut started = false;
    let mut last_errno = 0;
    let mut ev = [0u8; ffs::EVENT_SIZE * 8];
    loop {
        match sys::read(ep0.as_fd(), &mut ev) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
                last_errno = 0;
                for chunk in ev[..n].chunks_exact(ffs::EVENT_SIZE) {
                    let record: [u8; ffs::EVENT_SIZE] = chunk.try_into().expect("chunk size");
                    match Event::parse(&record) {
                        Event::Enable => {
                            logln!("gadget: ENABLE");
                            if !started {
                                started = true;
                                let ep1 = open_ep(&ffs_dir, "ep1");
                                let ep2 = open_ep(&ffs_dir, "ep2");
                                logln!("link up: telemetry + injector running");
                                let tele = Arc::clone(&tele);
                                let cmds = Arc::clone(&cmds);
                                std::thread::spawn(move || telemetry_reader(ep1, tele));
                                std::thread::spawn(move || injector(ep2, cmds));
                            }
                        }
                        Event::Disable => logln!("gadget: DISABLE"),
                        Event::Setup(s) => handle_setup(&ep0, s),
                        Event::Bind => logln!("gadget: BIND"),
                        Event::Unbind => logln!("gadget: UNBIND"),
                        Event::Other(_) => {}
                    }
                }
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                }
                if code != sys::errno::ESHUTDOWN && code != sys::errno::ENODEV && code != last_errno
                {
                    logln!("gadget: ep0 read error: {e}");
                }
                last_errno = code;
                sleep(Duration::from_millis(50));
            }
        }
    }
}
