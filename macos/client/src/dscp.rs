//! What service class actually arrived.
//!
//! A QoS experiment that only records what the sender asked for measures the
//! sender's intent, not the network's behaviour. Windows strips
//! application-set TOS by default, qWAVE marks only when the path supports
//! it, and an access point is free to ignore either. The one place the answer
//! exists is in the IP header of the datagrams that turn up here.
//!
//! `IP_RECVTOS` asks the kernel to attach the received TOS byte as ancillary
//! data, which means reading through `recvmsg` rather than `recv`. The cost is
//! one extra control-message buffer per call and no allocation.
//!
//! DSCP is the top six bits of that byte, so `tos >> 2`. The values worth
//! recognising:
//!
//! ```text
//!  0  CS0   best effort
//! 32  CS4   RFC 8325 real-time interactive
//! 40  AF41  what qWAVE's audio/video traffic type marks
//! 56  CS7   what qWAVE's control traffic type marks
//! ```

use std::net::UdpSocket;
use std::os::fd::AsRawFd;

/// How many DSCP codepoints can be seen before the rest are lumped together.
/// A stream should use exactly one; more than a handful means something on
/// the path is rewriting them per packet, which is itself the finding.
const TRACKED: usize = 8;

/// Counts of the DSCP codepoints seen on arriving datagrams.
///
/// A fixed array rather than a map: this is touched once per datagram on the
/// receive thread, and at 4000 packets a second an allocation there would be
/// measuring the instrument.
#[derive(Clone, Copy, Debug, Default)]
pub struct Observed {
    codepoints: [(u8, u64); TRACKED],
    used: usize,
    /// Datagrams whose TOS the kernel did not report.
    unknown: u64,
}

impl Observed {
    pub fn record(&mut self, dscp: Option<u8>) {
        let Some(dscp) = dscp else {
            self.unknown += 1;
            return;
        };
        for slot in &mut self.codepoints[..self.used] {
            if slot.0 == dscp {
                slot.1 += 1;
                return;
            }
        }
        if self.used < TRACKED {
            self.codepoints[self.used] = (dscp, 1);
            self.used += 1;
        } else {
            self.unknown += 1;
        }
    }

    /// The codepoint most datagrams carried, with its share.
    pub fn dominant(&self) -> Option<(u8, f64)> {
        let total: u64 = self.codepoints[..self.used].iter().map(|(_, n)| n).sum();
        if total == 0 {
            return None;
        }
        self.codepoints[..self.used]
            .iter()
            .max_by_key(|(_, n)| *n)
            .map(|(dscp, n)| (*dscp, *n as f64 * 100.0 / total as f64))
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0 && self.unknown == 0
    }
}

impl core::fmt::Display for Observed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_empty() {
            return f.write_str("nothing observed");
        }
        let mut first = true;
        for (dscp, count) in &self.codepoints[..self.used] {
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            write!(f, "DSCP {dscp} ({}) x{count}", name(*dscp))?;
        }
        if self.unknown > 0 {
            write!(f, ", {} without a reported TOS", self.unknown)?;
        }
        Ok(())
    }
}

fn name(dscp: u8) -> &'static str {
    match dscp {
        0 => "CS0/best effort",
        32 => "CS4",
        34 => "AF41",
        40 => "CS5/qWAVE audio-video",
        46 => "EF",
        48 => "CS6",
        56 => "CS7/qWAVE control",
        _ => "other",
    }
}

/// Asks the kernel to report the TOS byte of every datagram.
///
/// Returns whether it agreed. A receiver that cannot see the byte reports
/// that rather than reporting zero, which would look like best effort.
pub fn request_tos(socket: &UdpSocket) -> bool {
    let enable: libc::c_int = 1;
    // SAFETY: the socket outlives the call and the option value is a live
    // c_int of the length declared.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_RECVTOS,
            (&raw const enable).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    result == 0
}

/// One datagram plus the DSCP the kernel saw on it.
///
/// Replaces `UdpSocket::recv` on the media path. The ancillary buffer is a
/// caller-owned array so the hot path allocates nothing.
pub fn recv_with_dscp(
    socket: &UdpSocket,
    buffer: &mut [u8],
) -> std::io::Result<(usize, Option<u8>)> {
    let mut control = [0u8; 64];
    let mut iov = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    let mut header: libc::msghdr = unsafe { core::mem::zeroed() };
    header.msg_iov = &raw mut iov;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = control.len() as libc::socklen_t;

    // SAFETY: every pointer in the header addresses a live local that
    // outlives the call, and the lengths are those buffers' own.
    let received = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut header, 0) };
    if received < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: the header was just filled by the kernel, so its control
    // messages are well formed and within the buffer that was handed over.
    let mut dscp = None;
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&raw const header);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::IPPROTO_IP && (*cmsg).cmsg_type == libc::IP_RECVTOS {
                dscp = Some(*libc::CMSG_DATA(cmsg) >> 2);
                break;
            }
            cmsg = libc::CMSG_NXTHDR(&raw const header, cmsg);
        }
    }
    Ok((received as usize, dscp))
}
