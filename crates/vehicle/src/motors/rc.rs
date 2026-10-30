//! Motor start/stop over the RC link: the RC writer sends
//! [`flyc::function_control_frame`] (`0x03/0x2a`) and the RC reader hands the
//! FC's ACK to the mailbox under the frame's sequence number.

use std::thread::sleep;
use std::time::{Duration, Instant};

use dji_protocol::flyc;

use super::{ACK_TIMEOUT, ATTEMPTS, DUML_KEY, Motors, hex};

/// How an RC-link attempt ended short of success.
pub enum Fail {
    /// The FC answered with a refusal; the UART would be refused too.
    Refused(String),
    /// Nothing came back (or nothing could be sent); worth trying the UART.
    Silent(&'static str),
}

/// Start/stop over the RC link: have the RC writer send `0x03/0x2a` and wait
/// for the FC's ACK under the sequence number it went out with.
pub fn send(on: bool, motors: &Motors) -> Result<String, Fail> {
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
