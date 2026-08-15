//! Asking the system for a deadline rather than for a priority.
//!
//! A thread that feeds an audio callback has a hard period: the device takes a buffer
//! every few milliseconds whether or not anybody filled it, and an unfilled buffer is a
//! click. That is a deadline, and macOS has a policy for expressing one -
//! `THREAD_TIME_CONSTRAINT_POLICY`, which states a period, how much computation is
//! needed inside it, and how late the work may finish.
//!
//! The alternative, and what this replaced, is a quality-of-service class. A class is a
//! band, not a deadline: it says this work matters more than that work, and leaves the
//! scheduler to decide what that means. Measured on this machine, a producer at
//! `QOS_CLASS_USER_INTERACTIVE` underran zero, twelve, zero and zero times across four
//! runs of a few minutes each, and a controlled comparison against a compiler running in
//! parallel showed no underruns at all - so the trigger was never sustained load and was
//! never identified. A mechanism whose failures cannot be explained is not a mechanism
//! to build on, and the point of the time-constraint policy is that it does not require
//! the explanation.
//!
//! The request can be refused, and a refusal is reported rather than swallowed. A
//! measurement taken under a policy nobody granted is a measurement of something else,
//! and this project has spent enough of its time on instruments that reported a number
//! from a state they had not checked.

use std::fmt;

/// What the system actually granted.
///
/// Kept as a value rather than a boolean because the report has to say which of the two
/// a run was measured under, and because a refusal carries the reason it was refused.
pub enum ScheduledAs {
    /// A real deadline: a period, a computation budget inside it, and a constraint.
    TimeConstraint {
        period_ns: u64,
        computation_ns: u64,
        constraint_ns: u64,
    },
    /// The system refused, and the thread runs wherever the scheduler puts it.
    Default(String),
}

impl fmt::Display for ScheduledAs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScheduledAs::TimeConstraint {
                period_ns,
                computation_ns,
                constraint_ns,
            } => write!(
                f,
                "time constraint, period {:.3} ms computation {:.3} ms constraint {:.3} ms",
                *period_ns as f64 / 1e6,
                *computation_ns as f64 / 1e6,
                *constraint_ns as f64 / 1e6
            ),
            ScheduledAs::Default(why) => write!(f, "default priority: {why}"),
        }
    }
}

impl ScheduledAs {
    /// Whether the thread got the deadline it asked for.
    pub fn is_real_time(&self) -> bool {
        matches!(self, ScheduledAs::TimeConstraint { .. })
    }

    /// Asks for a deadline on the calling thread, sized from the period it has to keep up
    /// with.
    ///
    /// The computation budget is an eighth of the period, which is generous for
    /// generating at most one buffer of audio and deliberately not larger: a thread that
    /// claims more computation than it uses reserves a share of the machine that nothing
    /// here needs, and on a laptop that is somebody's battery.
    ///
    /// The constraint is half the period, so the work must land well inside the cycle it
    /// belongs to rather than merely before the next one begins.
    #[cfg(target_os = "macos")]
    pub fn request(period_ns: u64) -> ScheduledAs {
        let computation_ns = period_ns / 8;
        let constraint_ns = period_ns / 2;

        let Some(timebase) = MachTimebase::read() else {
            return ScheduledAs::Default("mach_timebase_info failed".to_owned());
        };

        let mut policy = libc::thread_time_constraint_policy {
            period: timebase.ticks(period_ns),
            computation: timebase.ticks(computation_ns),
            constraint: timebase.ticks(constraint_ns),
            // Preemptible on purpose. A non-preemptible thread that misbehaves takes the
            // machine with it, and no measurement here is worth that risk.
            preemptible: 1,
        };

        // SAFETY: the port comes from this thread's own pthread handle, the policy
        // pointer is a live local of exactly the type the flavour names, and the count is
        // the constant the same header defines for it. The call affects only this thread.
        let status = unsafe {
            libc::thread_policy_set(
                libc::pthread_mach_thread_np(libc::pthread_self()),
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                std::ptr::from_mut(&mut policy).cast::<libc::integer_t>() as libc::thread_policy_t,
                libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            )
        };

        if status == 0 {
            ScheduledAs::TimeConstraint {
                period_ns,
                computation_ns,
                constraint_ns,
            }
        } else {
            ScheduledAs::Default(format!("thread_policy_set returned {status}"))
        }
    }

    /// Off macOS there is no policy to ask for, and pretending otherwise would make the
    /// report describe a machine this is not running on.
    #[cfg(not(target_os = "macos"))]
    pub fn request(_period_ns: u64) -> ScheduledAs {
        ScheduledAs::Default("not macOS".to_owned())
    }
}

/// The ratio between nanoseconds and the units the scheduler counts in.
///
/// Read rather than assumed: the two are equal on the Intel Macs this project started on
/// and are not on Apple silicon, and a policy stated in the wrong units asks for a period
/// off by a factor of forty.
#[cfg(target_os = "macos")]
struct MachTimebase {
    numer: u64,
    denom: u64,
}

#[cfg(target_os = "macos")]
impl MachTimebase {
    fn read() -> Option<MachTimebase> {
        #[repr(C)]
        #[derive(Default)]
        struct Info {
            numer: u32,
            denom: u32,
        }

        unsafe extern "C" {
            fn mach_timebase_info(info: *mut Info) -> i32;
        }

        let mut info = Info::default();
        // SAFETY: a valid, correctly sized out-parameter, and the call has no other
        // effect.
        let status = unsafe { mach_timebase_info(&mut info) };
        if status != 0 || info.numer == 0 || info.denom == 0 {
            return None;
        }
        Some(MachTimebase {
            numer: u64::from(info.numer),
            denom: u64::from(info.denom),
        })
    }

    /// Nanoseconds as scheduler ticks. Saturating, because a period wider than a `u32`
    /// of ticks is not a period any audio device has and clamping is better than
    /// wrapping into a deadline of microseconds.
    fn ticks(&self, nanos: u64) -> u32 {
        let ticks = nanos.saturating_mul(self.denom) / self.numer;
        u32::try_from(ticks).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::ScheduledAs;

    /// A refusal is not a real-time thread, and the report has to be able to tell.
    #[test]
    fn a_refusal_is_not_mistaken_for_a_deadline() {
        let refused = ScheduledAs::Default("thread_policy_set returned 46".to_owned());
        assert!(!refused.is_real_time());
        assert!(refused.to_string().contains("default priority"));
    }

    /// And a granted policy states the three numbers it was granted with, because a
    /// report that said only "real time" would not let anybody check the arithmetic.
    #[test]
    fn a_granted_policy_states_its_numbers() {
        let granted = ScheduledAs::TimeConstraint {
            period_ns: 5_333_333,
            computation_ns: 666_666,
            constraint_ns: 2_666_666,
        };
        assert!(granted.is_real_time());
        let shown = granted.to_string();
        assert!(shown.contains("period 5.333 ms"), "{shown}");
        assert!(shown.contains("computation 0.667 ms"), "{shown}");
        assert!(shown.contains("constraint 2.667 ms"), "{shown}");
    }

    /// The request has to survive being made on this machine, whatever the system then
    /// decides: a panic here would be a probe that cannot start rather than one that
    /// reports it was refused.
    #[test]
    fn requesting_a_deadline_never_panics() {
        let policy = ScheduledAs::request(5_333_333);
        // Either outcome is legitimate. What is not legitimate is a request that cannot
        // be made without bringing the process down.
        assert!(!policy.to_string().is_empty());
    }
}
