//! UDP socket setup, and an honest account of what the kernel granted.
//!
//! Asking for a socket buffer is not the same as getting one: macOS clamps
//! every request to `kern.ipc.maxsockbuf` (8 MiB by default) without saying
//! so. A harness that printed the requested size would be quoting its own
//! command line back at itself, so every number here is read back off the
//! socket after the fact.

use core::fmt;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};

use libc::c_int;

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
    configure(socket, "SO_SNDBUF", libc::SO_SNDBUF, requested)
}

pub fn recv_buffer(socket: &UdpSocket, requested: Option<usize>) -> io::Result<SocketBuffer> {
    configure(socket, "SO_RCVBUF", libc::SO_RCVBUF, requested)
}

fn configure(
    socket: &UdpSocket,
    name: &'static str,
    option: c_int,
    requested: Option<usize>,
) -> io::Result<SocketBuffer> {
    let fd = socket.as_raw_fd();
    if let Some(bytes) = requested {
        set_size(fd, option, bytes)?;
    }
    Ok(SocketBuffer {
        name,
        requested,
        granted: get_size(fd, option)?,
    })
}

fn set_size(fd: RawFd, option: c_int, bytes: usize) -> io::Result<()> {
    let value = c_int::try_from(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket buffer too large"))?;
    // SAFETY: `fd` is borrowed from a live `UdpSocket`, and `value` is a live
    // `c_int` whose address and length are exactly what SOL_SOCKET/SO_*BUF
    // reads. `setsockopt` copies out of the pointer before returning.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&raw const value).cast(),
            size_of::<c_int>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn get_size(fd: RawFd, option: c_int) -> io::Result<usize> {
    let mut value: c_int = 0;
    let mut len = size_of::<c_int>() as libc::socklen_t;
    // SAFETY: `fd` is borrowed from a live `UdpSocket`; `value` and `len` are
    // live locals of exactly the type and size SOL_SOCKET/SO_*BUF writes back,
    // and `len` tells the kernel the buffer's true capacity.
    let rc = unsafe {
        libc::getsockopt(
            fd,
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
