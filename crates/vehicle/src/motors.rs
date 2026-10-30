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
//! opened per request ([`uart`]). The RC-link frame is written by the RC
//! writer, which takes it from [`Motors::take_rc_pending`] ([`rc`]).
//! [`live`] keeps the live motor state and the USB-lock state current from
//! the telemetry.
//!
//! **This spins the real motors.** It only ever runs on an explicit UI
//! request; props off unless flying.

use std::sync::{Mutex, MutexGuard};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;

use dji_common::logln;
use dji_protocol::{duml, flyc, osdk};

pub mod live;
pub mod rc;
pub mod uart;

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
    /// send once as `0x03/0x2a`. Set only by [`rc::send`], which owns the
    /// retries, the ACK wait and the "a stop beats a start" rule.
    rc_pending: Option<bool>,
    /// The sequence number the writer sent the last RC-link motor frame
    /// with, for [`rc::send`] to match the FC's ACK.
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

/// Mailbox key of a DUML ACK: `0x100 + cmd_id` (OpenProtocol keys are ≤ 31).
const DUML_KEY: u16 = 0x100;

/// Carry out motor requests, one at a time, forever; `app_id` is the Onboard
/// SDK app the UART path activates (`None` disables that path).
pub fn run(vehicle: &std::sync::Arc<Vehicle>, app_id: Option<u32>) {
    {
        let vehicle = std::sync::Arc::clone(vehicle);
        std::thread::spawn(move || live::run(&vehicle));
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

fn execute(req: Request, vehicle: &Vehicle, app_id: Option<u32>) -> Result<String, String> {
    let mut note = "";
    if req != Request::UsbFlight && vehicle.telemetry.lock().unwrap().link_up {
        match rc::send(req == Request::Start, &vehicle.motors) {
            Ok(m) => return Ok(m),
            Err(rc::Fail::Refused(e)) => return Err(e),
            Err(rc::Fail::Silent(why)) => {
                logln!("motors: RC link: {why}; trying the UART");
                note = why;
            }
        }
    }
    uart::send(req, vehicle, app_id).map_err(|e| {
        if note.is_empty() {
            e
        } else {
            format!("RC link: {note}; UART: {e}")
        }
    })
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
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
