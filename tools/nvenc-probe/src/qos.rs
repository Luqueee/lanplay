//! Asking Windows to treat the media socket as interactive video.
//!
//! W2 established that the link's cadence problem is not bandwidth: between
//! 20 and 50 Mbps the arrival p99 does not move. What has not been tested is
//! *when* the datagrams leave, and that is a question about service class.
//! RFC 8325 maps real-time interactive traffic to DSCP CS4 and, on 802.11, to
//! user priority 4 - the video access category, which has a shorter contention
//! window than best effort.
//!
//! Two ways to ask, because they fail differently:
//!
//! * `IP_TOS` through `setsockopt`. Windows ignores it by default: the TCP/IP
//!   stack strips application-set TOS unless `DisableUserTOSSetting` is
//!   cleared in the registry. The call still succeeds, which is why this
//!   module reports what it asked for rather than claiming what happened.
//! * qWAVE. `QOSAddSocketToFlow` with `QOSTrafficTypeAudioVideo` is the
//!   supported path, and it marks both DSCP and the 802.11 user priority.
//!
//! Neither is assumed to work. The experiment compares them against
//! best-effort on the numbers that matter, and a marking the router or the
//! driver ignores will show up as no difference at all.

#![cfg(windows)]

use std::net::UdpSocket;
use std::os::windows::io::AsRawSocket;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::QoS::{
    QOS_NON_ADAPTIVE_FLOW, QOS_TRAFFIC_TYPE, QOS_VERSION, QOSAddSocketToFlow, QOSCreateHandle,
    QOSTrafficTypeAudioVideo, QOSTrafficTypeControl,
};
use windows::Win32::Networking::WinSock::SOCKET;

/// What service class to request for the media socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ServiceClass {
    /// Ask for nothing. The baseline every other arm is compared against.
    BestEffort,
    /// DSCP only, through `IP_TOS`. Expected to be ignored on Windows without
    /// a registry change; kept because "we asked and nothing happened" is
    /// itself a result worth having on the record.
    Dscp,
    /// qWAVE's audio/video traffic type: the documented path, and the one
    /// that also sets the 802.11 user priority.
    AudioVideo,
    /// qWAVE's control traffic type, one class above video. Included because
    /// if class changes the cadence at all, the direction and size of the
    /// change across two classes says more than one class alone.
    Control,
}

/// A live qWAVE flow. Dropping the handle removes the flow, so it is held for
/// as long as the socket is sending.
///
/// The handle crosses to whichever thread owns the sender. qWAVE handles are
/// process-wide and documented as usable from any thread; the raw pointer
/// inside `HANDLE` is what makes the compiler ask.
pub struct Marking {
    handle: Option<HANDLE>,
    pub requested: ServiceClass,
    /// What the operating system said, rather than what was asked for.
    pub applied: String,
}

// SAFETY: a qWAVE handle belongs to the process, not to a thread, and this
// one is only ever read (to close it) after the sender that owns it is done.
unsafe impl Send for Marking {}

impl Marking {
    pub fn apply(socket: &UdpSocket, class: ServiceClass, target: std::net::SocketAddr) -> Marking {
        match class {
            ServiceClass::BestEffort => Marking {
                handle: None,
                requested: class,
                applied: "nothing requested".into(),
            },
            ServiceClass::Dscp => Marking {
                handle: None,
                requested: class,
                applied: set_tos(socket),
            },
            ServiceClass::AudioVideo => add_flow(socket, class, QOSTrafficTypeAudioVideo, target),
            ServiceClass::Control => add_flow(socket, class, QOSTrafficTypeControl, target),
        }
    }
}

impl Drop for Marking {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // Closing the qWAVE handle removes every flow on it. Errors here
            // cannot be acted on: the process is finishing.
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
        }
    }
}

/// DSCP CS4, the RFC 8325 class for real-time interactive traffic.
///
/// The TOS byte carries DSCP in its top six bits, so the value is the DSCP
/// codepoint shifted left twice.
const CS4_TOS: u32 = 32 << 2;

fn set_tos(socket: &UdpSocket) -> String {
    const IPPROTO_IP: i32 = 0;
    const IP_TOS: i32 = 3;

    let value = CS4_TOS;
    // SAFETY: the socket outlives the call and the option value is a live u32
    // of the length declared.
    let result = unsafe {
        windows::Win32::Networking::WinSock::setsockopt(
            SOCKET(socket.as_raw_socket() as usize),
            IPPROTO_IP,
            IP_TOS,
            Some(core::slice::from_raw_parts(
                (&raw const value).cast::<u8>(),
                size_of::<u32>(),
            )),
        )
    };
    if result == 0 {
        // Deliberately not "applied": the call succeeding says nothing about
        // whether the stack will keep the bits.
        "IP_TOS CS4 accepted by setsockopt (Windows may still strip it)".into()
    } else {
        let error = unsafe { windows::Win32::Networking::WinSock::WSAGetLastError() };
        format!("IP_TOS refused, WSA error {}", error.0)
    }
}

/// Adds the socket to a qWAVE flow of the given class, and asks for nothing
/// else.
///
/// `QOSSetFlow` is what would request rate shaping, and it is deliberately
/// never called: an arm asking for both a class and a rate while another
/// asks for neither would be two variables wearing one name.
fn add_flow(
    socket: &UdpSocket,
    class: ServiceClass,
    traffic: QOS_TRAFFIC_TYPE,
    target: std::net::SocketAddr,
) -> Marking {
    let version = QOS_VERSION {
        MajorVersion: 1,
        MinorVersion: 0,
    };
    let mut handle = HANDLE::default();
    // SAFETY: both arguments are live locals for the duration of the call.
    if !unsafe { QOSCreateHandle(&version, &raw mut handle) }.as_bool() {
        return Marking {
            handle: None,
            requested: class,
            applied: format!("QOSCreateHandle failed: {}", last_error()),
        };
    }

    let address = socket_address(target);
    let mut flow_id = 0u32;
    // SAFETY: the handle and socket are live, the address outlives the call,
    // and `flow_id` is a live out-parameter. A null `destaddr` is legal for a
    // connected socket, but passing it explicitly is what lets qWAVE find the
    // path and therefore the wireless link.
    let added = unsafe {
        QOSAddSocketToFlow(
            handle,
            SOCKET(socket.as_raw_socket() as usize),
            Some(address.as_ptr()),
            traffic,
            Some(QOS_NON_ADAPTIVE_FLOW),
            &raw mut flow_id,
        )
    };
    if added.as_bool() {
        Marking {
            handle: Some(handle),
            requested: class,
            applied: format!("qWAVE flow {flow_id} as {traffic:?}, no rate shaping requested"),
        }
    } else {
        let reason = last_error();
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
        Marking {
            handle: None,
            requested: class,
            applied: format!("QOSAddSocketToFlow failed: {reason}"),
        }
    }
}

fn last_error() -> String {
    format!("{:?}", unsafe {
        windows::Win32::Foundation::GetLastError()
    })
}

/// The destination as a `SOCKADDR`, kept alive by the caller.
fn socket_address(target: std::net::SocketAddr) -> SocketAddress {
    use windows::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, IN_ADDR, IN_ADDR_0, SOCKADDR_IN, SOCKADDR_IN6,
    };

    match target {
        std::net::SocketAddr::V4(v4) => {
            let sockaddr = SOCKADDR_IN {
                sin_family: AF_INET,
                sin_port: v4.port().to_be(),
                sin_addr: IN_ADDR {
                    S_un: IN_ADDR_0 {
                        S_addr: u32::from_ne_bytes(v4.ip().octets()),
                    },
                },
                sin_zero: [0; 8],
            };
            SocketAddress::V4(sockaddr)
        }
        std::net::SocketAddr::V6(v6) => {
            let mut sockaddr = SOCKADDR_IN6 {
                sin6_family: AF_INET6,
                sin6_port: v6.port().to_be(),
                ..Default::default()
            };
            sockaddr.sin6_addr.u.Byte = v6.ip().octets();
            SocketAddress::V6(sockaddr)
        }
    }
}

pub enum SocketAddress {
    V4(windows::Win32::Networking::WinSock::SOCKADDR_IN),
    V6(windows::Win32::Networking::WinSock::SOCKADDR_IN6),
}

impl SocketAddress {
    fn as_ptr(&self) -> *const windows::Win32::Networking::WinSock::SOCKADDR {
        match self {
            SocketAddress::V4(v4) => {
                (v4 as *const windows::Win32::Networking::WinSock::SOCKADDR_IN).cast()
            }
            SocketAddress::V6(v6) => {
                (v6 as *const windows::Win32::Networking::WinSock::SOCKADDR_IN6).cast()
            }
        }
    }
}
