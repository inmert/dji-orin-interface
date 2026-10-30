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

use crate::video::{self, Feeds, VideoStatus};
use crate::web::http::Conn;
use crate::web::portal;

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
/// unread, see [`super::http::Replay`]): stream telemetry down, apply
/// commands up. On any exit (disconnect, error) release every gimbal — a
/// dead-man stop.
pub fn client<C: Conn>(stream: C, peer: String, vehicle: &Arc<Vehicle>, feeds: &Feeds) {
    // Handshake with a generous timeout; the short poll timeout is set only
    // afterwards, or a client slower than 50 ms to send its upgrade request
    // fails the handshake with WouldBlock and is dropped.
    if let Err(e) = stream
        .tcp()
        .set_read_timeout(Some(Duration::from_secs(5)))
        // Snapshots go out at 10 Hz; a client whose link cannot take one
        // within this long is dropped (and reconnects), so keep it generous
        // enough that a congested Wi-Fi moment does not cost the connection.
        .and_then(|()| stream.tcp().set_write_timeout(Some(Duration::from_secs(5))))
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
    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;

    use tungstenite::client::IntoClientRequest;

    use super::*;

    /// A server-side [`client`] on one end of a loopback connection, and a
    /// WebSocket handshake from the other with `origin`.
    fn connect(
        vehicle: &Arc<Vehicle>,
        origin: &str,
    ) -> (
        Result<tungstenite::WebSocket<TcpStream>, String>,
        JoinHandle<()>,
    ) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = {
            let vehicle = Arc::clone(vehicle);
            std::thread::spawn(move || {
                let (s, peer) = l.accept().unwrap();
                client(s, peer.to_string(), &vehicle, &Feeds::new());
            })
        };
        let mut req = format!("ws://orin:{}/ws", addr.port())
            .into_client_request()
            .unwrap();
        req.headers_mut().insert("Origin", origin.parse().unwrap());
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let ws = tungstenite::client::client(req, tcp)
            .map(|(ws, _)| ws)
            .map_err(|e| e.to_string());
        (ws, server)
    }

    /// A snapshot with every section filled, as the portal sees it in flight.
    fn full_snapshot() -> serde_json::Value {
        use dji_commands::{Command, Horizontal, Plan, Vertical};
        use dji_protocol::duml::{self, Device, Header};
        use dji_protocol::osdk::broadcast::*;
        use dji_protocol::thermal::{AreaStats, Report};
        use dji_protocol::{flyc, gimbal};

        let mut t = Telemetry::default();
        let frame = |sender, cmd_set, cmd_id, payload: &[u8]| {
            let h = Header {
                sender,
                receiver: duml::addr(Device::App, 0),
                seq: 1,
                cmd_type: 0,
                cmd_set,
                cmd_id,
            };
            duml::build(&h, payload).unwrap()
        };
        for index in [0, 2] {
            let f = frame(
                duml::addr(Device::Gimbal, index),
                gimbal::CMD_SET,
                gimbal::CMD_ID_ATTITUDE,
                &[0x0a, 0, 0, 0, 0xec, 0xff],
            );
            t.update(&duml::parse(&f).unwrap().0);
        }
        let rpm = frame(
            duml::addr(Device::FlightCtrl, 0),
            flyc::CMD_SET,
            flyc::CMD_ID_POWER,
            &1536.25f32.to_le_bytes(),
        );
        t.update(&duml::parse(&rpm).unwrap().0);
        t.flight.port = "/dev/ttyUSB0".into();
        t.flight.open = true;
        t.flight.frames = 5;
        t.flight.rejected = 1;
        t.flight.data = FlightData {
            time_ms: Some(532_800),
            attitude: Some(Euler {
                roll: 1.5,
                pitch: -2.0,
                yaw: 90.0,
            }),
            quaternion: Some([1.0, 0.0, 0.0, 0.0]),
            acceleration: Some([0.0, 0.0, 9.75]),
            velocity: Some(Velocity {
                x: 1.0,
                y: 2.0,
                z: -0.5,
                gps: true,
            }),
            angular_rate: Some([0.0, 0.25, 0.0]),
            position: Some(Position {
                latitude: 45.5,
                longitude: -73.5,
                altitude: 167.5,
                height: 2.5,
                health: 4,
            }),
            obstacles: Some(Obstacles {
                down: 0.25,
                front: 100.0,
                right: 100.0,
                back: 3.5,
                left: 100.0,
                up: 1.75,
                health: 0x3f,
            }),
            gps: Some(Gps {
                date: 20_261_030,
                time: 224_425,
                latitude: 45.5,
                longitude: -73.5,
                hfsl: 270.5,
                velocity_ned: [1.0, -0.5, 0.0],
                hdop: 1.25,
                pdop: 2.25,
                fix: 3.0,
                satellites: 12,
            }),
            mag: Some([101, 582, 1366]),
            rc: Some(Rc {
                roll: 0,
                pitch: 10_000,
                yaw: 0,
                throttle: -10_000,
                mode: 8000,
                gear: 0,
            }),
            gimbal: Some(Gimbal {
                roll: 0.0,
                pitch: -30.0,
                yaw: -97.5,
            }),
            status: Some(Status {
                flight: 1,
                flight_name: flight_name(1),
                mode: 6,
                mode_name: mode_name(6),
                gear: 1,
                gear_name: gear_name(1),
                error: 0,
            }),
            battery: Some(Battery {
                capacity: 4000,
                voltage: 24_500,
                current: -1200,
                percent: 87,
            }),
            compass: Some(0),
            last_flags: 0x7FFF,
        };
        t.usb.port = "/dev/ttyACM0".into();
        t.usb.open = true;
        t.usb.frames = 3;
        t.usb.osd = Some(flyc::Osd {
            pitch: 1.5,
            roll: 2.5,
            yaw: 143.0,
            mode: 6,
            mode_name: mode_name(6),
            satellites: 0,
            unverified: flyc::osd::Unverified::default(),
        });
        t.usb.gimbals = t.gimbals.clone();
        t.thermal = Some(Report {
            spot_x: 0.5,
            spot_y: 0.5,
            spot_temp: 36.5,
            spot_valid: true,
            display_mode: Some(3),
            measure: 2,
            isotherm_enabled: true,
            isotherm_lower: 27,
            isotherm_middle: 32,
            isotherm_upper: 38,
            area: Some(AreaStats {
                rect: [0.0, 0.0, 1.0, 1.0],
                avg: 21.5,
                min: 18.25,
                max: 36.5,
                min_x: 0.25,
                min_y: 0.75,
                max_x: 0.5,
                max_y: 0.5,
            }),
        });
        // Every kind of step the registry knows.
        let plan = Plan::new(vec![
            Command::Arm,
            Command::Wait { seconds: 2.5 },
            Command::Pan {
                camera: 2,
                direction: Horizontal::Left,
                seconds: 1.0,
            },
            Command::Tilt {
                camera: 1,
                direction: Vertical::Up,
                seconds: 0.5,
            },
            Command::Recenter { camera: None },
            Command::Recenter { camera: Some(2) },
            Command::Link { on: true },
            Command::Disarm,
        ]);
        let snapshot = Snapshot {
            tele: &t,
            linked: true,
            video: BTreeMap::from([(
                "cam1",
                video::Feed::new("cam1", video::Source::Rc(0x11), Some((640, 360))).status(),
            )]),
            thermal_auto: false,
            motors: dji_vehicle::motors::Status {
                busy: false,
                last: Some("start"),
                ok: Some(true),
                message: "motors started over the RC link".into(),
                armed: Some(true),
                armed_from: Some("RC link"),
                usb_flight: "no_usb",
            },
            plan: dji_vehicle::executor::Status {
                running: true,
                plan: Some(plan.clone()),
                text: "the sentence".into(),
                step: 2,
                message: "step 3: pan camera 2 Left 1 s".into(),
                ok: None,
                preview: Some(plan),
                preview_error: None,
                parsing: false,
                source: "grammar".into(),
            },
        };
        serde_json::to_value(&snapshot).unwrap()
    }

    /// `web/tests/snapshot.json` is what the server sends, so the web
    /// validators are tested against the real shape (`npm test`). After a
    /// deliberate change to the snapshot, regenerate it with
    /// `UPDATE_FIXTURES=1 cargo test -p dji-server snapshot`.
    #[test]
    fn the_web_fixture_is_the_snapshot_the_server_sends() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/tests/snapshot.json");
        let snapshot = full_snapshot();
        if std::env::var_os("UPDATE_FIXTURES").is_some() {
            let text = serde_json::to_string_pretty(&snapshot).unwrap() + "\n";
            std::fs::write(path, text).unwrap();
        }
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            fixture, snapshot,
            "the snapshot changed: regenerate web/tests/snapshot.json (see this test's doc)"
        );
    }

    #[test]
    fn a_foreign_origin_is_refused_at_the_handshake() {
        let v = Vehicle::new();
        let (ws, server) = connect(&v, "http://evil.example");
        assert!(ws.unwrap_err().contains("403"));
        server.join().unwrap();
    }

    #[test]
    fn snapshots_flow_garbage_is_ignored_and_leaving_releases_the_gimbals() {
        let v = Vehicle::new();
        let (ws, server) = connect(&v, "http://orin:8080");
        let mut ws = ws.unwrap();
        let first = ws.read().unwrap().into_text().unwrap();
        let snap: serde_json::Value = serde_json::from_str(first.as_str()).unwrap();
        for key in [
            "link_up",
            "frames",
            "linked",
            "video",
            "motors",
            "plan",
            "thermal_auto",
        ] {
            assert!(snap.get(key).is_some(), "snapshot without {key}");
        }
        for garbage in [
            "not json",
            "{}",
            r#"{"cmd":"gimbal","camera":1,"pitch":99999,"active":true}"#,
            r#"{"cmd":"gimbal","camera":"1","active":true}"#,
            r#"{"cmd":"motors","on":"yes"}"#,
            r#"{"cmd":"launch"}"#,
        ] {
            ws.send(Message::text(garbage)).unwrap();
        }
        ws.send(Message::binary(vec![0xff; 64])).unwrap();
        ws.send(Message::text(
            r#"{"cmd":"gimbal","camera":1,"pitch":5,"yaw":0,"active":true}"#,
        ))
        .unwrap();
        // The connection survives all of it and keeps sending snapshots.
        for _ in 0..3 {
            assert!(ws.read().unwrap().is_text());
        }
        assert!(v.gimbal.lock().cam1.active, "the valid command was applied");
        assert_eq!(v.motors.lock().pending, None);
        ws.close(None).unwrap();
        while ws.read().is_ok() {}
        server.join().unwrap();
        assert!(
            !v.gimbal.lock().cam1.active,
            "released when the client left"
        );
        assert!(!v.gimbal.lock().cam2.active);
    }

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
