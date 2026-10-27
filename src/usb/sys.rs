//! Raw kernel ABI: the few libc calls std does not cover, plus the usbfs ioctl
//! structures and request codes. Safe, typed wrappers live in [`super::usbfs`]
//! and [`super::ffs`]; nothing outside those modules should need this one.

use core::ffi::{c_int, c_ulong, c_void};
use std::io;
use std::mem::size_of;
use std::os::fd::{AsRawFd, BorrowedFd};

mod c {
    use core::ffi::{c_int, c_ulong, c_void};
    // `ioctl` is variadic in C; the declaration must match libc's symbol.
    unsafe extern "C" {
        pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
        pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    }
}

/// errno values with no stable [`io::ErrorKind`] that we branch on
/// (asm-generic numbering, as used on aarch64 and x86_64).
pub mod errno {
    pub const ENODEV: i32 = 19;
    /// FunctionFS returns this from the ep0 read/write that stalls a request.
    pub const EL2HLT: i32 = 51;
    pub const ESHUTDOWN: i32 = 108;
}

/// `struct usbdevfs_ctrltransfer`.
#[repr(C)]
#[derive(Debug)]
pub struct CtrlTransfer {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
    /// Milliseconds; 0 waits forever.
    pub timeout: u32,
    pub data: *mut c_void,
}

/// `struct usbdevfs_bulktransfer`.
#[repr(C)]
#[derive(Debug)]
pub struct BulkTransfer {
    pub ep: u32,
    pub len: u32,
    /// Milliseconds; 0 waits forever.
    pub timeout: u32,
    pub data: *mut c_void,
}

/// `struct usbdevfs_disconnect_claim`.
#[repr(C)]
#[derive(Debug)]
pub struct DisconnectClaim {
    pub interface: u32,
    pub flags: u32,
    pub driver: [u8; 256],
}

// <asm-generic/ioctl.h> encoding, so the codes track the struct sizes above
// instead of being hard-coded for one pointer width.
const IOC_WRITE: c_ulong = 1;
const IOC_READ: c_ulong = 2;

const fn ioc(dir: c_ulong, ty: u8, nr: u8, size: usize) -> c_ulong {
    (dir << 30) | ((size as c_ulong) << 16) | ((ty as c_ulong) << 8) | nr as c_ulong
}

// <linux/usbdevice_fs.h>
pub const USBDEVFS_CONTROL: c_ulong = ioc(IOC_READ | IOC_WRITE, b'U', 0, size_of::<CtrlTransfer>());
pub const USBDEVFS_BULK: c_ulong = ioc(IOC_READ | IOC_WRITE, b'U', 2, size_of::<BulkTransfer>());
pub const USBDEVFS_CLAIMINTERFACE: c_ulong = ioc(IOC_READ, b'U', 15, size_of::<u32>());
pub const USBDEVFS_DISCONNECT_CLAIM: c_ulong =
    ioc(IOC_READ, b'U', 27, size_of::<DisconnectClaim>());

/// read(2). Unlike `std::io::Read`, the contract here is explicit: a
/// zero-length `buf` performs a real zero-length read, which FunctionFS ep0
/// uses as a status-stage acknowledgement.
pub fn read(fd: BorrowedFd<'_>, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buf` is valid for writes of `buf.len()` bytes.
    let n = unsafe { c::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    usize::try_from(n).map_err(|_| io::Error::last_os_error())
}

/// write(2). A zero-length `buf` performs a real zero-length write (FunctionFS
/// ep0 zero-length data stage / stall).
pub fn write(fd: BorrowedFd<'_>, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: `buf` is valid for reads of `buf.len()` bytes.
    let n = unsafe { c::write(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
    usize::try_from(n).map_err(|_| io::Error::last_os_error())
}

/// ioctl(2) with a pointer argument.
///
/// # Safety
/// `arg` must point to a live structure of the size and layout `request`
/// expects, and any buffers it references must be valid for the transfer;
/// the kernel reads and/or writes through them.
pub unsafe fn ioctl(fd: BorrowedFd<'_>, request: c_ulong, arg: *mut c_void) -> io::Result<c_int> {
    // SAFETY: upheld by the caller.
    let r = unsafe { c::ioctl(fd.as_raw_fd(), request, arg) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn ioctl_codes_match_64bit_kernel_headers() {
        assert_eq!(USBDEVFS_CONTROL, 0xC018_5500);
        assert_eq!(USBDEVFS_BULK, 0xC018_5502);
        assert_eq!(USBDEVFS_CLAIMINTERFACE, 0x8004_550F);
        assert_eq!(USBDEVFS_DISCONNECT_CLAIM, 0x8108_551B);
    }
}
