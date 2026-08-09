//! UDP socket setup, and an honest account of what the kernel granted.
//!
//! Asking for a socket buffer is not the same as getting one: macOS clamps
//! every request to `kern.ipc.maxsockbuf` (8 MiB by default) without saying
//! so, and Windows applies its own limits. A harness that printed the
//! requested size would be quoting its own command line back at itself, so
//! every number here is read back off the socket after the fact.
//!
//! The sender runs on Windows and the receiver on macOS, so both sides of
//! this file have to exist.

use core::fmt;
use std::io;
use std::net::{SocketAddr, UdpSocket};

use sys::{SO_RCVBUF, SO_SNDBUF};

/// A socket buffer as asked for and as granted.
#[derive(Clone, Copy, Debug)]
pub struct SocketBuffer {
    pub name: &'static str,
    pub requested: Option<usize>,
    pub granted: usize,
}

impl fmt::Display for SocketBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:<10} granted {:>9} B", self.name, self.granted)?;
        match self.requested {
            Some(requested) if requested != self.granted => {
                write!(f, "  (requested {requested} B, clamped)")
            }
            Some(requested) => write!(f, "  (requested {requested} B, honoured)"),
            None => write!(f, "  (system default)"),
        }
    }
}

pub fn bind(addr: SocketAddr) -> io::Result<UdpSocket> {
    UdpSocket::bind(addr)
}

pub fn send_buffer(socket: &UdpSocket, requested: Option<usize>) -> io::Result<SocketBuffer> {
    configure(socket, "SO_SNDBUF", SO_SNDBUF, requested)
}

pub fn recv_buffer(socket: &UdpSocket, requested: Option<usize>) -> io::Result<SocketBuffer> {
    configure(socket, "SO_RCVBUF", SO_RCVBUF, requested)
}

fn configure(
    socket: &UdpSocket,
    name: &'static str,
    option: i32,
    requested: Option<usize>,
) -> io::Result<SocketBuffer> {
    let handle = sys::handle(socket);
    if let Some(bytes) = requested {
        sys::set_size(handle, option, bytes)?;
    }
    Ok(SocketBuffer {
        name,
        requested,
        granted: sys::get_size(handle, option)?,
    })
}

#[cfg(unix)]
mod sys {
    use std::io;
    use std::net::UdpSocket;
    use std::os::fd::AsRawFd;

    pub type Handle = std::os::fd::RawFd;
    pub const SO_SNDBUF: i32 = libc::SO_SNDBUF;
    pub const SO_RCVBUF: i32 = libc::SO_RCVBUF;

    pub fn handle(socket: &UdpSocket) -> Handle {
        socket.as_raw_fd()
    }

    pub fn set_size(handle: Handle, option: i32, bytes: usize) -> io::Result<()> {
        let value = i32::try_from(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket buffer too large"))?;
        // SAFETY: `handle` is borrowed from a live `UdpSocket`, and `value` is
        // a live `c_int` whose address and length are exactly what
        // SOL_SOCKET/SO_*BUF reads. `setsockopt` copies out before returning.
        let rc = unsafe {
            libc::setsockopt(
                handle,
                libc::SOL_SOCKET,
                option,
                (&raw const value).cast(),
                size_of::<i32>() as libc::socklen_t,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn get_size(handle: Handle, option: i32) -> io::Result<usize> {
        let mut value: i32 = 0;
        let mut len = size_of::<i32>() as libc::socklen_t;
        // SAFETY: `handle` is borrowed from a live `UdpSocket`; `value` and
        // `len` are live locals of exactly the type and size SOL_SOCKET/SO_*BUF
        // writes back, and `len` states the buffer's true capacity.
        let rc = unsafe {
            libc::getsockopt(
                handle,
                libc::SOL_SOCKET,
                option,
                (&raw mut value).cast(),
                &raw mut len,
            )
        };
        if rc == 0 {
            Ok(value.max(0) as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::io;
    use std::net::UdpSocket;
    use std::os::windows::io::AsRawSocket;

    /// Winsock's `SOCKET`, which is a handle rather than a file descriptor.
    pub type Handle = usize;
    const SOL_SOCKET: i32 = 0xFFFF;
    pub const SO_SNDBUF: i32 = 0x1001;
    pub const SO_RCVBUF: i32 = 0x1002;

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn setsockopt(
            socket: Handle,
            level: i32,
            name: i32,
            value: *const core::ffi::c_char,
            len: i32,
        ) -> i32;
        fn getsockopt(
            socket: Handle,
            level: i32,
            name: i32,
            value: *mut core::ffi::c_char,
            len: *mut i32,
        ) -> i32;
    }

    pub fn handle(socket: &UdpSocket) -> Handle {
        socket.as_raw_socket() as Handle
    }

    pub fn set_size(handle: Handle, option: i32, bytes: usize) -> io::Result<()> {
        let value = i32::try_from(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket buffer too large"))?;
        // SAFETY: `handle` is borrowed from a live `UdpSocket`, and `value` is
        // a live `i32` whose address and length are what SO_*BUF expects.
        let rc = unsafe {
            setsockopt(
                handle,
                SOL_SOCKET,
                option,
                (&raw const value).cast(),
                size_of::<i32>() as i32,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn get_size(handle: Handle, option: i32) -> io::Result<usize> {
        let mut value: i32 = 0;
        let mut len = size_of::<i32>() as i32;
        // SAFETY: `handle` is borrowed from a live `UdpSocket`; `value` and
        // `len` are live locals of the exact type and size SO_*BUF writes back.
        let rc = unsafe {
            getsockopt(
                handle,
                SOL_SOCKET,
                option,
                (&raw mut value).cast(),
                &raw mut len,
            )
        };
        if rc == 0 {
            Ok(value.max(0) as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
