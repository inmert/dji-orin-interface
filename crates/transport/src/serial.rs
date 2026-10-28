//! Raw serial ports (the aircraft's OSDK UART), opened **receive-only**.
//!
//! The port is opened `O_RDONLY`, so nothing written here can ever reach the
//! aircraft: the type has no write path at all. Line settings are raw 8N1, no
//! flow control, at one of the standard rates.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

mod c {
    use core::ffi::c_int;

    /// glibc's `struct termios` on Linux (NCCS = 32).
    #[repr(C)]
    pub struct Termios {
        pub iflag: u32,
        pub oflag: u32,
        pub cflag: u32,
        pub lflag: u32,
        pub line: u8,
        pub cc: [u8; 32],
        pub ispeed: u32,
        pub ospeed: u32,
    }

    unsafe extern "C" {
        pub fn tcgetattr(fd: c_int, t: *mut Termios) -> c_int;
        pub fn tcsetattr(fd: c_int, action: c_int, t: *const Termios) -> c_int;
        pub fn cfmakeraw(t: *mut Termios);
        pub fn cfsetspeed(t: *mut Termios, speed: u32) -> c_int;
    }

    pub const O_NOCTTY: i32 = 0o400;
    pub const CLOCAL: u32 = 0o4000;
    pub const CREAD: u32 = 0o200;
    pub const CRTSCTS: u32 = 0o20000000000;
    pub const VTIME: usize = 5;
    pub const VMIN: usize = 6;
    pub const TCSANOW: c_int = 0;
}

/// The `speed_t` code for a baud rate, if it is one we support.
fn speed_code(baud: u32) -> Option<u32> {
    Some(match baud {
        57_600 => 0o010001,
        115_200 => 0o010002,
        230_400 => 0o010003,
        460_800 => 0o010004,
        921_600 => 0o010007,
        1_000_000 => 0o010010,
        _ => return None,
    })
}

/// A serial port open for reading only.
pub struct RxPort {
    file: File,
}

impl RxPort {
    /// Open `path` read-only and configure raw 8N1 at `baud`. Reads block until
    /// at least one byte arrives or one second passes (then return 0).
    pub fn open(path: impl AsRef<Path>, baud: u32) -> io::Result<RxPort> {
        let speed = speed_code(baud).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported baud {baud}"),
            )
        })?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(c::O_NOCTTY)
            .open(path)?;
        let fd = file.as_raw_fd();
        let mut t = c::Termios {
            iflag: 0,
            oflag: 0,
            cflag: 0,
            lflag: 0,
            line: 0,
            cc: [0; 32],
            ispeed: 0,
            ospeed: 0,
        };
        // SAFETY: `fd` is open for the lifetime of `file`; `t` is a valid,
        // exclusively borrowed termios matching glibc's layout.
        if unsafe { c::tcgetattr(fd, &raw mut t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `t` is a valid termios; both calls only write into it.
        unsafe {
            c::cfmakeraw(&raw mut t);
            if c::cfsetspeed(&raw mut t, speed) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        t.cflag |= c::CLOCAL | c::CREAD;
        t.cflag &= !c::CRTSCTS;
        t.cc[c::VMIN] = 0;
        t.cc[c::VTIME] = 10; // deciseconds
        // SAFETY: as above; `t` is only read.
        if unsafe { c::tcsetattr(fd, c::TCSANOW, &raw const t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(RxPort { file })
    }

    /// Read whatever has arrived; `Ok(0)` means a second passed in silence.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termios_matches_glibc_size() {
        assert_eq!(size_of::<c::Termios>(), 60);
    }

    #[test]
    fn only_standard_rates() {
        assert_eq!(speed_code(230_400), Some(0o010003));
        assert_eq!(speed_code(230_000), None);
        assert!(RxPort::open("/dev/null", 1).is_err());
    }
}
