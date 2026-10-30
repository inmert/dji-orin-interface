//! Motor requests over the OSDK UART (OpenProtocol, see
//! [`dji_protocol::osdk::command`]): get version, activate, obtain control
//! authority, act, release authority. The act is a motor start/stop, or the
//! USB flight unlock sent on the USB API port under that authority.

use std::thread::sleep;
use std::time::Duration;

use dji_protocol::flyc;
use dji_protocol::osdk::command;
use dji_transport::serial::TxPort;

use crate::Vehicle;
use crate::mailbox::Mailbox;

use super::{ACK_TIMEOUT, ATTEMPTS, DUML_KEY, Request, hex};

/// Carry out `req` for Onboard SDK app `app_id`.
pub fn send(req: Request, vehicle: &Vehicle, app_id: Option<u32>) -> Result<String, String> {
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
