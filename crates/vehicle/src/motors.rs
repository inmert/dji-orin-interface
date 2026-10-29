//! Motor requests from the UI.
//!
//! Start and stop go over the RC link when it is up: the RC writer sends
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
//! ACKs arrive on the link readers ([`crate::links`]), which hand them to the
//! [`Mailbox`]; UART and USB requests go out on separate write-only handles
//! opened per request. The RC-link frame is written by the RC writer, which
//! takes it from [`Motors::take_rc_pending`].
//!
//! **This spins the real motors.** It only ever runs on an explicit UI
//! request; props off unless flying.

use std::sync::{Mutex, MutexGuard};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_common::logln;
use dji_protocol::osdk::{self, command};
use dji_protocol::{duml, flyc};
use dji_transport::serial::TxPort;

use crate::Vehicle;
use crate::mailbox::Mailbox;

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

/// Requests, progress and the RC-link handshake with the writer.
#[derive(Default)]
pub struct State {
    /// A request from the UI, carried out once by [`run`].
    pub pending: Option<Request>,
    pub status: Status,
    /// Requests carried out so far (success or not); [`Motors::perform`]
    /// waits for this to move.
    completed: u64,
    /// "Allow with USB" succeeded during the current USB session.
    pub usb_allowed: bool,
    /// Motor start (`true`) / stop over the RC link, for the RC writer to
    /// send once as `0x03/0x2a`. Set only by [`via_rc`], which owns the
    /// retries, the ACK wait and the "a stop beats a start" rule.
    rc_pending: Option<bool>,
    /// The sequence number the writer sent the last RC-link motor frame
    /// with, for [`via_rc`] to match the FC's ACK.
    rc_sent: Option<u16>,
}

/// The motors feature: requests in, status out, ACKs through the mailbox.
#[derive(Default)]
pub struct Motors {
    state: Mutex<State>,
    pub mailbox: Mailbox,
}

impl Motors {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn status(&self) -> Status {
        self.lock().status.clone()
    }

    /// Queue a request. A stop replaces anything waiting; nothing replaces a
    /// waiting stop, and the USB unlock never displaces a motor request.
    /// `Err` names why it was not queued.
    pub fn request(&self, req: Request) -> Result<(), &'static str> {
        let mut s = self.lock();
        match req {
            Request::Start if s.pending == Some(Request::Stop) => {
                return Err("start ignored, stop pending");
            }
            Request::UsbFlight if s.pending.is_some() => {
                return Err("usb_flight ignored, a motor request is pending");
            }
            _ => {}
        }
        s.pending = Some(req);
        Ok(())
    }

    /// Queue `req` and wait for it to be carried out: the outcome message,
    /// or why it failed. `Err` also when nothing picks the request up within
    /// `timeout` (the motor worker is not running).
    pub fn perform(&self, req: Request, timeout: Duration) -> Result<String, String> {
        let before = self.lock().completed;
        self.request(req)?;
        let deadline = Instant::now() + timeout;
        loop {
            sleep(Duration::from_millis(50));
            let s = self.lock();
            if s.completed > before {
                return match s.status.ok {
                    Some(true) => Ok(s.status.message.clone()),
                    _ => Err(s.status.message.clone()),
                };
            }
            if Instant::now() >= deadline {
                return Err(format!("{} did not complete in time", req.name()));
            }
        }
    }

    /// For the RC writer: the motor frame to send once, if any.
    pub fn take_rc_pending(&self) -> Option<bool> {
        self.lock().rc_pending.take()
    }

    /// For the RC writer: the frame could not be written; send it next tick.
    pub fn requeue_rc(&self, on: bool) {
        self.lock().rc_pending.get_or_insert(on);
    }

    /// For the RC writer: the frame went out with sequence `seq`.
    pub fn rc_sent(&self, seq: u16) {
        self.lock().rc_sent = Some(seq);
    }

    /// Hand an OpenProtocol ACK from the SDK UART reader to a waiting request.
    pub fn deliver_ack(&self, f: &osdk::Frame) {
        if f.ack {
            self.mailbox.push(u16::from(f.session), f.seq, f.payload);
        }
    }

    /// Hand the FC's DUML ACK of [`flyc::usb_flight_frame`] (USB port reader)
    /// or [`flyc::function_control_frame`] (RC reader) to a waiting request.
    pub fn deliver_duml_ack(&self, f: &duml::Frame) {
        if f.crc_ok
            && f.cmd_type & 0x80 != 0
            && f.cmd_set == flyc::CMD_SET
            && [flyc::CMD_ID_USB_FLIGHT, flyc::CMD_ID_FUNCTION_CONTROL].contains(&f.cmd_id)
        {
            self.mailbox
                .push(DUML_KEY + u16::from(f.cmd_id), f.seq, f.payload);
        }
    }
}

/// How recent a source must be to count as live.
const FRESH: Duration = Duration::from_secs(2);
/// The FC flags the USB lock (error 200) only every few seconds; this long
/// without it, with the UART live, means arming is allowed.
const LOCK_QUIET: Duration = Duration::from_secs(10);
/// OSDK status error code while the USB lock is on.
const ERROR_USB_LOCK: u8 = 200;

/// Keep [`Status::armed`] and [`Status::usb_flight`] current, forever.
fn live(vehicle: &Vehicle) {
    let (tele, motors) = (&vehicle.telemetry, &vehicle.motors);
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
        let mut m = motors.lock();
        // A new USB session (aircraft restart, replug) forgets an earlier
        // unlock and lock.
        if usb != usb_session {
            usb_session.clone_from(&usb);
            m.usb_allowed = false;
            last_lock = None;
        }
        let locked = last_lock.is_some_and(|t| now - t < LOCK_QUIET);
        let quiet = uart_since.is_some_and(|t| now - t >= LOCK_QUIET) && !locked;
        m.status.usb_flight = if usb.is_none() {
            "no_usb"
        } else if locked {
            "locked"
        } else if m.usb_allowed || quiet {
            "allowed"
        } else {
            "unknown"
        };
        m.status.armed = armed;
        m.status.armed_from = from;
    }
}

/// Mailbox key of a DUML ACK: `0x100 + cmd_id` (OpenProtocol keys are ≤ 31).
const DUML_KEY: u16 = 0x100;

/// Carry out motor requests, one at a time, forever; `app_id` is the Onboard
/// SDK app the UART path activates (`None` disables that path).
pub fn run(vehicle: &std::sync::Arc<Vehicle>, app_id: Option<u32>) {
    {
        let vehicle = std::sync::Arc::clone(vehicle);
        std::thread::spawn(move || live(&vehicle));
    }
    let motors = &vehicle.motors;
    loop {
        sleep(Duration::from_millis(50));
        let Some(req) = motors.lock().pending.take() else {
            continue;
        };
        let what = req.name();
        logln!("motors: {what} requested");
        set_status(
            motors,
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
        let result = execute(req, vehicle, app_id);
        match &result {
            Ok(m) => logln!("motors: {m}"),
            Err(e) => logln!("motors: {what} failed: {e}"),
        }
        if req == Request::UsbFlight && result.is_ok() {
            motors.lock().usb_allowed = true;
        }
        set_status(
            motors,
            false,
            what,
            Some(result.is_ok()),
            result.unwrap_or_else(|e| e),
        );
        motors.lock().completed += 1;
    }
}

/// Update the request fields of the status, leaving the live state alone.
fn set_status(motors: &Motors, busy: bool, last: &'static str, ok: Option<bool>, message: String) {
    let m = &mut motors.lock().status;
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

fn execute(req: Request, vehicle: &Vehicle, app_id: Option<u32>) -> Result<String, String> {
    let mut note = "";
    if req != Request::UsbFlight && vehicle.telemetry.lock().unwrap().link_up {
        match via_rc(req == Request::Start, &vehicle.motors) {
            Ok(m) => return Ok(m),
            Err(RcFail::Refused(e)) => return Err(e),
            Err(RcFail::Silent(why)) => {
                logln!("motors: RC link: {why}; trying the UART");
                note = why;
            }
        }
    }
    via_uart(req, vehicle, app_id).map_err(|e| {
        if note.is_empty() {
            e
        } else {
            format!("RC link: {note}; UART: {e}")
        }
    })
}

/// Start/stop over the RC link: have the RC writer send `0x03/0x2a` and wait
/// for the FC's ACK under the sequence number it went out with.
fn via_rc(on: bool, motors: &Motors) -> Result<String, RcFail> {
    let key = DUML_KEY + u16::from(flyc::CMD_ID_FUNCTION_CONTROL);
    let mut sent_any = false;
    for _ in 0..ATTEMPTS {
        {
            let mut m = motors.lock();
            m.rc_sent = None;
            m.rc_pending = Some(on);
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        let seq = loop {
            if let Some(seq) = motors.lock().rc_sent.take() {
                break Some(seq);
            }
            if Instant::now() >= deadline {
                break None;
            }
            sleep(Duration::from_millis(10));
        };
        let Some(seq) = seq else {
            // Not written (RC busy or gone); don't let it go out late.
            motors.lock().rc_pending = None;
            continue;
        };
        sent_any = true;
        if let Some(data) = motors.mailbox.wait(key, seq, ACK_TIMEOUT) {
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

fn via_uart(req: Request, vehicle: &Vehicle, app_id: Option<u32>) -> Result<String, String> {
    let app_id = app_id.ok_or("OSDK_APP_ID is not set on the server")?;
    let (port, usb_port) = {
        let t = vehicle.telemetry.lock().unwrap();
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
        mailbox: &vehicle.motors.mailbox,
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
        (Request::UsbFlight, Some((tx, p))) => usb_flight(tx, &p, link.seq, link.mailbox),
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
fn usb_flight(
    mut tx: TxPort,
    port: &str,
    seq_base: u16,
    mailbox: &Mailbox,
) -> Result<String, String> {
    for i in 0..ATTEMPTS as u16 {
        let seq = seq_base.wrapping_add(0x4000 + i);
        tx.send(&flyc::usb_flight_frame(seq, true))
            .map_err(|e| format!("USB flight: write to {port} failed: {e}"))?;
        if let Some(data) = mailbox.wait(
            DUML_KEY + u16::from(flyc::CMD_ID_USB_FLIGHT),
            seq,
            ACK_TIMEOUT,
        ) {
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

struct Link<'a> {
    tx: TxPort,
    seq: u16,
    mailbox: &'a Mailbox,
}

impl Link<'_> {
    /// Send a request (rebuilt with a fresh sequence number on each attempt)
    /// and return its ACK data.
    fn request(&mut self, what: &str, build: impl Fn(u16) -> Vec<u8>) -> Result<Vec<u8>, String> {
        for _ in 0..ATTEMPTS {
            self.seq = self.seq.wrapping_add(1);
            let seq = self.seq;
            self.tx
                .send(&build(seq))
                .map_err(|e| format!("{what}: write failed: {e}"))?;
            if let Some(data) = self
                .mailbox
                .wait(u16::from(command::SESSION_ACK), seq, ACK_TIMEOUT)
            {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_stop_is_never_replaced_by_a_start() {
        let m = Motors::default();
        m.request(Request::Start).unwrap();
        assert_eq!(m.lock().pending, Some(Request::Start));
        m.request(Request::Stop).unwrap();
        assert!(m.request(Request::Start).is_err());
        assert!(m.request(Request::UsbFlight).is_err());
        assert_eq!(m.lock().pending, Some(Request::Stop));
    }

    #[test]
    fn perform_waits_for_completion_and_reports_the_outcome() {
        let m = Motors::default();
        // Nobody runs requests here: perform times out, and the request is
        // left queued for a worker.
        let err = m
            .perform(Request::Start, Duration::from_millis(120))
            .unwrap_err();
        assert!(err.contains("did not complete"), "{err}");
        assert_eq!(m.lock().pending, Some(Request::Start));
        // A stop cannot be displaced, so a start is refused outright.
        m.request(Request::Stop).unwrap();
        assert!(m.perform(Request::Start, Duration::ZERO).is_err());
    }

    #[test]
    fn rc_frame_handshake_with_the_writer() {
        let m = Motors::default();
        assert_eq!(m.take_rc_pending(), None);
        m.lock().rc_pending = Some(true);
        assert_eq!(m.take_rc_pending(), Some(true));
        m.requeue_rc(true);
        m.requeue_rc(false); // the first requeue wins
        assert_eq!(m.take_rc_pending(), Some(true));
        m.rc_sent(42);
        assert_eq!(m.lock().rc_sent.take(), Some(42));
    }
}
