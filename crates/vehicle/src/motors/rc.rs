//! Motor start/stop over the RC link: the RC writer sends
//! [`flyc::function_control_frame`] (`0x03/0x2a`) and the RC reader hands the
//! FC's ACK to the mailbox under the frame's sequence number.

use std::thread::sleep;
use std::time::{Duration, Instant};

use dji_protocol::flyc;

use super::{ACK_TIMEOUT, ATTEMPTS, DUML_KEY, Motors, RcFrame, hex};

/// How an RC-link attempt ended short of success.
pub enum Fail {
    /// The FC answered with a refusal; the UART would be refused too.
    Refused(String),
    /// Nothing came back (or nothing could be sent); worth trying the UART.
    Silent(&'static str),
    /// A start given up because a stop is waiting; the stop goes next.
    Abandoned,
}

/// Start/stop over the RC link: have the RC writer send `0x03/0x2a` and wait
/// for the FC's ACK under the sequence number it went out with.
pub fn send(on: bool, motors: &Motors) -> Result<String, Fail> {
    let key = DUML_KEY + u16::from(flyc::CMD_ID_FUNCTION_CONTROL);
    let mut sent_any = false;
    for attempt in 0..ATTEMPTS {
        if on && attempt > 0 && motors.stop_requested() {
            return Err(Fail::Abandoned);
        }
        {
            let mut m = motors.lock();
            m.rc_attempts += 1;
            let frame = RcFrame {
                on,
                attempt: m.rc_attempts,
            };
            m.rc_attempt = Some(frame.attempt);
            m.rc_sent = None;
            m.rc_pending = Some(frame);
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
        {
            // Written or not, this attempt is over for the writer: a frame
            // not yet written must not go out late (RC busy or gone).
            let mut m = motors.lock();
            m.rc_attempt = None;
            m.rc_pending = None;
        }
        let Some(seq) = seq else {
            continue;
        };
        sent_any = true;
        if let Some(data) = motors.mailbox.wait(key, seq, ACK_TIMEOUT) {
            let what = if on { "started" } else { "stopped" };
            return match data.first() {
                Some(0) => Ok(format!("motors {what} over the RC link")),
                _ => Err(Fail::Refused(format!(
                    "FC refused over the RC link (ACK {})",
                    hex(&data)
                ))),
            };
        }
    }
    Err(Fail::Silent(if sent_any {
        "no ACK from the FC"
    } else {
        "could not send"
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dji_protocol::duml::{self, Device, Header};

    use super::*;

    /// The FC's DUML answer to a `0x03/0x2a` frame sent with `seq`.
    fn ack(seq: u16, code: u8) -> Vec<u8> {
        let h = Header {
            sender: duml::addr(Device::FlightCtrl, 0),
            receiver: duml::addr(Device::App, 0),
            seq,
            cmd_type: 0x80,
            cmd_set: flyc::CMD_SET,
            cmd_id: flyc::CMD_ID_FUNCTION_CONTROL,
        };
        duml::build(&h, &[code]).unwrap()
    }

    /// Plays the RC writer and the FC: takes each motor frame, reports it
    /// sent, and (with `code`) answers it as the reader would.
    fn writer(motors: &Arc<Motors>, code: Option<u8>) {
        let motors = Arc::clone(motors);
        std::thread::spawn(move || {
            let mut seq = 100u16;
            loop {
                sleep(Duration::from_millis(5));
                if let Some(frame) = motors.take_rc_pending() {
                    seq += 1;
                    motors.rc_sent(frame, seq);
                    if let Some(code) = code {
                        let frame = ack(seq, code);
                        motors.deliver_duml_ack(&duml::parse(&frame).unwrap().0);
                    }
                }
            }
        });
    }

    #[test]
    fn an_acked_frame_succeeds_and_a_refusal_is_final() {
        let m = Arc::new(Motors::default());
        writer(&m, Some(0x00));
        assert_eq!(
            send(true, &m).ok(),
            Some("motors started over the RC link".to_string())
        );
        let m = Arc::new(Motors::default());
        writer(&m, Some(0x05));
        match send(false, &m) {
            Err(Fail::Refused(e)) => assert!(e.contains("ACK 05"), "{e}"),
            _ => panic!("a refusal must not be retried or passed to the UART"),
        }
    }

    #[test]
    fn nothing_written_and_nothing_acked_are_told_apart() {
        // No writer at all: the frame never goes out, and is withdrawn.
        let m = Motors::default();
        assert!(matches!(
            send(true, &m),
            Err(Fail::Silent("could not send"))
        ));
        assert_eq!(m.take_rc_pending(), None, "a late write must not happen");
        // Written, but the FC stays silent through every attempt.
        let m = Arc::new(Motors::default());
        writer(&m, None);
        assert!(matches!(
            send(true, &m),
            Err(Fail::Silent("no ACK from the FC"))
        ));
    }

    #[test]
    fn an_ack_for_another_sequence_is_not_taken() {
        let m = Arc::new(Motors::default());
        // An old ACK waits in the mailbox under a sequence the writer will not use.
        let stale = ack(7, 0x00);
        m.deliver_duml_ack(&duml::parse(&stale).unwrap().0);
        writer(&m, Some(0x01));
        assert!(matches!(send(true, &m), Err(Fail::Refused(_))));
    }
}
