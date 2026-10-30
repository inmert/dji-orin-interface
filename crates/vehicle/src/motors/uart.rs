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

    // A start gives way to a stop that arrives meanwhile, between steps.
    let abandon = || req == Request::Start && vehicle.motors.stop_requested();
    let version = link.request("get version", command::get_version)?;
    let fw = command::fw_version(&version).ok_or("unreadable FC version")?;
    if abandon() {
        return Err(ABANDONED.into());
    }
    let code = link.code("activate", |s| command::activate(s, app_id, fw))?;
    if code != 0 {
        return Err(format!("activation: {}", command::activation_message(code)));
    }

    // From the first obtain request on, hand authority back on every way out:
    // an obtain whose ACKs were all lost may still have been granted.
    let mut obtained = Ok(command::OBTAIN_CONTROL_IN_PROGRESS);
    for _ in 0..5 {
        obtained = link.code("obtain control", |s| command::set_control(s, true));
        if obtained != Ok(command::OBTAIN_CONTROL_IN_PROGRESS) {
            break;
        }
        sleep(Duration::from_millis(500));
    }
    let acted = match obtained {
        Ok(command::OBTAIN_CONTROL_SUCCESS) if abandon() => Err(ABANDONED.to_string()),
        Ok(command::OBTAIN_CONTROL_SUCCESS) => act(req, usb, &mut link),
        // The FC answered and did not grant it: there is nothing to release.
        Ok(code) => {
            return Err(format!(
                "control authority: {}",
                command::control_message(code)
            ));
        }
        Err(e) => Err(e),
    };
    let released = release(&mut link);
    acted
        .map(|m| format!("{m}; {released}"))
        .map_err(|e| format!("{e}; {released}"))
}

/// Why a start ended early: a stop was requested while it ran.
const ABANDONED: &str = "start abandoned for a stop";

/// The act itself, under control authority: the USB unlock, or the motors.
fn act(req: Request, usb: Option<(TxPort, String)>, link: &mut Link) -> Result<String, String> {
    if let (Request::UsbFlight, Some((tx, p))) = (req, usb) {
        return usb_flight(tx, &p, link.seq, link.mailbox);
    }
    let on = req == Request::Start;
    let task = link.request("motors", |s| command::motors(s, on))?;
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
}

/// Hand control authority back to the RC, retrying while the FC says it is
/// still releasing; what happened, for the outcome message.
fn release(link: &mut Link) -> String {
    let mut release = link.code("release control", |s| command::set_control(s, false));
    for _ in 0..5 {
        if release != Ok(command::RELEASE_CONTROL_IN_PROGRESS) {
            break;
        }
        sleep(Duration::from_millis(500));
        release = link.code("release control", |s| command::set_control(s, false));
    }
    match release {
        Ok(command::RELEASE_CONTROL_SUCCESS) => "control released".to_string(),
        Ok(c) => format!("release: {}", command::control_message(c)),
        Err(e) => format!("release: {e}"),
    }
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

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    use dji_protocol::osdk::{self, Decoder};

    use super::*;

    /// What the fake FC asked for: (set, id, first payload byte).
    type Seen = Arc<Mutex<Vec<(u8, u8, u8)>>>;

    /// A vehicle whose SDK UART is a FIFO read by a fake FC, which answers
    /// each request with `answer(set, id, payload)` (`None`: silence), as
    /// the UART reader would hand its ACKs over.
    fn uart(
        name: &str,
        answer: impl Fn(&Vehicle, (u8, u8), &[u8]) -> Option<Vec<u8>> + Send + 'static,
    ) -> (Arc<Vehicle>, Seen) {
        let fifo = std::env::temp_dir().join(format!("dji-uart-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&fifo);
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo");
        let v = Vehicle::new();
        {
            let mut t = v.telemetry.lock().unwrap();
            t.flight.open = true;
            t.flight.port = fifo.to_string_lossy().into_owned();
        }
        let seen = Seen::default();
        {
            let (v, seen) = (Arc::clone(&v), Arc::clone(&seen));
            std::thread::spawn(move || {
                let mut rx = std::fs::File::open(&fifo).unwrap();
                let mut d = Decoder::new();
                let mut buf = [0u8; 512];
                while let Ok(n @ 1..) = rx.read(&mut buf) {
                    d.push(&buf[..n]);
                    let mut requests = Vec::new();
                    d.drain(|f| requests.push((f.seq, (f.cmd_set, f.cmd_id), f.payload.to_vec())));
                    for (seq, cmd, payload) in requests {
                        seen.lock().unwrap().push((cmd.0, cmd.1, payload[0]));
                        if let Some(data) = answer(&v, cmd, &payload) {
                            v.motors.deliver_ack(&osdk::Frame {
                                session: command::SESSION_ACK,
                                ack: true,
                                enc: 0,
                                seq,
                                cmd_set: 0,
                                cmd_id: 0,
                                payload: &data,
                            });
                        }
                    }
                }
                let _ = std::fs::remove_file(&fifo);
            });
        }
        (v, seen)
    }

    /// The FC's version answer, as the M210 V1 gives it.
    fn version() -> Vec<u8> {
        let mut a = vec![0, 0];
        a.extend_from_slice(b"095XE9X00200W8\0SDK-v1.1 BETA PM410-03.03.10.18\0");
        a
    }

    /// An FC that grants everything; `obtain` answers the obtain request.
    fn fc(obtain: Option<u16>) -> impl Fn(&Vehicle, (u8, u8), &[u8]) -> Option<Vec<u8>> {
        move |_, cmd, p| match (cmd, p[0]) {
            (command::GET_VERSION, _) => Some(version()),
            (command::ACTIVATE, _) => Some(vec![0, 0]),
            (command::SET_CONTROL, 1) => obtain.map(|c| c.to_le_bytes().to_vec()),
            (command::SET_CONTROL, _) => Some(vec![1, 0]),
            (command::TASK, _) => Some(vec![0, 0]),
            _ => None,
        }
    }

    const RELEASE: (u8, u8, u8) = (command::SET_CONTROL.0, command::SET_CONTROL.1, 0);
    const START: (u8, u8, u8) = (command::TASK.0, command::TASK.1, command::START_MOTOR);

    #[test]
    fn a_start_runs_the_whole_sequence_and_hands_authority_back() {
        let (v, seen) = uart("start", fc(Some(command::OBTAIN_CONTROL_SUCCESS)));
        assert_eq!(
            send(Request::Start, &v, Some(1234)),
            Ok("motors started over the UART; control released".to_string())
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[seen.len() - 2..], [START, RELEASE]);
    }

    #[test]
    fn a_lost_obtain_ack_still_releases_authority() {
        // The FC may have granted authority although no ACK came back.
        let (v, seen) = uart("lost", fc(None));
        let err = send(Request::Start, &v, Some(1234)).unwrap_err();
        assert!(err.contains("obtain control: no ACK"), "{err}");
        assert!(err.ends_with("control released"), "{err}");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.last(), Some(&RELEASE));
        assert!(!seen.contains(&START));
    }

    #[test]
    fn a_stop_requested_meanwhile_ends_a_start_before_the_motors() {
        let (v, seen) = uart("abandon", move |v, cmd, p| {
            if cmd == command::SET_CONTROL && p[0] == 1 {
                // The operator presses Disarm while authority is obtained.
                v.motors.request(Request::Stop).unwrap();
            }
            fc(Some(command::OBTAIN_CONTROL_SUCCESS))(v, cmd, p)
        });
        let err = send(Request::Start, &v, Some(1234)).unwrap_err();
        assert_eq!(err, "start abandoned for a stop; control released");
        assert!(!seen.lock().unwrap().contains(&START));
    }

    #[test]
    fn a_refused_obtain_needs_no_release() {
        let (v, seen) = uart("refused", fc(Some(0)));
        let err = send(Request::Start, &v, Some(1234)).unwrap_err();
        assert!(err.starts_with("control authority: RC mode error"), "{err}");
        assert!(!seen.lock().unwrap().contains(&RELEASE));
    }
}
