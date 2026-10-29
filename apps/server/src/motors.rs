//! Motor requests from the UI.
//!
//! Start and stop go over the RC link when it is up: the injector sends
//! [`flyc::function_control_frame`] (`0x03/0x2a`, no activation or authority
//! needed) and the RC reader hands back the FC's ACK. Without the RC link, or
//! if the FC never ACKs there, they go over the OSDK UART instead.
//!
//! On the UART (OpenProtocol, see [`dji_protocol::osdk::command`]) each
//! request runs the whole sequence the Onboard SDK uses: get the FC version,
//! activate the app (`OSDK_APP_ID`), obtain control authority, act, then
//! release authority again so the RC keeps control. The act is one of:
//!
//! * start or stop all motors;
//! * allow arming while the USB API port is connected
//!   ([`flyc::usb_flight_frame`] on that port; the FC takes it only under
//!   authority and forgets it on restart).
//!
//! ACKs arrive on the readers ([`crate::flight`], [`crate::reader`]), which
//! hand them here; UART and USB requests go out on separate write-only handles
//! opened per request.
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

/// What the UI shows about motor commands, and the live motor state.
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
    /// Motors running, from the freshest source; `None` when neither is live.
    pub armed: Option<bool>,
    /// Where [`Status::armed`] came from: `UART` or `RC link`.
    pub armed_from: Option<&'static str>,
    /// Arming with the USB API port connected: `no_usb` (port closed, no
    /// lock), `locked`, `allowed` or `unknown`.
    pub usb_flight: &'static str,
}

/// How recent a source must be to count as live.
const FRESH: Duration = Duration::from_secs(2);
/// The FC flags the USB lock (error 200) only every few seconds; this long
/// without it, with the UART live, means arming is allowed.
const LOCK_QUIET: Duration = Duration::from_secs(10);
/// OSDK status error code while the USB lock is on.
const ERROR_USB_LOCK: u8 = 200;

/// Keep [`Status::armed`] and [`Status::usb_flight`] current, forever.
fn live(cmds: Shared, tele: Tele) {
    let start = Instant::now();
    // (frame count, when it last changed) per source.
    let mut uart = (0u64, start);
    let mut rc = (0u64, start);
    // When the UART last came back after a gap.
    let mut uart_since: Option<Instant> = None;
    let mut last_lock: Option<Instant> = None;
    let mut usb_session: Option<String> = None;
    loop {
        sleep(Duration::from_millis(100));
        let (uart_frames, status, rc_frames, link_up, rpm, usb) = {
            let t = tele.lock().unwrap();
            (
                t.flight.frames,
                t.flight.data.status.clone(),
                t.frames,
                t.link_up,
                t.motor_rpm,
                t.usb.open.then(|| t.usb.port.clone()),
            )
        };
        let now = Instant::now();
        let changed = |seen: &mut (u64, Instant), count: u64| {
            if count != seen.0 {
                *seen = (count, now);
            }
        };
        changed(&mut uart, uart_frames);
        changed(&mut rc, rc_frames);
        let uart_live = uart.0 > 0 && now - uart.1 < FRESH;
        if !uart_live {
            uart_since = None;
        } else if uart_since.is_none() {
            uart_since = Some(now);
        }
        let status = status.filter(|_| uart_live);
        let (armed, from) = match (&status, rpm) {
            (Some(s), _) => (Some(s.flight >= 1), Some("UART")),
            (None, Some(r)) if link_up && now - rc.1 < FRESH => (Some(r > 0.0), Some("RC link")),
            _ => (None, None),
        };
        if status.as_ref().is_some_and(|s| s.error == ERROR_USB_LOCK) {
            last_lock = Some(now);
        }
        let mut c = cmds.lock().unwrap();
        // A new USB session (aircraft restart, replug) forgets an earlier
        // unlock and lock.
        if usb != usb_session {
            usb_session.clone_from(&usb);
            c.usb_allowed = false;
            last_lock = None;
        }
        let locked = last_lock.is_some_and(|t| now - t < LOCK_QUIET);
        let quiet = uart_since.is_some_and(|t| now - t >= LOCK_QUIET) && !locked;
        c.motors.usb_flight = if usb.is_none() {
            "no_usb"
        } else if locked {
            "locked"
        } else if c.usb_allowed || quiet {
            "allowed"
        } else {
            "unknown"
        };
        c.motors.armed = armed;
        c.motors.armed_from = from;
    }
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

/// Hand the FC's DUML ACK of [`flyc::usb_flight_frame`] (USB port reader) or
/// [`flyc::function_control_frame`] (RC reader) to a waiting request.
pub fn deliver_duml_ack(f: &duml::Frame) {
    if f.crc_ok
        && f.cmd_type & 0x80 != 0
        && f.cmd_set == flyc::CMD_SET
        && [flyc::CMD_ID_USB_FLIGHT, flyc::CMD_ID_FUNCTION_CONTROL].contains(&f.cmd_id)
    {
        push_ack(DUML_KEY + u16::from(f.cmd_id), f.seq, f.payload);
    }
}

const DUML_KEY: u16 = 0x100;

/// Carry out motor requests from the UI, one at a time, forever.
pub fn run(cmds: Shared, tele: Tele, app_id: Option<u32>) {
    {
        let (cmds, tele) = (std::sync::Arc::clone(&cmds), std::sync::Arc::clone(&tele));
        std::thread::spawn(move || live(cmds, tele));
    }
    loop {
        sleep(Duration::from_millis(50));
        let Some(req) = cmds.lock().unwrap().motors_pending.take() else {
            continue;
        };
        let what = req.name();
        logln!("motors: {what} requested");
        set_status(
            &cmds,
            true,
            what,
            None,
            match req {
                Request::Start => "starting motors…",
                Request::Stop => "stopping motors…",
                Request::UsbFlight => "allowing motors with USB connected…",
            }
            .into(),
        );
        let result = execute(req, &cmds, &tele, app_id);
        match &result {
            Ok(m) => logln!("motors: {m}"),
            Err(e) => logln!("motors: {what} failed: {e}"),
        }
        if req == Request::UsbFlight && result.is_ok() {
            cmds.lock().unwrap().usb_allowed = true;
        }
        set_status(
            &cmds,
            false,
            what,
            Some(result.is_ok()),
            result.unwrap_or_else(|e| e),
        );
    }
}

/// Update the request fields of the status, leaving the live state alone.
fn set_status(cmds: &Shared, busy: bool, last: &'static str, ok: Option<bool>, message: String) {
    let m = &mut cmds.lock().unwrap().motors;
    m.busy = busy;
    m.last = Some(last);
    m.ok = ok;
    m.message = message;
}

/// How an RC-link attempt ended short of success.
enum RcFail {
    /// The FC answered with a refusal; the UART would be refused too.
    Refused(String),
    /// Nothing came back (or nothing could be sent); worth trying the UART.
    Silent(&'static str),
}

fn execute(
    req: Request,
    cmds: &Shared,
    tele: &Tele,
    app_id: Option<u32>,
) -> Result<String, String> {
    let mut note = "";
    if req != Request::UsbFlight && tele.lock().unwrap().link_up {
        match via_rc(req == Request::Start, cmds) {
            Ok(m) => return Ok(m),
            Err(RcFail::Refused(e)) => return Err(e),
            Err(RcFail::Silent(why)) => {
                logln!("motors: RC link: {why}; trying the UART");
                note = why;
            }
        }
    }
    via_uart(req, tele, app_id).map_err(|e| {
        if note.is_empty() {
            e
        } else {
            format!("RC link: {note}; UART: {e}")
        }
    })
}

/// Start/stop over the RC link: have the injector send `0x03/0x2a` and wait
/// for the FC's ACK under the sequence number it went out with.
fn via_rc(on: bool, cmds: &Shared) -> Result<String, RcFail> {
    let key = DUML_KEY + u16::from(flyc::CMD_ID_FUNCTION_CONTROL);
    let mut sent_any = false;
    for _ in 0..ATTEMPTS {
        {
            let mut c = cmds.lock().unwrap();
            c.rc_motor_sent = None;
            c.rc_motor_pending = Some(on);
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        let seq = loop {
            if let Some(seq) = cmds.lock().unwrap().rc_motor_sent.take() {
                break Some(seq);
            }
            if Instant::now() >= deadline {
                break None;
            }
            sleep(Duration::from_millis(10));
        };
        let Some(seq) = seq else {
            // Not written (RC busy or gone); don't let it go out late.
            cmds.lock().unwrap().rc_motor_pending = None;
            continue;
        };
        sent_any = true;
        if let Some(data) = wait_ack(key, seq) {
            let what = if on { "started" } else { "stopped" };
            return match data.first() {
                Some(0) => Ok(format!("motors {what} over the RC link")),
                _ => Err(RcFail::Refused(format!(
                    "FC refused over the RC link (ACK {})",
                    hex(&data)
                ))),
            };
        }
    }
    Err(RcFail::Silent(if sent_any {
        "no ACK from the FC"
    } else {
        "could not send"
    }))
}

fn via_uart(req: Request, tele: &Tele, app_id: Option<u32>) -> Result<String, String> {
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
                    Ok(format!(
                        "motors {} over the UART",
                        if on { "started" } else { "stopped" }
                    ))
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
