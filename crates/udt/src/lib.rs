//! Minimal receive-only UDT4 client.
//!
//! The aircraft serves its camera streams over UDT (a reliable stream protocol
//! on UDP) on the network its USB API port brings up: the FPV camera at
//! `192.168.42.2:40003` sends raw H.264. This wraps the vendored UDT 4.11
//! library from DJI's Onboard SDK through a small C shim (`shim.cpp`).
//!
//! The client only takes part in UDT's own handshake, ACK and keep-alive
//! traffic; it sends no aircraft commands.

use std::ffi::{CStr, c_char, c_int};
use std::net::SocketAddrV4;
use std::time::Duration;

unsafe extern "C" {
    fn udt_shim_connect(
        ipv4: u32,
        port: u16,
        recv_timeout_ms: c_int,
        err: *mut c_char,
        err_len: c_int,
    ) -> c_int;
    fn udt_shim_recv(
        sock: c_int,
        buf: *mut c_char,
        len: c_int,
        err: *mut c_char,
        err_len: c_int,
    ) -> c_int;
    fn udt_shim_close(sock: c_int);
}

const ERR_LEN: usize = 256;

fn message(err: &[u8; ERR_LEN]) -> String {
    CStr::from_bytes_until_nul(err)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown UDT error".into())
}

/// A connected UDT stream socket, closed on drop.
pub struct Stream {
    sock: c_int,
}

impl Stream {
    /// Connect to `addr`. Each [`recv`](Self::recv) then waits at most
    /// `recv_timeout`. UDT's own connect timeout is a few seconds.
    pub fn connect(addr: SocketAddrV4, recv_timeout: Duration) -> Result<Self, String> {
        let mut err = [0u8; ERR_LEN];
        let timeout = c_int::try_from(recv_timeout.as_millis()).unwrap_or(c_int::MAX);
        // SAFETY: `err` is a writable buffer of the stated length; the shim
        // NUL-terminates what it writes.
        let sock = unsafe {
            udt_shim_connect(
                u32::from(*addr.ip()),
                addr.port(),
                timeout,
                err.as_mut_ptr().cast(),
                ERR_LEN as c_int,
            )
        };
        if sock < 0 {
            Err(message(&err))
        } else {
            Ok(Self { sock })
        }
    }

    /// Receive into `buf`: `Ok(0)` means the timeout passed with no data,
    /// `Err` that the connection is gone.
    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let mut err = [0u8; ERR_LEN];
        let len = c_int::try_from(buf.len()).unwrap_or(c_int::MAX);
        // SAFETY: `buf` is valid for `len` bytes and `err` for ERR_LEN; the
        // socket is open for the life of `self`.
        let n = unsafe {
            udt_shim_recv(
                self.sock,
                buf.as_mut_ptr().cast(),
                len,
                err.as_mut_ptr().cast(),
                ERR_LEN as c_int,
            )
        };
        usize::try_from(n).map_err(|_| message(&err))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the socket came from a successful connect and is closed once.
        unsafe { udt_shim_close(self.sock) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// Nothing listens on this loopback port, so the connect fails cleanly
    /// (UDT times out its handshake) instead of hanging.
    #[test]
    fn connect_to_nothing_fails() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9);
        let err = Stream::connect(addr, Duration::from_millis(100)).err();
        assert!(err.is_some_and(|e| !e.is_empty()));
    }
}
