//! WebSocket endpoint: telemetry snapshots down, UI commands up.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tungstenite::Message;

use dji_common::logln;
use dji_protocol::Telemetry;

use crate::reader::Tele;
use crate::state::{self, Shared};
use crate::video::{self, Feeds, VideoStatus};
use crate::{WS_ADDR, die};

/// What the server broadcasts to the UI: the telemetry snapshot plus the current
/// link state, so a freshly-loaded or refreshed page reflects the real state.
#[derive(Serialize)]
struct Snapshot<'a> {
    #[serde(flatten)]
    tele: &'a Telemetry,
    linked: bool,
    /// State of each camera video feed, by feed name.
    video: BTreeMap<&'static str, VideoStatus>,
    /// Whether the thermal auto range is on (server-authoritative).
    thermal_auto: bool,
    /// Motor start/stop progress ([`crate::motors`]).
    motors: crate::motors::Status,
}

/// Accept WebSocket clients forever, one thread each.
pub fn serve(cmds: Shared, tele: Tele, feeds: Arc<Feeds>) {
    let listener =
        TcpListener::bind(WS_ADDR).unwrap_or_else(|e| die(&format!("ws: bind {WS_ADDR}: {e}")));
    logln!("ws: listening on {WS_ADDR}");
    for stream in listener.incoming().flatten() {
        let cmds = Arc::clone(&cmds);
        let tele = Arc::clone(&tele);
        let feeds = Arc::clone(&feeds);
        std::thread::spawn(move || client(stream, cmds, tele, feeds));
    }
}

/// One WebSocket client: stream telemetry down, apply commands up. On any exit
/// (disconnect, error) release every gimbal — a dead-man stop.
fn client(stream: TcpStream, cmds: Shared, tele: Tele, feeds: Arc<Feeds>) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    // Handshake with a generous timeout; the short poll timeout is set only
    // afterwards, or a client slower than 50 ms to send its upgrade request
    // fails the handshake with WouldBlock and is dropped.
    if let Err(e) = stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(1))))
    {
        logln!("ws: cannot set timeouts for {peer}: {e}");
        return;
    }
    let mut ws = match tungstenite::accept(stream) {
        Ok(ws) => ws,
        Err(e) => {
            logln!("ws: handshake with {peer} failed: {e}");
            return;
        }
    };
    let _ = ws
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(50)));
    logln!("ws: client {peer} connected");
    let mut last_send = Instant::now() - Duration::from_secs(1);
    // Display snapshots need not run at the gimbal command injection rate.
    // Commands are still read on every iteration, without this throttle.
    let interval = Duration::from_millis(100);
    loop {
        if last_send.elapsed() >= interval {
            let (linked, thermal_auto, motors) = {
                let c = cmds.lock().unwrap();
                (c.linked, c.thermal_auto, c.motors.clone())
            };
            let video = video::statuses(&feeds);
            let t = tele.lock().unwrap().clone();
            let json = {
                serde_json::to_string(&Snapshot {
                    tele: &t,
                    linked,
                    video,
                    thermal_auto,
                    motors,
                })
                .unwrap_or_else(|_| "{}".into())
            };
            if ws.send(Message::text(json)).is_err() {
                break;
            }
            last_send = Instant::now();
        }
        match ws.read() {
            Ok(Message::Text(t)) => {
                let thermal = tele.lock().unwrap().thermal;
                state::handle(t.as_str(), &cmds, thermal.as_ref());
            }
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
