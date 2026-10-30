//! The live motor state for the UI: whether the motors run (from the UART
//! status when it is live, else the RC link's motor RPM) and whether the FC
//! lets them arm with the USB API port connected.

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::Vehicle;

/// How recent a source must be to count as live.
const FRESH: Duration = Duration::from_secs(2);
/// The FC flags the USB lock (error 200) only every few seconds; this long
/// without it, with the UART live, means arming is allowed.
const LOCK_QUIET: Duration = Duration::from_secs(10);
/// OSDK status error code while the USB lock is on.
const ERROR_USB_LOCK: u8 = 200;

/// Keep [`Status::armed`](super::Status::armed) and
/// [`Status::usb_flight`](super::Status::usb_flight) current, forever.
pub fn run(vehicle: &Vehicle) {
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
