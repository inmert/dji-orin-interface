//! FunctionFS (device/gadget side): the descriptor and string blobs written to
//! ep0, the ep0 event stream, and the USB setup packet. Lets the gadget answer
//! the controller's AOA handshake and expose two bulk endpoints.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::thread::sleep;
use std::time::Duration;

use super::sys;
use dji_common::logln;

// Descriptor/string blob magics and flags (<linux/usb/functionfs.h>).
const DESCRIPTORS_MAGIC_V2: u32 = 3;
const STRINGS_MAGIC: u32 = 2;
const HAS_FS_DESC: u32 = 1;
const HAS_HS_DESC: u32 = 2;
const ALL_CTRL_RECIP: u32 = 1 << 6;

// One vendor-specific interface + 2 bulk endpoints (OUT=0x01, IN=0x81).
const FS_DESC: [u8; 23] = [
    9, 4, 0, 0, 2, 0xff, 0xff, 0x00, 1, // interface
    7, 5, 0x01, 0x02, 64, 0, 0, // ep1 OUT bulk, 64
    7, 5, 0x81, 0x02, 64, 0, 0, // ep2 IN  bulk, 64
];
const HS_DESC: [u8; 23] = [
    9, 4, 0, 0, 2, 0xff, 0xff, 0x00, 1, //
    7, 5, 0x01, 0x02, 0x00, 0x02, 0, // 512
    7, 5, 0x81, 0x02, 0x00, 0x02, 0,
];

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// Write the FunctionFS v2 descriptor blob to ep0.
pub fn write_descriptors(ep0: BorrowedFd<'_>) -> io::Result<()> {
    let mut b = Vec::new();
    let total = 20 + FS_DESC.len() + HS_DESC.len();
    put32(&mut b, DESCRIPTORS_MAGIC_V2);
    put32(&mut b, total as u32);
    put32(&mut b, HAS_FS_DESC | HAS_HS_DESC | ALL_CTRL_RECIP);
    put32(&mut b, 3); // fs descriptor count
    put32(&mut b, 3); // hs descriptor count
    b.extend_from_slice(&FS_DESC);
    b.extend_from_slice(&HS_DESC);
    sys::write(ep0, &b).map(|_| ())
}

/// Write the FunctionFS string blob (one string, language 0x0409) to ep0.
pub fn write_strings(ep0: BorrowedFd<'_>) -> io::Result<()> {
    let s = b"DJI-Capture\0";
    let mut b = Vec::new();
    put32(&mut b, STRINGS_MAGIC);
    put32(&mut b, (16 + 2 + s.len()) as u32);
    put32(&mut b, 1); // str count
    put32(&mut b, 1); // lang count
    b.extend_from_slice(&[0x09, 0x04]); // language 0x0409, LE
    b.extend_from_slice(s);
    sys::write(ep0, &b).map(|_| ())
}

/// A USB setup packet (the 8 bytes at the head of an [`Event::Setup`]).
#[derive(Debug, Clone, Copy)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    /// Parse the 8-byte setup packet.
    pub fn parse(s: &[u8; 8]) -> Self {
        Self {
            request_type: s[0],
            request: s[1],
            value: u16::from_le_bytes([s[2], s[3]]),
            index: u16::from_le_bytes([s[4], s[5]]),
            length: u16::from_le_bytes([s[6], s[7]]),
        }
    }

    /// Host→device data direction bit of `bmRequestType`.
    pub fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }

    /// `bmRequestType` type field is "vendor".
    pub fn is_vendor(&self) -> bool {
        self.request_type & 0x60 == 0x40
    }
}

/// A FunctionFS ep0 event. `usb_functionfs_event` is 12 bytes: the first 8 hold
/// a setup packet (only for `Setup`), byte 8 is the type.
#[derive(Debug, Clone, Copy)]
pub enum Event {
    Bind,
    Unbind,
    Enable,
    Disable,
    Setup(Setup),
    /// An event type we do not handle (value carried for logs).
    Other(u8),
}

/// Size of one `usb_functionfs_event`.
pub const EVENT_SIZE: usize = 12;

impl Event {
    /// Decode one 12-byte event record.
    pub fn parse(e: &[u8; EVENT_SIZE]) -> Self {
        const BIND: u8 = 0;
        const UNBIND: u8 = 1;
        const ENABLE: u8 = 2;
        const DISABLE: u8 = 3;
        const SETUP: u8 = 4;
        match e[8] {
            BIND => Event::Bind,
            UNBIND => Event::Unbind,
            ENABLE => Event::Enable,
            DISABLE => Event::Disable,
            SETUP => {
                let setup: [u8; 8] = e[0..8].try_into().expect("8 bytes");
                Event::Setup(Setup::parse(&setup))
            }
            other => Event::Other(other),
        }
    }
}

/// Stall ep0 (reject an unsupported request) by reading/writing against the
/// transfer direction, which FunctionFS turns into a protocol stall. Returns
/// the expected `EL2HLT` as success.
pub fn stall(ep0: BorrowedFd<'_>, dir_in: bool) -> io::Result<()> {
    // To stall, issue a transfer in the opposite direction of the data stage.
    let r = if dir_in {
        sys::read(ep0, &mut [])
    } else {
        sys::write(ep0, &[])
    };
    match r {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(sys::errno::EL2HLT) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Open `<ffs_dir>/<name>` read-write, retrying for up to `tries` × 20 ms (the
/// bulk endpoint files only open once the host has enabled the gadget).
pub fn open_endpoint(ffs_dir: &str, name: &str, tries: u32) -> io::Result<OwnedFd> {
    let path = format!("{ffs_dir}/{name}");
    let mut last = io::Error::other(format!("{path}: no attempts"));
    for i in 0..tries.max(1) {
        if i > 0 {
            sleep(Duration::from_millis(20));
        }
        match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => return Ok(f.into()),
            Err(e) => last = io::Error::new(e.kind(), format!("{path}: {e}")),
        }
    }
    Err(last)
}

/// Open the gadget's ep0 and write the descriptor and string blobs, after
/// which the UDC can be bound.
pub fn init_ep0(ffs_dir: &str) -> io::Result<OwnedFd> {
    let ep0 = open_endpoint(ffs_dir, "ep0", 1)?;
    write_descriptors(ep0.as_fd())?;
    write_strings(ep0.as_fd())?;
    Ok(ep0)
}

/// Read ep0 events forever, calling `on_event` for each. Read errors back off
/// (50 ms); `ESHUTDOWN`/`ENODEV` (normal with no host attached) stay quiet and
/// any other error is logged once per distinct errno, not every spin.
pub fn run_events(ep0: BorrowedFd<'_>, mut on_event: impl FnMut(Event)) -> ! {
    let mut last_errno = 0;
    let mut ev = [0u8; EVENT_SIZE * 8];
    loop {
        match sys::read(ep0, &mut ev) {
            Ok(0) => sleep(Duration::from_millis(2)),
            Ok(n) => {
                last_errno = 0;
                for chunk in ev[..n].chunks_exact(EVENT_SIZE) {
                    let record: [u8; EVENT_SIZE] = chunk.try_into().expect("chunk size");
                    on_event(Event::parse(&record));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code != sys::errno::ESHUTDOWN && code != sys::errno::ENODEV && code != last_errno
                {
                    logln!("gadget: ep0 read error: {e}");
                }
                last_errno = code;
                sleep(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setup_event() {
        // bmRequestType=0x40 bRequest=52 wValue=0 wIndex=2 wLength=5, type=SETUP
        let e = [0x40, 52, 0, 0, 2, 0, 5, 0, 4, 0, 0, 0];
        match Event::parse(&e) {
            Event::Setup(s) => {
                assert!(s.is_vendor() && !s.is_in());
                assert_eq!((s.request, s.index, s.length), (52, 2, 5));
            }
            other => panic!("expected Setup, got {other:?}"),
        }
    }

    #[test]
    fn parses_enable_event() {
        let e = [0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0];
        assert!(matches!(Event::parse(&e), Event::Enable));
    }
}
