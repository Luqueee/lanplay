//! The one hop a phase request makes on this host, from the process that hears
//! it to the thing that can act on it.
//!
//! The receiver negotiates a decoder with this process, so its request arrives
//! here, and what it asks for is the largest term in the whole pipeline's
//! latency: the wait between a frame being decoded and the display next being
//! willing to show it, 5.08 ms at p50 against 1.04 ms of decode, with 600 s soak
//! percentiles of 4.4, 7.8 and 8.4 ms against the 4.17, 7.9 and 8.25 predicted
//! for a phase uniformly distributed inside one 8.33 ms period. It is phase and
//! not work, and it is larger than decode, encode, capture and the network put
//! together.
//!
//! Two other levers were built and both measure neutral. Delaying the capture
//! tick is neutral by derivation: it moves when a frame is ready and never when
//! its content was drawn, so it buys nothing until it crosses a display tick and
//! then it costs a whole period. Delaying the producer's draw is neutral by
//! experiment: a 3.00 ms shift, confirmed applied by the producer during a live
//! run, left a 50-sample phase trace on the viewer inside 3.61 to 4.69 ms, the
//! largest movement between samples being 0.374 ms. Desktop Duplication follows
//! the compositor rather than the program drawing into it, so a draw moved
//! inside a composition interval is composited at the same virtual-display
//! vblank and leaves this host at the same instant. That measurement is why the
//! producer has no phase mechanism left: a lever that looks wired and moves
//! nothing is believed by the next person to read the code.
//!
//! What the composition cadence follows is the vblank of the virtual display,
//! and this project owns the driver that declares it. Holding
//! `IddCxSwapChainFinishedProcessingFrame` back once by d moves that cadence by
//! d, so a request goes from here into the driver in `windows/idd-lab` and
//! nowhere else.
//!
//! The path is found and never named. An indirect display device is created at
//! runtime by a controller process, so its instance number depends on the order
//! the devices came up in and a literal path works until something restarts. The
//! driver publishes a device interface class instead, and the configuration
//! manager is asked which paths are present for it right now.
//!
//! Advisory in both directions, as a phase request is everywhere else in this
//! project. Four bytes go down, nothing comes back, and the delay applies to the
//! next frame alone. A host whose driver predates the interface finds nothing to
//! open and says so in the report, because a zero that means "the driver was not
//! there" must not read like a receiver that never asked. Nothing travels
//! alongside the delay, where the loopback datagram this replaced also carried a
//! frame rate: that field existed because two independently started processes
//! could disagree about the period a correction was computed against, and a
//! driver folding against the vblank period it declares itself cannot.

#![cfg(windows)]

use core::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lanplay_telemetry::Nanos;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CONFIGRET, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_DATA, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{FILE_DEVICE_UNKNOWN, METHOD_BUFFERED};
use windows::core::{GUID, PCWSTR};

/// The interface class the display driver publishes for phase requests.
pub const PHASE_INTERFACE: GUID = GUID::from_u128(0x60EBFC7A_1723_41F3_9CC6_19EBF0DEBED2);

/// `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_WRITE_DATA)`.
/// The macro is C, so the arithmetic it stands for is written out here; the
/// driver computes the same number from the same four values.
const IOCTL_PHASE_SHIFT: u32 =
    (FILE_DEVICE_UNKNOWN << 16) | (FILE_WRITE_DATA.0 << 14) | (0x800 << 2) | METHOD_BUFFERED;

/// How many times the enumeration is sized and refilled before giving up. A
/// device arriving between the two calls makes the second one report a short
/// buffer rather than truncating, and retrying forever would hang a run behind
/// a machine busy creating displays.
const RESIZE_ATTEMPTS: u32 = 4;

/// The sending half: one device, or the reason there is none, and a record of
/// what went down it.
///
/// Shared by reference so the thread reading the control connection and the
/// thread writing the report see the same counters.
pub struct Relay {
    device: Result<Device, Error>,
    /// Where requests went, already in the words the report uses: the resolved
    /// interface path, or the absence that stood in for it.
    destination: String,
    sent: AtomicU64,
    nanos: AtomicU64,
    errors: AtomicU64,
    said: AtomicBool,
}

impl Relay {
    /// Resolves the interface and opens it now, whether or not a receiver ever
    /// asks, so a run has one place to count requests against and an absent
    /// driver is known at the start of a soak rather than in the report at the
    /// end of it.
    ///
    /// Never fails. Media is what this process exists to produce, and a phase
    /// correction is advisory: a missing driver is a line in the report, not a
    /// reason to refuse a run. Nothing is said here either, because the
    /// destination is what the caller prints and it carries the reason with it.
    pub fn open() -> Relay {
        let device = Device::open(PHASE_INTERFACE);
        let destination = match &device {
            Ok(device) => device.path.clone(),
            Err(error) => format!("nothing ({error})"),
        };
        Relay {
            device,
            destination,
            sent: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            said: AtomicBool::new(false),
        }
    }

    /// Asks the driver to hold its next frame back by `delay_nanos`, counting
    /// whatever happened.
    ///
    /// A failure is recorded and swallowed. Nothing downstream of a phase
    /// correction depends on it, and a driver that is not loaded - or an older
    /// build of one that never declared the interface - must not be able to end
    /// a stream.
    pub fn send(&self, delay_nanos: u32) {
        self.sent.fetch_add(1, Ordering::Relaxed);
        self.nanos
            .fetch_add(u64::from(delay_nanos), Ordering::Relaxed);
        // A string, and only on the failure path: the two ways this fails read
        // nothing alike, and one line that says which is worth an allocation a
        // healthy run never makes.
        let failure = match &self.device {
            Ok(device) => device
                .shift(delay_nanos)
                .err()
                .map(|status| format!("the driver refused the phase request: {status}")),
            Err(absent) => Some(absent.to_string()),
        };
        if let Some(failure) = failure {
            self.errors.fetch_add(1, Ordering::Relaxed);
            // Once. A converged loop asks every batch, so a driver that refuses
            // every request would otherwise fill the log of a ten-minute soak
            // with the same sentence, and the count in the report is what says
            // how many there were.
            if !self.said.swap(true, Ordering::Relaxed) {
                eprintln!("phase relay: {failure}");
            }
        }
    }

    /// Where requests are going, for the run's banner.
    pub fn destination(&self) -> &str {
        &self.destination
    }

    pub fn counts(&self) -> RelayCounts {
        RelayCounts {
            destination: self.destination.clone(),
            sent: self.sent.load(Ordering::Relaxed),
            requested: Nanos(self.nanos.load(Ordering::Relaxed)),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

/// What one run relayed, for the encoder host's report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayCounts {
    pub destination: String,
    pub sent: u64,
    /// Everything asked for, before the driver folds any of it into a period.
    pub requested: Nanos,
    pub errors: u64,
}

impl fmt::Display for RelayCounts {
    /// Every run, zeros included. Printed only when something happened, the
    /// line's absence would not distinguish a viewer that never asked from a
    /// host that dropped the request.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "phase requests: {} relayed to {}, {} asked for, {} not sent",
            self.sent, self.destination, self.requested, self.errors
        )
    }
}

/// The open interface, held for the life of the run.
///
/// Opened once rather than per request: a request arrives at the end of every
/// measurement batch, and paying an open and a close for each one would put two
/// kernel transitions in the way of a correction that has to be believable to
/// the microsecond.
struct Device {
    handle: HANDLE,
    /// The path it was opened from, kept for the report: the instance number in
    /// it is the only thing that says which device a run was steering.
    path: String,
}

// SAFETY: a file handle belongs to the process rather than to a thread. This one
// is written to by the control reader alone and read by nothing else; the raw
// pointer inside `HANDLE` is what makes the compiler ask.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    fn open(interface: GUID) -> Result<Device, Error> {
        let list = interface_list(interface)?;
        let (path, present) = first_interface(&list)?;
        let text = String::from_utf16_lossy(&path[..path.len() - 1]);
        if present > 1 {
            // Two virtual displays mean two vblanks, and the one this steers
            // may not be the one being captured.
            eprintln!("phase relay: {present} devices expose the phase interface; steering {text}");
        }
        // Write access and no more, because the IOCTL asks for `FILE_WRITE_DATA`
        // and nothing is ever read back. Sharing is granted so that a second
        // process holding the interface open - the driver's own sender, used to
        // apply a delay by hand - does not make a run fail to open it.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(|error| Error::Open {
            path: text.clone(),
            error,
        })?;
        Ok(Device { handle, path: text })
    }

    /// Four bytes of little-endian nanoseconds, no output buffer. Any other
    /// shape is rejected by the driver, which is the whole reason the size is
    /// fixed rather than negotiated.
    fn shift(&self, delay_nanos: u32) -> windows::core::Result<()> {
        let delay = delay_nanos.to_le_bytes();
        // SAFETY: a live handle, a buffer that outlives the call, and no output
        // buffer to be written into.
        unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_PHASE_SHIFT,
                Some(delay.as_ptr().cast()),
                delay.len() as u32,
                None,
                0,
                None,
                None,
            )
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: opened by `open`, closed here and nowhere else.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Every interface path a present device exposes for `interface`, as the
/// multi-string the configuration manager hands back.
///
/// `PRESENT` and not every registered interface: an installed driver with no
/// running device still has its class registered, and a path for a device that
/// is not there opens with an error that names the wrong problem.
fn interface_list(interface: GUID) -> Result<Vec<u16>, Error> {
    let mut last = CR_SUCCESS;
    for _ in 0..RESIZE_ATTEMPTS {
        let mut units = 0u32;
        // SAFETY: both calls write only through the pointers given, and the
        // buffer is sized by the first of them.
        let sized = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut units,
                &interface,
                PCWSTR::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if sized != CR_SUCCESS {
            return Err(Error::Enumerate(sized));
        }
        let mut list = vec![0u16; units as usize];
        last = unsafe {
            CM_Get_Device_Interface_ListW(
                &interface,
                PCWSTR::null(),
                &mut list,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        match last {
            CR_SUCCESS => return Ok(list),
            // A device arrived between the sizing and the fill. Size again.
            CR_BUFFER_SMALL => continue,
            other => return Err(Error::Enumerate(other)),
        }
    }
    Err(Error::Enumerate(last))
}

/// The first path in a multi-string list, terminator included so it can be
/// handed to Win32 as it stands, and how many paths the list held.
///
/// An empty list is the shape an absent driver produces, and it has to be told
/// apart from an enumeration that failed: nothing is wrong with the machine, the
/// driver simply is not there.
fn first_interface(list: &[u16]) -> Result<(&[u16], usize), Error> {
    let mut paths = list
        .split_inclusive(|unit| *unit == 0)
        // A multi-string ends with an empty string, and a buffer whose last
        // path has no terminator at all is not one this can hand to Win32.
        .take_while(|path| path.len() > 1 && path.last() == Some(&0));
    let first = paths.next().ok_or(Error::InterfaceAbsent)?;
    Ok((first, 1 + paths.count()))
}

/// Why the interface could not be reached. A refusal by an interface that is
/// there is not here: that carries the driver's own status and is reported with
/// it, because "invalid parameter" and "device removed" are different mornings'
/// work.
#[derive(Debug)]
pub enum Error {
    /// No present device exposes the interface. The controller that creates the
    /// virtual display is not running, or the driver behind it is a build from
    /// before the interface existed.
    InterfaceAbsent,
    /// The configuration manager would not enumerate the class at all.
    Enumerate(CONFIGRET),
    /// A path that was present would not open.
    Open {
        path: String,
        error: windows::core::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InterfaceAbsent => write!(
                f,
                "no present device exposes the phase interface {PHASE_INTERFACE:?}: \
                 the IDD-LAB display driver is not loaded"
            ),
            Error::Enumerate(code) => write!(
                f,
                "enumerating the phase interface {PHASE_INTERFACE:?} failed with CR {}",
                code.0
            ),
            Error::Open { path, error } => write!(f, "opening {path}: {error}"),
        }
    }
}

impl core::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path as the configuration manager returns it: NUL-terminated, and the
    /// whole list terminated by an empty string.
    fn multi_string(paths: &[&str]) -> Vec<u16> {
        let mut list: Vec<u16> = Vec::new();
        for path in paths {
            list.extend(path.encode_utf16());
            list.push(0);
        }
        list.push(0);
        list
    }

    /// The number the driver's `CTL_CODE` has to agree with, written out so a
    /// change to either side fails here rather than in a run that silently
    /// stops moving the phase.
    #[test]
    fn the_control_code_is_the_one_in_the_contract() {
        assert_eq!(IOCTL_PHASE_SHIFT, 0x0022_A000);
    }

    #[test]
    fn a_path_comes_back_whole_and_ready_for_win32() {
        let list = multi_string(&[r"\\?\root#display#0002#{60ebfc7a-1723-41f3-9cc6-19ebf0debed2}"]);
        let (path, present) = first_interface(&list).unwrap();

        assert_eq!(present, 1);
        assert_eq!(path.last(), Some(&0), "the terminator Win32 needs was cut");
        assert_eq!(
            String::from_utf16_lossy(&path[..path.len() - 1]),
            r"\\?\root#display#0002#{60ebfc7a-1723-41f3-9cc6-19ebf0debed2}"
        );
    }

    /// A driver that is not loaded exposes nothing, and that has to arrive as
    /// the absence it is rather than as an empty string a run would try to open.
    #[test]
    fn an_empty_list_is_the_absent_interface_and_not_a_path() {
        assert!(matches!(
            first_interface(&multi_string(&[])),
            Err(Error::InterfaceAbsent)
        ));
        assert!(matches!(first_interface(&[]), Err(Error::InterfaceAbsent)));
        // A buffer whose path was truncated before its terminator: handing that
        // to `CreateFileW` would read past the end of it.
        assert!(matches!(
            first_interface(&"path".encode_utf16().collect::<Vec<u16>>()),
            Err(Error::InterfaceAbsent)
        ));
    }

    /// Two displays are a real state of this machine, and the run has to be
    /// able to say which one it steered.
    #[test]
    fn a_second_device_is_counted_and_the_first_is_the_one_used() {
        let list = multi_string(&[
            r"\\?\root#display#0002#{guid}",
            r"\\?\root#display#0003#{guid}",
        ]);
        let (path, present) = first_interface(&list).unwrap();

        assert_eq!(present, 2);
        assert_eq!(
            String::from_utf16_lossy(&path[..path.len() - 1]),
            r"\\?\root#display#0002#{guid}"
        );
    }

    /// The absent path against the real configuration manager. No driver
    /// exposes this class, on this machine or any other, so the answer is the
    /// same everywhere and it is the answer an unloaded IDD-LAB driver gets.
    #[test]
    fn an_interface_nothing_exposes_names_itself_absent() {
        let unused = GUID::from_u128(0x0DEAD0DE_0000_4000_8000_000000000001);
        let opened = Device::open(unused);

        match opened {
            Err(Error::InterfaceAbsent) => {}
            Err(other) => panic!("enumeration failed instead of reporting absence: {other}"),
            Ok(device) => panic!("something answered on {}", device.path),
        }
    }

    /// Whatever this machine has, opening a relay produces a relay. A run
    /// refused over a correction nothing depends on would be the worst outcome
    /// available here, and the destination is what a report needs either way:
    /// with `--nocapture` this test also says which of the two the machine it
    /// ran on is in.
    #[test]
    fn a_relay_opens_whatever_the_machine_has() {
        let relay = Relay::open();
        println!("phase interface: {}", relay.destination());

        let counts = relay.counts();
        assert!(
            !counts.destination.is_empty(),
            "a report with no destination"
        );
        assert_eq!(counts.sent, 0, "a relay counted a request nobody made");
        assert_eq!(
            counts.errors, 0,
            "a relay counted a failure before any send"
        );
        assert_eq!(counts.requested, Nanos::ZERO);
        assert!(
            counts.destination.starts_with(r"\\?\") || counts.destination.starts_with("nothing ("),
            "{}",
            counts.destination
        );
    }

    /// What a run reports has to agree with what the machine did, on a host
    /// carrying the driver and on one without: the request is counted either
    /// way, and the failure count is the only thing that tells the two apart. A
    /// real request goes down a real interface when there is one, because a
    /// one-off shift of under a period on a virtual display is what this
    /// mechanism does for a living and the next frame absorbs it. An interface
    /// that is there and refuses four legal bytes fails here, which is the point.
    #[test]
    fn a_request_is_counted_whether_or_not_there_was_a_driver() {
        let relay = Relay::open();
        let reachable = relay.device.is_ok();
        relay.send(1_000_000);

        let counts = relay.counts();
        assert_eq!(counts.sent, 1);
        assert_eq!(counts.requested, Nanos(1_000_000));
        assert_eq!(counts.errors, u64::from(!reachable), "{counts}");
    }

    /// A run whose driver was missing must not report the same thing as a run
    /// whose receiver never asked, which is the whole reason the destination is
    /// in the line.
    #[test]
    fn the_report_line_says_where_the_requests_went() {
        let relayed = RelayCounts {
            destination: r"\\?\root#display#0002#{60ebfc7a-1723-41f3-9cc6-19ebf0debed2}".into(),
            sent: 12,
            requested: Nanos(45_832_000),
            errors: 0,
        };
        assert_eq!(
            relayed.to_string(),
            r"phase requests: 12 relayed to \\?\root#display#0002#{60ebfc7a-1723-41f3-9cc6-19ebf0debed2}, 45.83 ms asked for, 0 not sent"
        );

        let absent = RelayCounts {
            destination: format!("nothing ({})", Error::InterfaceAbsent),
            sent: 3,
            requested: Nanos(12_500_000),
            errors: 3,
        };
        let line = absent.to_string();
        assert!(
            line.contains("phase requests: 3 relayed to nothing ("),
            "{line}"
        );
        assert!(line.contains("is not loaded"), "{line}");
        assert!(line.ends_with("12.50 ms asked for, 3 not sent"), "{line}");
    }
}
