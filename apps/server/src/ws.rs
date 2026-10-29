//! The WebSocket (`/ws` on the HTTP listeners): telemetry snapshots down,
//! UI commands up.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tungstenite::Message;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};

use dji_common::logln;
use dji_protocol::Telemetry;
use dji_vehicle::Vehicle;

use crate::http::Conn;
use crate::portal;
use crate::video::{self, Feeds, VideoStatus};

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
    /// Motor start/stop progress ([`dji_vehicle::motors`]).
    motors: dji_vehicle::motors::Status,
    /// The command plan being run, or last run / checked
    /// ([`dji_vehicle::executor`]).
    plan: dji_vehicle::executor::Status,
}

/// The host part of a `Host` header or an `Origin` URL, without the port:
/// `orin:8081` and `http://orin:8080/` both give `orin`.
fn host_of(value: &str) -> &str {
    let value = value.split_once("://").map_or(value, |(_, rest)| rest);
    let value = value.split('/').next().unwrap_or("");
    if let Some(end) = value.strip_prefix('[').and_then(|v| v.find(']')) {
        &value[..end + 2]
    } else {
        value.rsplit_once(':').map_or(value, |(host, _)| host)
    }
}

/// Whether a client may open the socket. Browsers always send `Origin`, and
/// WebSockets are exempt from the same-origin policy, so without this check
/// any web page open on a LAN machine could send commands (arm the motors).
/// A client that sends no `Origin` is not a browser page and is allowed.
fn origin_allowed(host: Option<&str>, origin: Option<&str>) -> bool {
    match (host, origin) {
        (_, None) => true,
        (Some(h), Some(o)) => host_of(h).eq_ignore_ascii_case(host_of(o)),
        (None, Some(_)) => false,
    }
}

// The signature is tungstenite's handshake callback; the error type is theirs.
#[allow(clippy::result_large_err)]
fn check_origin(req: &Request, resp: Response) -> Result<Response, ErrorResponse> {
    let header = |name| req.headers().get(name).and_then(|v| v.to_str().ok());
    if origin_allowed(header("host"), header("origin")) {
        return Ok(resp);
    }
    logln!(
        "ws: refused origin {:?} (host {:?})",
        header("origin").unwrap_or(""),
        header("host").unwrap_or("")
    );
    let mut refused = ErrorResponse::new(Some("origin not allowed".into()));
    *refused.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
    Err(refused)
}

/// One WebSocket client on an accepted connection (its request head still
/// unread, see [`crate::http::Replay`]): stream telemetry down, apply
/// commands up. On any exit (disconnect, error) release every gimbal — a
/// dead-man stop.
pub fn client<C: Conn>(stream: C, peer: String, vehicle: &Arc<Vehicle>, feeds: &Feeds) {
    // Handshake with a generous timeout; the short poll timeout is set only
    // afterwards, or a client slower than 50 ms to send its upgrade request
    // fails the handshake with WouldBlock and is dropped.
    if let Err(e) = stream
        .tcp()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .and_then(|()| stream.tcp().set_write_timeout(Some(Duration::from_secs(1))))
    {
        logln!("ws: cannot set timeouts for {peer}: {e}");
        return;
    }
    let mut ws = match tungstenite::accept_hdr(stream, check_origin) {
        Ok(ws) => ws,
        Err(e) => {
            logln!("ws: handshake with {peer} failed: {e}");
            return;
        }
    };
    let _ = ws
        .get_ref()
        .tcp()
        .set_read_timeout(Some(Duration::from_millis(50)));
    logln!("ws: client {peer} connected");
    let mut last_send = Instant::now() - Duration::from_secs(1);
    // Display snapshots need not run at the gimbal command injection rate.
    // Commands are still read on every iteration, without this throttle.
    let interval = Duration::from_millis(100);
    loop {
        if last_send.elapsed() >= interval {
            let (linked, thermal_auto, motors, plan) = (
                vehicle.gimbal.linked(),
                vehicle.thermal.auto(),
                vehicle.motors.status(),
                vehicle.executor.status(),
            );
            let video = video::statuses(feeds);
            let t = vehicle.snapshot();
            let json = {
                serde_json::to_string(&Snapshot {
                    tele: &t,
                    linked,
                    video,
                    thermal_auto,
                    motors,
                    plan,
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
                let thermal = vehicle.telemetry.lock().unwrap().thermal;
                portal::handle(t.as_str(), vehicle, thermal.as_ref());
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {} // ping/pong/binary: ignore
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    vehicle.gimbal.release_all();
    logln!("ws: client {peer} gone; gimbals released");
}

#[cfg(test)]
mod tests {
    use super::{host_of, origin_allowed};

    #[test]
    fn origin_must_match_the_host_the_page_came_from() {
        assert_eq!(host_of("orin:8081"), "orin");
        assert_eq!(host_of("http://orin:8080/"), "orin");
        assert_eq!(host_of("http://[fe80::1]:8080"), "[fe80::1]");
        assert_eq!(host_of("192.168.1.5"), "192.168.1.5");
        assert!(origin_allowed(Some("orin:8081"), Some("http://orin:8080")));
        assert!(origin_allowed(Some("ORIN:8081"), Some("http://orin:8080")));
        assert!(origin_allowed(Some("orin:8081"), None)); // not a browser page
        assert!(!origin_allowed(
            Some("orin:8081"),
            Some("http://evil.example")
        ));
        assert!(!origin_allowed(
            Some("orin:8081"),
            Some("http://orin.example:8080")
        ));
        assert!(!origin_allowed(None, Some("http://orin:8080")));
    }
}
