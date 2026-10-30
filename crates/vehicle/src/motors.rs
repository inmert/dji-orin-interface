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

use std::collections::VecDeque;
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
    /// Every queued request gets the next ticket, so a waiter can tell its
    /// own outcome from another request's.
    tickets: u64,
    /// The ticket of [`State::pending`].
    pending_ticket: u64,
    /// The ticket of the request being carried out, if any.
    executing: Option<u64>,
    /// Recent outcomes no waiter has collected yet: ticket, success,
    /// message. One per request, so two requests finishing close together
    /// cannot hide each other's (at most [`OUTCOMES`] are kept).
    done: VecDeque<(u64, bool, String)>,
    /// Queued requests later ones replaced before they ran (as many).
    displaced: VecDeque<u64>,
    /// "Allow with USB" succeeded during the current USB session.
    pub usb_allowed: bool,
    /// Motor start / stop over the RC link, for the RC writer to send once
    /// as `0x03/0x2a`. Set only by [`rc::send`], which owns the retries, the
    /// ACK wait and the "a stop beats a start" rule.
    rc_pending: Option<RcFrame>,
    /// The attempt [`rc::send`] is waiting on: only its frame may be
    /// requeued or reported sent, so a write that comes back after the
    /// attempt gave up changes nothing.
    rc_attempt: Option<u64>,
    rc_attempts: u64,
    /// The sequence number the writer sent the current attempt's frame
    /// with, for [`rc::send`] to match the FC's ACK.
    rc_sent: Option<u16>,
}

/// Outcomes and displacements kept for waiters.
const OUTCOMES: usize = 16;

/// One RC-link motor frame for the writer: start (`on`) or stop, tagged with
/// the [`rc::send`] attempt it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcFrame {
    pub on: bool,
    attempt: u64,
}

fn bounded<T>(q: &mut VecDeque<T>, item: T) {
    q.push_back(item);
    while q.len() > OUTCOMES {
        q.pop_front();
    }
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
        self.enqueue(req).map(|_| ())
    }

    /// [`Motors::request`], returning the request's ticket.
    fn enqueue(&self, req: Request) -> Result<u64, &'static str> {
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
        if s.pending.is_some() {
            let t = s.pending_ticket;
            bounded(&mut s.displaced, t);
        }
        s.tickets += 1;
        s.pending_ticket = s.tickets;
        s.pending = Some(req);
        Ok(s.pending_ticket)
    }

    /// For the worker: the next request and its ticket.
    fn take(&self) -> Option<(Request, u64)> {
        let mut s = self.lock();
        let req = s.pending.take()?;
        s.executing = Some(s.pending_ticket);
        Some((req, s.pending_ticket))
    }

    /// For the worker: `ticket` finished.
    fn finished(&self, ticket: u64, ok: bool, message: String) {
        let mut s = self.lock();
        s.executing = None;
        bounded(&mut s.done, (ticket, ok, message));
    }

    /// Queue `req` and wait for *that* request to be carried out: the outcome
    /// message, or why it failed. Another request's completion (a UI button
    /// pressed meanwhile) is not mistaken for it. `Err` also when a later
    /// request replaced it before it ran, or when nothing picks it up within
    /// `timeout`; a request still waiting then is withdrawn, so it cannot run
    /// after its caller gave up on it.
    pub fn perform(&self, req: Request, timeout: Duration) -> Result<String, String> {
        let ticket = self.enqueue(req)?;
        let deadline = Instant::now() + timeout;
        loop {
            sleep(Duration::from_millis(50));
            let mut s = self.lock();
            if let Some(i) = s.done.iter().position(|(t, ..)| *t == ticket) {
                let (_, ok, message) = s.done.remove(i).expect("found above");
                return if ok { Ok(message) } else { Err(message) };
            }
            if let Some(i) = s.displaced.iter().position(|t| *t == ticket) {
                s.displaced.remove(i);
                return Err(format!("{} replaced by a later request", req.name()));
            }
            if Instant::now() >= deadline {
                if s.pending.is_some() && s.pending_ticket == ticket {
                    s.pending = None;
                }
                return Err(format!("{} did not complete in time", req.name()));
            }
        }
    }

    /// Whether a stop is waiting: a start in progress gives up between its
    /// steps so the stop goes out next.
    pub fn stop_requested(&self) -> bool {
        self.lock().pending == Some(Request::Stop)
    }

    /// For the RC writer: the motor frame to send once, if any.
    pub fn take_rc_pending(&self) -> Option<RcFrame> {
        self.lock().rc_pending.take()
    }

    /// For the RC writer: the frame could not be written; send it next tick,
    /// unless its attempt has given up meanwhile.
    pub fn requeue_rc(&self, frame: RcFrame) {
        let mut s = self.lock();
        if s.rc_attempt == Some(frame.attempt) {
            s.rc_pending.get_or_insert(frame);
        }
    }

    /// For the RC writer: the frame went out with sequence `seq`.
    pub fn rc_sent(&self, frame: RcFrame, seq: u16) {
        let mut s = self.lock();
        if s.rc_attempt == Some(frame.attempt) {
            s.rc_sent = Some(seq);
        }
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
        let Some((req, ticket)) = motors.take() else {
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
        let ok = result.is_ok();
        let message = result.unwrap_or_else(|e| e);
        set_status(motors, false, what, Some(ok), message.clone());
        motors.finished(ticket, ok, message);
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
            Err(rc::Fail::Abandoned) => return Err("start abandoned for a stop".into()),
            Err(rc::Fail::Silent(why)) => {
                logln!("motors: RC link: {why}; trying the UART");
                note = why;
            }
        }
        if req == Request::Start && vehicle.motors.stop_requested() {
            return Err(format!("RC link: {note}; start abandoned for a stop"));
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
    fn a_request_that_timed_out_is_withdrawn() {
        let m = Motors::default();
        // Nobody runs requests here: perform times out, and the start must
        // not stay queued to run after its caller gave up (it would arm the
        // motors after a plan already ended).
        let err = m
            .perform(Request::Start, Duration::from_millis(120))
            .unwrap_err();
        assert!(err.contains("did not complete"), "{err}");
        assert_eq!(m.lock().pending, None);
        // One being carried out cannot be withdrawn, and a newer one is not
        // touched by an older waiter.
        let t = m.enqueue(Request::Stop).unwrap();
        assert_eq!(m.take().map(|(_, n)| n), Some(t));
        assert!(m.perform(Request::Start, Duration::ZERO).is_err());
        assert_eq!(m.lock().executing, Some(t));
        // A stop cannot be displaced, so a start is refused outright.
        m.request(Request::Stop).unwrap();
        assert!(m.perform(Request::Start, Duration::ZERO).is_err());
        assert!(m.stop_requested());
    }

    #[test]
    fn outcomes_finishing_together_reach_their_own_waiters() {
        let m = std::sync::Arc::new(Motors::default());
        // The executor's stop is carried out...
        let stop = {
            let m = std::sync::Arc::clone(&m);
            std::thread::spawn(move || m.perform(Request::Stop, Duration::from_secs(2)))
        };
        sleep(Duration::from_millis(20));
        let (_, t1) = m.take().unwrap();
        // ...and a UI request right behind it fails at once: both finish
        // within one poll of the stop's waiter.
        let t2 = m.enqueue(Request::UsbFlight).unwrap();
        m.finished(t1, true, "motors stopped".into());
        assert_eq!(m.take().map(|(_, n)| n), Some(t2));
        m.finished(t2, false, "OSDK_APP_ID is not set on the server".into());
        assert_eq!(stop.join().unwrap(), Ok("motors stopped".to_string()));
    }

    #[test]
    fn each_replaced_request_hears_so() {
        let m = std::sync::Arc::new(Motors::default());
        let wait = |req| {
            let m = std::sync::Arc::clone(&m);
            std::thread::spawn(move || m.perform(req, Duration::from_secs(2)))
        };
        // A start replaced by a stop, replaced in turn by another stop.
        let a = wait(Request::Start);
        sleep(Duration::from_millis(20));
        let b = wait(Request::Stop);
        sleep(Duration::from_millis(20));
        let c = wait(Request::Stop);
        sleep(Duration::from_millis(20));
        let started = Instant::now();
        assert!(a.join().unwrap().unwrap_err().contains("replaced"));
        assert!(b.join().unwrap().unwrap_err().contains("replaced"));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "waited out the timeout"
        );
        let (_, t) = m.take().unwrap();
        m.finished(t, true, "motors stopped".into());
        assert_eq!(c.join().unwrap(), Ok("motors stopped".to_string()));
    }

    #[test]
    fn perform_reports_its_own_request_not_an_earlier_one() {
        let m = Motors::default();
        // A UI start is in progress when the executor queues a stop.
        m.request(Request::Start).unwrap();
        let (req, t1) = m.take().unwrap();
        assert_eq!(req, Request::Start);
        let m = std::sync::Arc::new(m);
        let stop = {
            let m = std::sync::Arc::clone(&m);
            std::thread::spawn(move || m.perform(Request::Stop, Duration::from_secs(2)))
        };
        sleep(Duration::from_millis(100));
        // The start finishes first: the stop's waiter must not take its outcome.
        m.finished(t1, true, "motors started".into());
        sleep(Duration::from_millis(150));
        assert!(
            !stop.is_finished(),
            "took the start's completion as its own"
        );
        let (req, t2) = m.take().unwrap();
        assert_eq!(req, Request::Stop);
        m.finished(t2, true, "motors stopped".into());
        assert_eq!(stop.join().unwrap().unwrap(), "motors stopped");
        // A request replaced before it ran says so instead of waiting.
        let a = m.enqueue(Request::Start).unwrap();
        m.request(Request::Stop).unwrap();
        assert_eq!(m.lock().displaced, [a]);
    }

    #[test]
    fn rc_frame_handshake_with_the_writer() {
        let m = Motors::default();
        assert_eq!(m.take_rc_pending(), None);
        let frame = RcFrame {
            on: true,
            attempt: 1,
        };
        {
            let mut s = m.lock();
            s.rc_attempt = Some(1);
            s.rc_pending = Some(frame);
        }
        assert_eq!(m.take_rc_pending(), Some(frame));
        m.requeue_rc(frame);
        m.requeue_rc(RcFrame { on: false, ..frame }); // the first requeue wins
        assert_eq!(m.take_rc_pending(), Some(frame));
        m.rc_sent(frame, 42);
        assert_eq!(m.lock().rc_sent.take(), Some(42));
        // Once the attempt has given up, a late writer changes nothing.
        m.lock().rc_attempt = None;
        m.requeue_rc(frame);
        m.rc_sent(frame, 43);
        assert_eq!((m.take_rc_pending(), m.lock().rc_sent), (None, None));
    }
}
