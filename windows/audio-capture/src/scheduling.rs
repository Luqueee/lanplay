//! Asking Windows for an audio thread's scheduling, and reporting what it gave.
//!
//! A thread that carries audio has a period the hardware keeps whether or not
//! anybody kept up with it: the endpoint delivers a packet every device period,
//! and a packet collected late is a packet whose frames arrive late at the far
//! end for the rest of the session. That is a deadline, and Windows expresses a
//! deadline through the Multimedia Class Scheduler Service rather than through a
//! priority. Joining an MMCSS task tells the scheduler the thread is doing
//! periodic media work, and MMCSS then raises it into the real-time range for
//! most of each period while reserving the remainder for everything else on the
//! machine, which is precisely the guarantee a plain
//! `THREAD_PRIORITY_TIME_CRITICAL` does not give: a raised priority is a
//! position in a queue, not a promise about a period.
//!
//! `Pro Audio` and not `Audio`, which is the task the tone source joins. The
//! task name selects the priority MMCSS grants inside its real-time range, and
//! `Pro Audio` is the highest of the audio classes. The thread here does more
//! per period than a render thread does -- it collects a packet, encodes two
//! frames and puts two datagrams on a socket -- so it is the one that most
//! wants the head of the queue, and A1's capture probe left this registration
//! named as the thing its ordinary-thread baseline existed to be judged
//! against.
//!
//! The grant is read back rather than assumed, in the same discipline the macOS
//! side follows with `THREAD_TIME_CONSTRAINT_POLICY`: the call can be refused,
//! a refusal carries a reason, and a measurement taken under a policy nobody
//! checked is a measurement of something else. The priority is read from the
//! thread afterwards for the same reason -- it is what the scheduler actually
//! put there, and MMCSS raising a thread is observable in it.

use core::fmt;

#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;
#[cfg(windows)]
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentThread,
    GetThreadPriority,
};
#[cfg(windows)]
use windows::core::w;

/// The MMCSS task an audio thread on this project asks to join.
pub const PRO_AUDIO: &str = "Pro Audio";

/// What the system granted the calling thread.
///
/// A value rather than a boolean because the report has to say which of the two
/// a run was measured under, and because a refusal carries the reason it was
/// refused. Nothing here decides whether a run is believable; it states what
/// the thread was running as, and the gate reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scheduling {
    /// Membership of an MMCSS task, with the priority the thread ended up at
    /// once it had joined.
    Mmcss {
        task: &'static str,
        /// `GetThreadPriority`'s answer. MMCSS reports a thread it has raised
        /// as `THREAD_PRIORITY_TIME_CRITICAL`, so a grant that left this at
        /// normal is a grant worth looking at rather than trusting.
        priority: i32,
    },
    /// The request was refused, and the thread runs wherever the scheduler puts
    /// it.
    Refused(String),
}

impl Scheduling {
    /// Whether the thread got the periodic-media scheduling it asked for.
    pub fn is_granted(&self) -> bool {
        matches!(self, Scheduling::Mmcss { .. })
    }
}

impl fmt::Display for Scheduling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scheduling::Mmcss { task, priority } => {
                write!(f, "MMCSS {task}, thread priority {priority}")
            }
            Scheduling::Refused(why) => write!(f, "default scheduling: {why}"),
        }
    }
}

/// Membership of the `Pro Audio` MMCSS task for as long as the value lives.
///
/// Reverted on drop rather than left to process exit, because the handle is a
/// registration in a system service and a thread that outlived the work it was
/// registered for would keep a share of the machine reserved for nothing.
pub struct ProAudio {
    #[cfg(windows)]
    handle: Option<HANDLE>,
    granted: Scheduling,
}

impl ProAudio {
    /// Asks for the task on the calling thread. Never fails: a refusal is a
    /// state to report, not an error to return, because a run under default
    /// scheduling is still a run and saying so is what stops its numbers being
    /// read as the task's.
    #[cfg(windows)]
    pub fn join() -> ProAudio {
        let mut index = 0u32;
        // SAFETY: the task name is a static wide literal, the index is a live
        // local the call writes once, and the registration applies to the
        // calling thread only.
        match unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) } {
            Ok(handle) => ProAudio {
                handle: Some(handle),
                granted: Scheduling::Mmcss {
                    task: PRO_AUDIO,
                    priority: thread_priority(),
                },
            },
            Err(error) => ProAudio {
                handle: None,
                granted: Scheduling::Refused(format!(
                    "AvSetMmThreadCharacteristicsW(\"{PRO_AUDIO}\") returned 0x{:08X}",
                    error.code().0 as u32
                )),
            },
        }
    }

    /// Off Windows there is no MMCSS to join, and a value claiming otherwise
    /// would make the report describe a machine this is not running on.
    #[cfg(not(windows))]
    pub fn join() -> ProAudio {
        ProAudio {
            granted: Scheduling::Refused(format!(
                "no MMCSS on {}, so nothing was asked for",
                std::env::consts::OS
            )),
        }
    }

    pub fn granted(&self) -> &Scheduling {
        &self.granted
    }
}

#[cfg(windows)]
impl Drop for ProAudio {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: the handle came from `AvSetMmThreadCharacteristicsW` on
            // this thread and is reverted exactly once, since it is taken out
            // of the option first.
            unsafe {
                let _ = AvRevertMmThreadCharacteristics(handle);
            }
        }
    }
}

/// The priority the scheduler has this thread at.
#[cfg(windows)]
fn thread_priority() -> i32 {
    // SAFETY: `GetCurrentThread` is a pseudo-handle needing no release, and
    // `GetThreadPriority` only reads.
    unsafe { GetThreadPriority(GetCurrentThread()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the whole type exists for: a refusal must not be
    /// readable as a grant, because every timing figure in a report is a figure
    /// about whichever of the two the thread actually ran under.
    #[test]
    fn a_refusal_is_not_mistaken_for_a_grant() {
        let refused =
            Scheduling::Refused("AvSetMmThreadCharacteristicsW returned 0x80070005".into());
        assert!(!refused.is_granted());
        assert!(refused.to_string().starts_with("default scheduling"));

        let granted = Scheduling::Mmcss {
            task: PRO_AUDIO,
            priority: 15,
        };
        assert!(granted.is_granted());
        assert_eq!(granted.to_string(), "MMCSS Pro Audio, thread priority 15");
    }

    /// Asking is allowed to fail and is not allowed to bring the process down,
    /// which is the only part of the request that can be checked anywhere.
    #[test]
    fn asking_never_panics_and_always_says_something() {
        let asked = ProAudio::join();
        assert!(!asked.granted().to_string().is_empty());
    }
}
