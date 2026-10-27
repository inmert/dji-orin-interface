//! The Orin as the AOA **device**, impersonating the DJI app toward the
//! controller: answering the ep0 SETUP packets of the controller's handshake.
//! Used by every gadget-side binary (`ffs_capture`, `bridge`, `server`).

use std::io;
use std::os::fd::BorrowedFd;

use super::{GET_PROTOCOL, PROTOCOL_VERSION, START};
use crate::usb::ffs::{self, Setup};
use crate::usb::sys;

/// How [`answer_setup`] handled a SETUP packet (for callers that log it).
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// Replied to `GET_PROTOCOL` with our version.
    Protocol,
    /// Acknowledged a no-data OUT request (e.g. `ACCESSORY_START`).
    Ack,
    /// Read the data stage of an OUT request (e.g. a `SEND_STRING` value).
    Data(Vec<u8>),
    /// Stalled an IN request we do not support.
    Stalled,
}

/// Answer one ep0 SETUP packet the way a DJI app device does.
pub fn answer_setup(ep0: BorrowedFd<'_>, s: &Setup) -> io::Result<Answer> {
    if s.is_vendor() && s.is_in() && s.request == GET_PROTOCOL {
        let ver = PROTOCOL_VERSION.to_le_bytes();
        let n = usize::from(s.length).min(ver.len());
        sys::write(ep0, &ver[..n])?;
        return Ok(Answer::Protocol);
    }
    if s.is_in() {
        // An IN request we do not support: stall it rather than returning a
        // bogus zero-length reply.
        ffs::stall(ep0, true)?;
        return Ok(Answer::Stalled);
    }
    if s.length > 0 && !(s.is_vendor() && s.request == START) {
        // OUT data stage we accept (e.g. AOA SEND_STRING): read it.
        let mut data = vec![0u8; usize::from(s.length).min(4096)];
        let n = sys::read(ep0, &mut data)?;
        data.truncate(n);
        return Ok(Answer::Data(data));
    }
    // No-data OUT (ACCESSORY_START or anything else): ack the status stage.
    sys::read(ep0, &mut [])?;
    Ok(Answer::Ack)
}
