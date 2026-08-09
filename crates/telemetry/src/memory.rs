//! Resident memory of this process.
//!
//! "Memory was stable" is a gate criterion for every soak test here, so it has
//! to be a number: sample this into a [`crate::Trend`] and read the slope. A
//! decoder that leaks one pixel buffer per frame at 120 fps shows up as a
//! slope long before it shows up as a crash.

/// Resident set size of this process in bytes, or `None` where the platform is
/// not supported.
pub fn resident_bytes() -> Option<u64> {
    platform::resident_bytes()
}

#[cfg(target_os = "macos")]
mod platform {
    /// `MACH_TASK_BASIC_INFO`
    const FLAVOR: u32 = 20;

    #[repr(C)]
    #[derive(Default)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [i32; 2],
        system_time: [i32; 2],
        policy: i32,
        suspend_count: i32,
    }

    /// The count `task_info` expects is in units of `natural_t`, not bytes.
    const INFO_COUNT: u32 = (size_of::<MachTaskBasicInfo>() / size_of::<u32>()) as u32;
    const _: () = assert!(INFO_COUNT == 12);

    unsafe extern "C" {
        /// Not a function: libSystem exports the current task port as data.
        static mach_task_self_: u32;
        fn task_info(
            target_task: u32,
            flavor: u32,
            task_info_out: *mut MachTaskBasicInfo,
            task_info_count: *mut u32,
        ) -> i32;
    }

    pub fn resident_bytes() -> Option<u64> {
        let mut info = MachTaskBasicInfo::default();
        let mut count = INFO_COUNT;
        // SAFETY: `mach_task_self_` is initialised by libSystem before main and
        // never mutated; `info` and `count` are valid out-parameters sized as
        // the flavour requires.
        let status = unsafe { task_info(mach_task_self_, FLAVOR, &mut info, &mut count) };
        (status == 0).then_some(info.resident_size)
    }
}

#[cfg(windows)]
mod platform {
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        /// The kernel32 forwarder, so no psapi import library is needed.
        fn K32GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    pub fn resident_bytes() -> Option<u64> {
        let mut counters = ProcessMemoryCounters {
            cb: size_of::<ProcessMemoryCounters>() as u32,
            ..Default::default()
        };
        // SAFETY: the pseudo-handle needs no release; `counters` is correctly
        // sized and its `cb` field set as the API requires.
        let ok = unsafe {
            let process = GetCurrentProcess();
            K32GetProcessMemoryInfo(process, &mut counters, counters.cb)
        };
        (ok != 0).then_some(counters.working_set_size as u64)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    pub fn resident_bytes() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_memory_tracks_a_large_allocation() {
        let before = resident_bytes().expect("platform reports resident memory");
        assert!(before > 1_000_000, "implausible RSS: {before}");

        // Touch every page: RSS counts resident pages, not reservations.
        let mut ballast = vec![0u8; 64 * 1024 * 1024];
        for page in ballast.chunks_mut(4096) {
            page[0] = 1;
        }
        let after = resident_bytes().expect("resident memory");
        assert!(
            after > before + 32_000_000,
            "RSS did not follow a 64 MB allocation: {before} -> {after}"
        );
        drop(ballast);
    }
}
