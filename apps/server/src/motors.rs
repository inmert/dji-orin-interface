//! Motor requests from the UI, over the OSDK UART (OpenProtocol, see
//! [`dji_protocol::osdk::command`]).
//!
//! Each request runs the whole sequence the Onboard SDK uses: get the FC
//! version, activate the app (`OSDK_APP_ID`), obtain control authority, act,
//! then release authority again so the RC keeps control. The act is one of:
//!
//! * start or stop all motors;
//! * allow arming while the USB API port is connected
//!   ([`flyc::usb_flight_frame`] on that port; the FC takes it only under
//!   authority and forgets it on restart).
//!
//! ACKs arrive on the readers' receive-only ports ([`crate::flight`]), which
//! hand them here; requests go out on separate write-only handles opened per
//! request.
//!
//! **This spins the real motors.** It only ever runs on an explicit UI
//! request; props off unless flying.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_common::logln;
use dji_protocol::osdk::{self, command};
use dji_protocol::{duml, flyc};
use dji_transport::serial::TxPort;

use crate::reader::Tele;
use crate::state::Shared;

/// How long to wait for each ACK, and how often to send a request.
const ACK_TIMEOUT: Duration = Duration::from_millis(1000);
const ATTEMPTS: usize = 3;

/// A request from the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Start,
    Stop,
    /// Allow arming with the USB API port connected (until the next restart).
    UsbFlight,
}

impl Request {
    fn name(self) -> &'static str {
        match self {
            Request::Start => "start",
            Request::Stop => "stop",
            Request::UsbFlight => "usb_flight",
        }
    }
}

/// What the UI shows about motor commands.
#[derive(Clone, Default, Serialize)]
pub struct Status {
    /// A request is being carried out.
    pub busy: bool,
    /// The last request: `start`, `stop` or `usb_flight`.
    pub last: Option<&'static str>,
    /// Whether the last request succeeded.
    pub ok: Option<bool>,
    /// What happened, for the UI.
    pub message: String,
}

/// ACKs from the readers, keyed by (session or DUML command id, sequence).
/// OpenProtocol ACKs use their session (≤ 31); DUML ACKs `0x100 + cmd_id`.
static ACKS: Mutex<VecDeque<(u16, u16, Vec<u8>)>> = Mutex::new(VecDeque::new());
static ACK_READY: Condvar = Condvar::new();

fn push_ack(key: u16, seq: u16, data: &[u8]) {
    let mut q = ACKS.lock().unwrap();
    q.push_back((key, seq, data.to_vec()));
    while q.len() > 32 {
        q.pop_front();
    }
    ACK_READY.notify_all();
}

/// Hand an OpenProtocol ACK from the SDK UART reader to a waiting request.
pub fn deliver_ack(f: &osdk::Frame) {
    if f.ack {
        push_ack(u16::from(f.session), f.seq, f.payload);
    }
}

/// Hand the FC's DUML ACK of [`flyc::usb_flight_frame`] (USB port reader) to
/// a waiting request.
pub fn deliver_duml_ack(f: &duml::Frame) {
    if f.crc_ok
        && f.cmd_type & 0x80 != 0
        && (f.cmd_set, f.cmd_id) == (flyc::CMD_SET, flyc::CMD_ID_USB_FLIGHT)
    {
        push_ack(DUML_KEY + u16::from(f.cmd_id), f.seq, f.payload);
    }
}

const DUML_KEY: u16 = 0x100;

/// Carry out motor requests from the UI, one at a time, forever.
pub fn run(cmds: Shared, tele: Tele, app_id: Option<u32>) {
    loop {
        sleep(Duration::from_millis(50));
        let Some(req) = cmds.lock().unwrap().motors_pending.take() else {
            continue;
        };
        let what = req.name();
        logln!("motors: {what} requested");
        set_status(
            &cmds,
            Status {
                busy: true,
                last: Some(what),
                ok: None,
                message: match req {
                    Request::Start => "starting motors…",
                    Request::Stop => "stopping motors…",
                    Request::UsbFlight => "allowing motors with USB connected…",
                }
                .into(),
            },
        );
        let result = execute(req, &tele, app_id);
        match &result {
            Ok(m) => logln!("motors: {m}"),
            Err(e) => logln!("motors: {what} failed: {e}"),
        }
        set_status(
            &cmds,
            Status {
                busy: false,
                last: Some(what),
                ok: Some(result.is_ok()),
                message: result.unwrap_or_else(|e| e),
            },
        );
    }
}

fn set_status(cmds: &Shared, s: Status) {
    cmds.lock().unwrap().motors = s;
}

fn execute(req: Request, tele: &Tele, app_id: Option<u32>) -> Result<String, String> {
    let app_id = app_id.ok_or("OSDK_APP_ID is not set on the server")?;
    let (port, usb_port) = {
        let t = tele.lock().unwrap();
        if !t.flight.open {
            return Err("the SDK UART is not open".into());
        }
        (
            t.flight.port.clone(),
            t.usb.open.then(|| t.usb.port.clone()),
        )
    };
    // Check the USB port before taking authority for nothing.
    let usb = match req {
        Request::UsbFlight => {
            let p = usb_port.ok_or("the aircraft's USB port is not open")?;
            Some((
                TxPort::open(&p).map_err(|e| format!("cannot open {p} for writing: {e}"))?,
                p,
            ))
        }
        _ => None,
    };
    let tx = TxPort::open(&port).map_err(|e| format!("cannot open {port} for writing: {e}"))?;
    let mut link = Link {
        tx,
        seq: initial_seq(),
    };

    let version = link.request("get version", command::get_version)?;
    let fw = command::fw_version(&version).ok_or("unreadable FC version")?;
    let code = link.code("activate", |s| command::activate(s, app_id, fw))?;
    if code != 0 {
        return Err(format!("activation: {}", command::activation_message(code)));
    }

    let mut code = command::OBTAIN_CONTROL_IN_PROGRESS;
    for _ in 0..5 {
        code = link.code("obtain control", |s| command::set_control(s, true))?;
        if code != command::OBTAIN_CONTROL_IN_PROGRESS {
            break;
        }
        sleep(Duration::from_millis(500));
    }
    if code != command::OBTAIN_CONTROL_SUCCESS {
        return Err(format!(
            "control authority: {}",
            command::control_message(code)
        ));
    }

    // From here on, always hand authority back, whatever the act did.
    let acted = match (req, usb) {
        (Request::UsbFlight, Some((tx, p))) => usb_flight(tx, &p, link.seq),
        (_, _) => {
            let on = req == Request::Start;
            link.request("motors", |s| command::motors(s, on))
                .and_then(|task| {
                    let code = command::ack_code(&task).ok_or("empty motor ACK")?;
                    if code != 0 {
                        return Err(format!(
                            "FC refused ({}; ACK {})",
                            command::task_message(code),
                            hex(&task)
                        ));
                    }
                    Ok(format!("motors {}", if on { "started" } else { "stopped" }))
                })
        }
    };
    let mut release = link.code("release control", |s| command::set_control(s, false));
    for _ in 0..5 {
        if release != Ok(command::RELEASE_CONTROL_IN_PROGRESS) {
            break;
        }
        sleep(Duration::from_millis(500));
        release = link.code("release control", |s| command::set_control(s, false));
    }
    let released = match release {
        Ok(command::RELEASE_CONTROL_SUCCESS) => "control released".to_string(),
        Ok(c) => format!("release: {}", command::control_message(c)),
        Err(e) => format!("release: {e}"),
    };
    acted
        .map(|m| format!("{m}; {released}"))
        .map_err(|e| format!("{e}; {released}"))
}

/// Send [`flyc::usb_flight_frame`] on the USB API port and wait for the FC's
/// DUML ACK (`00` = accepted).
fn usb_flight(mut tx: TxPort, port: &str, seq_base: u16) -> Result<String, String> {
    for i in 0..ATTEMPTS as u16 {
        let seq = seq_base.wrapping_add(0x4000 + i);
        tx.send(&flyc::usb_flight_frame(seq, true))
            .map_err(|e| format!("USB flight: write to {port} failed: {e}"))?;
        if let Some(data) = wait_ack(DUML_KEY + u16::from(flyc::CMD_ID_USB_FLIGHT), seq) {
            return match data.first() {
                Some(0) => Ok("motors allowed with USB connected (until restart)".into()),
                _ => Err(format!("USB flight: FC refused (ACK {})", hex(&data))),
            };
        }
    }
    Err("USB flight: no ACK from the FC".into())
}

/// Start each run's sequence numbers somewhere new, so a late ACK from an
/// earlier run can't match.
fn initial_seq() -> u16 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos >> 8) as u16
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

struct Link {
    tx: TxPort,
    seq: u16,
}

impl Link {
    /// Send a request (rebuilt with a fresh sequence number on each attempt)
    /// and return its ACK data.
    fn request(&mut self, what: &str, build: impl Fn(u16) -> Vec<u8>) -> Result<Vec<u8>, String> {
        for _ in 0..ATTEMPTS {
            self.seq = self.seq.wrapping_add(1);
            let seq = self.seq;
            self.tx
                .send(&build(seq))
                .map_err(|e| format!("{what}: write failed: {e}"))?;
            if let Some(data) = wait_ack(u16::from(command::SESSION_ACK), seq) {
                return Ok(data);
            }
        }
        Err(format!("{what}: no ACK from the FC"))
    }

    /// [`Link::request`], reduced to the ACK's code.
    fn code(&mut self, what: &str, build: impl Fn(u16) -> Vec<u8>) -> Result<u16, String> {
        let data = self.request(what, build)?;
        command::ack_code(&data).ok_or_else(|| format!("{what}: empty ACK"))
    }
}

fn wait_ack(key: u16, seq: u16) -> Option<Vec<u8>> {
    let deadline = Instant::now() + ACK_TIMEOUT;
    let mut q = ACKS.lock().unwrap();
    loop {
        if let Some(i) = q.iter().position(|(k, n, _)| (*k, *n) == (key, seq)) {
            return q.remove(i).map(|(_, _, d)| d);
        }
        let left = deadline.checked_duration_since(Instant::now())?;
        q = ACK_READY.wait_timeout(q, left).unwrap().0;
    }
}
