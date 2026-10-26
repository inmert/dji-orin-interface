//! Thin, dependency-free wrappers over the handful of libc calls the USB
//! plumbing needs. File descriptors are owned by [`std::os::fd::OwnedFd`], so
//! they close on drop; every call returns [`io::Result`] so errors surface
//! instead of being swallowed.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

mod c {
    use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
    // `open` and `ioctl` are variadic in C; rustc requires the declarations to
    // match libc's runtime symbols exactly, hence the `...`.
    extern "C" {
        pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
        pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
        pub fn usleep(usec: c_uint) -> c_int;
    }
}

pub const O_RDONLY: i32 = 0;
pub const O_RDWR: i32 = 2;

// errno values we branch on.
pub const EINTR: i32 = 4;
pub const EAGAIN: i32 = 11;
pub const ENODEV: i32 = 19;
pub const ETIMEDOUT: i32 = 110;
pub const ESHUTDOWN: i32 = 108;

// usbfs ioctl request codes (fixed on Linux; see <linux/usbdevice_fs.h>).
pub const USBDEVFS_CONTROL: u64 = 0xC018_5500;
pub const USBDEVFS_BULK: u64 = 0xC018_5502;
pub const USBDEVFS_CLAIMINTERFACE: u64 = 0x8004_550F;
pub const USBDEVFS_DISCONNECT_CLAIM: u64 = 0x8108_551B;

#[repr(C)]
pub struct CtrlTransfer {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
    pub timeout: u32,
    pub data: *mut core::ffi::c_void,
}

#[repr(C)]
pub struct BulkTransfer {
    pub ep: u32,
    pub len: u32,
    pub timeout: u32,
    pub data: *mut core::ffi::c_void,
}

#[repr(C)]
pub struct DisconnectClaim {
    pub interface: u32,
    pub flags: u32,
    pub driver: [u8; 256],
}

/// Open a path, returning an owned fd that closes on drop.
pub fn open(path: &str, flags: i32) -> io::Result<OwnedFd> {
    let cpath = CString::new(path).map_err(|_| io::Error::other("path contains NUL"))?;
    let fd = unsafe { c::open(cpath.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// read(2). Returns the number of bytes read (0 on a zero-length read).
pub fn read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { c::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// write(2). A zero-length slice performs a real zero-length write (used for
/// FunctionFS ep0 status-stage acknowledgements).
pub fn write(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    let n = unsafe { c::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// ioctl(2) with a pointer argument.
///
/// # Safety
/// `arg` must be a valid pointer to a structure of the size and layout the
/// given `request` expects; the kernel reads and/or writes through it.
pub unsafe fn ioctl(fd: RawFd, request: u64, arg: *mut core::ffi::c_void) -> io::Result<i32> {
    let r = c::ioctl(fd, request, arg);
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

pub fn usleep(usec: u32) {
    unsafe {
        c::usleep(usec);
    }
}

/// Convenience: raw fd of anything fd-like.
pub fn raw(fd: &impl AsRawFd) -> RawFd {
    fd.as_raw_fd()
}
