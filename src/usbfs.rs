//! Host-side USB access through the kernel's usbfs (`/dev/bus/usb/BBB/DDD`):
//! device lookup via sysfs, control and bulk transfers, interface claiming,
//! and endpoint discovery. No raw pointers cross this module's API.

use crate::sys;
use core::ffi::c_void;
use std::fs;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;

/// Bus/device numbers of an attached device, as used in `/dev/bus/usb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusAddr {
    pub bus: u32,
    pub dev: u32,
}

fn read_radix(path: &Path, radix: u32) -> Option<u32> {
    let s = fs::read_to_string(path).ok()?;
    u32::from_str_radix(s.trim(), radix).ok()
}

/// Find an attached device by VID/PID (first match), scanning sysfs.
pub fn find_device(vid: u16, pid: u16) -> Option<BusAddr> {
    for entry in fs::read_dir("/sys/bus/usb/devices").ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip interface nodes ("1-2:1.0") and "." / ".." style entries.
        if name.starts_with('.') || name.contains(':') {
            continue;
        }
        let p = entry.path();
        if read_radix(&p.join("idVendor"), 16) == Some(u32::from(vid))
            && read_radix(&p.join("idProduct"), 16) == Some(u32::from(pid))
        {
            if let (Some(bus), Some(dev)) = (
                read_radix(&p.join("busnum"), 10),
                read_radix(&p.join("devnum"), 10),
            ) {
                return Some(BusAddr { bus, dev });
            }
        }
    }
    None
}

/// The pair of bulk endpoints of one interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkEndpoints {
    pub in_addr: u8,
    pub out_addr: u8,
}

/// Find the first interface that has at least one bulk IN endpoint, returning
/// its bulk IN/OUT addresses, from a raw configuration-descriptor blob (the
/// bytes usbfs returns from a plain `read` of the device node).
///
/// Bounds-checked: a descriptor whose length runs past the blob ends the walk.
pub fn find_bulk_endpoints(desc: &[u8]) -> Option<BulkEndpoints> {
    const EP_DESC: u8 = 0x05;
    const XFER_BULK: u8 = 0x02;
    const IFACE_DESC: u8 = 0x04;

    let (mut in_addr, mut out_addr) = (0u8, 0u8);
    let mut i = 0usize;
    while i + 2 <= desc.len() {
        let blen = desc[i] as usize;
        let btype = desc[i + 1];
        if blen < 2 || i + blen > desc.len() {
            break;
        }
        // A new interface starts a fresh endpoint group; stop once the current
        // interface has yielded a bulk IN, so we report a single interface.
        if btype == IFACE_DESC && in_addr != 0 {
            break;
        }
        if btype == EP_DESC && blen >= 4 && desc[i + 3] & 0x03 == XFER_BULK {
            let addr = desc[i + 2];
            if addr & 0x80 != 0 {
                if in_addr == 0 {
                    in_addr = addr;
                }
            } else if out_addr == 0 {
                out_addr = addr;
            }
        }
        i += blen;
    }
    (in_addr != 0).then_some(BulkEndpoints { in_addr, out_addr })
}

/// An open usbfs device handle. The fd closes on drop.
#[derive(Debug)]
pub struct Device {
    fd: OwnedFd,
}

impl Device {
    /// Open `/dev/bus/usb/BBB/DDD` read-write.
    pub fn open(addr: BusAddr) -> io::Result<Self> {
        let path = format!("/dev/bus/usb/{:03}/{:03}", addr.bus, addr.dev);
        let file = fs::OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { fd: file.into() })
    }

    fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Read the device's active configuration descriptor blob (usbfs returns it
    /// from a plain read of the node).
    pub fn read_descriptors(&self, buf: &mut [u8]) -> io::Result<usize> {
        sys::read(self.fd(), buf)
    }

    /// Issue a control transfer. For IN transfers `data` receives the reply and
    /// the returned count is how many bytes arrived; for OUT transfers `data`
    /// is the payload to send. `timeout` of zero waits forever.
    pub fn control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
        timeout: u32,
    ) -> io::Result<usize> {
        let len = u16::try_from(data.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "control transfer too long")
        })?;
        let mut t = sys::CtrlTransfer {
            request_type,
            request,
            value,
            index,
            length: len,
            timeout,
            data: data.as_mut_ptr().cast::<c_void>(),
        };
        // SAFETY: `t` is a well-formed usbdevfs_ctrltransfer for USBDEVFS_CONTROL
        // and `data` is valid for `len` bytes for the duration of the call.
        let n = unsafe {
            sys::ioctl(
                self.fd(),
                sys::USBDEVFS_CONTROL,
                (&raw mut t).cast::<c_void>(),
            )?
        };
        Ok(n as usize)
    }

    /// Read from a bulk IN endpoint into `buf`, returning the byte count. A
    /// `timeout` of zero blocks until data arrives (so no data is discarded by
    /// an expiring transfer).
    pub fn bulk_read(&self, ep: u8, buf: &mut [u8], timeout: u32) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let mut t = sys::BulkTransfer {
            ep: u32::from(ep),
            len,
            timeout,
            data: buf.as_mut_ptr().cast::<c_void>(),
        };
        // SAFETY: `t` is a well-formed usbdevfs_bulktransfer for USBDEVFS_BULK
        // and `buf` is valid for `len` bytes for the duration of the call.
        let n =
            unsafe { sys::ioctl(self.fd(), sys::USBDEVFS_BULK, (&raw mut t).cast::<c_void>())? };
        Ok(n as usize)
    }

    /// Claim an interface, detaching any bound kernel driver first
    /// (DISCONNECT_CLAIM), falling back to a plain claim.
    pub fn claim_interface(&self, interface: u32) -> io::Result<Claim> {
        let mut dc = sys::DisconnectClaim {
            interface,
            flags: 0,
            driver: [0; 256],
        };
        // SAFETY: `dc` is a well-formed usbdevfs_disconnect_claim.
        let r = unsafe {
            sys::ioctl(
                self.fd(),
                sys::USBDEVFS_DISCONNECT_CLAIM,
                (&raw mut dc).cast::<c_void>(),
            )
        };
        if r.is_ok() {
            return Ok(Claim::DetachedDriver);
        }
        let mut ifnum = interface;
        // SAFETY: `ifnum` is a live u32, the argument CLAIMINTERFACE expects.
        unsafe {
            sys::ioctl(
                self.fd(),
                sys::USBDEVFS_CLAIMINTERFACE,
                (&raw mut ifnum).cast::<c_void>(),
            )?;
        }
        Ok(Claim::Claimed)
    }

    /// Raw fd, for callers that must poll it directly.
    pub fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

/// How [`Device::claim_interface`] obtained the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// A bound kernel driver was detached first.
    DetachedDriver,
    /// Claimed directly.
    Claimed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bulk_endpoints_of_first_interface() {
        // config(9) iface(9) ep OUT 0x01 bulk(7) ep IN 0x81 bulk(7)
        let desc = [
            9, 2, 32, 0, 1, 1, 0, 0x80, 250, // config
            9, 4, 0, 0, 2, 0xff, 0xff, 0x00, 1, // interface 0
            7, 5, 0x01, 0x02, 0x00, 0x02, 0, // ep OUT bulk
            7, 5, 0x81, 0x02, 0x00, 0x02, 0, // ep IN bulk
        ];
        assert_eq!(
            find_bulk_endpoints(&desc),
            Some(BulkEndpoints {
                in_addr: 0x81,
                out_addr: 0x01
            })
        );
    }

    #[test]
    fn rejects_descriptor_running_past_the_blob() {
        // An endpoint descriptor claiming length 7 with only 3 bytes left.
        let desc = [9, 4, 0, 0, 2, 0xff, 0xff, 0x00, 1, 7, 5, 0x81];
        assert_eq!(find_bulk_endpoints(&desc), None);
    }

    #[test]
    fn no_bulk_in_yields_none() {
        let desc = [9, 4, 0, 0, 0, 0xff, 0xff, 0x00, 1];
        assert_eq!(find_bulk_endpoints(&desc), None);
    }
}
