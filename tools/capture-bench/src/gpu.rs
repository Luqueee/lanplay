//! Timing a copy that has not happened yet.
//!
//! `CopyResource` queues work. It returns as soon as the driver has written a
//! command, which on a warm path is a few microseconds, and reporting that as
//! "copy time 0.01 ms" would be the exact lie this phase exists to avoid: the
//! GPU has not touched a pixel at that point. So three numbers, never one.
//!
//! * **submit (CPU)** — time inside `CopyResource`. What the calling thread
//!   paid. Real, and not the cost of the copy.
//! * **GPU time** — a `D3D11_QUERY_TIMESTAMP` on each side of the copy inside
//!   a `D3D11_QUERY_TIMESTAMP_DISJOINT`, converted with the frequency the
//!   disjoint query reports. What the GPU actually spent.
//! * **completion observed** — wall time from submitting to the first poll
//!   that found the result ready. An upper bound quantised by how often the
//!   loop polls, reported under its own name so it is never mistaken for the
//!   one above.
//!
//! Two things are deliberately not done. Nothing calls `Flush`: forcing the
//! command buffer out would make every number here prompt and would measure a
//! pipeline the product must never contain. And nothing waits for a result:
//! `GetData` is always passed `D3D11_ASYNC_GETDATA_DONOTFLUSH`, so a query
//! that is not ready is counted and moved past rather than stalling the
//! capture loop that is being measured.
//!
//! `windows` folds `S_FALSE` into `Ok(())` because it is not a failure code,
//! which loses precisely the bit "is it ready" consists of, so the raw vtable
//! entry is called and the `HRESULT` inspected directly.

#![cfg(windows)]

use core::ffi::c_void;

use lanplay_capture::CaptureError;
use lanplay_telemetry::{Nanos, Timestamp};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, D3D11_QUERY_DESC,
    D3D11_QUERY_TIMESTAMP, D3D11_QUERY_TIMESTAMP_DISJOINT, ID3D11Device, ID3D11DeviceContext,
    ID3D11Query,
};
use windows::core::{HRESULT, Interface};

use crate::series::{Series, Summary};

/// `S_FALSE`: the query has been issued but the GPU has not reached it.
const NOT_READY: HRESULT = HRESULT(1);

/// How long the drain at the end of a run keeps polling for results that were
/// submitted late. Bounded because the alternative is waiting for a flush that
/// this harness refuses to issue.
const DRAIN_BUDGET: Nanos = Nanos::from_millis(250);

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Free,
    /// Bracketing has begun; the copy has not been submitted yet.
    Open,
    /// Submitted and waiting on the GPU.
    Pending,
}

struct QuerySet {
    disjoint: ID3D11Query,
    begin: ID3D11Query,
    end: ID3D11Query,
    state: SlotState,
    submitted_at: Timestamp,
    /// Which measurement window this set belongs to. A set issued during the
    /// warm-up must not deliver its result into the steady-state statistics
    /// just because it finished late.
    generation: u64,
}

pub struct CopyTimer {
    sets: Vec<QuerySet>,
    generation: u64,
    copies: u64,
    resolved: u64,
    disjoint_discarded: u64,
    exhausted: u64,
    unresolved: u64,
    submit_cpu: Series,
    gpu: Series,
    completion: Series,
}

impl CopyTimer {
    /// `depth` query sets. More than the pool has slots, so a copy is never
    /// left unmeasured merely because the previous result has not been
    /// collected yet.
    pub fn new(device: &ID3D11Device, depth: u32) -> Result<CopyTimer, CaptureError> {
        let mut sets = Vec::with_capacity(depth as usize);
        for _ in 0..depth {
            sets.push(QuerySet {
                disjoint: query(device, D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                begin: query(device, D3D11_QUERY_TIMESTAMP)?,
                end: query(device, D3D11_QUERY_TIMESTAMP)?,
                state: SlotState::Free,
                submitted_at: Timestamp::from_nanos(0),
                generation: 0,
            });
        }
        Ok(CopyTimer {
            sets,
            generation: 0,
            copies: 0,
            resolved: 0,
            disjoint_discarded: 0,
            exhausted: 0,
            unresolved: 0,
            submit_cpu: Series::new(),
            gpu: Series::new(),
            completion: Series::new(),
        })
    }

    /// Opens the bracket around a copy that is about to be submitted.
    ///
    /// `None` when every set is still in flight: the copy still happens, it
    /// just goes unmeasured, and the count says how often that was.
    pub fn open(&mut self, context: &ID3D11DeviceContext) -> Option<usize> {
        let Some(index) = self
            .sets
            .iter()
            .position(|set| set.state == SlotState::Free)
        else {
            self.exhausted += 1;
            return None;
        };
        let set = &mut self.sets[index];
        // SAFETY: both queries were created by this device and belong to no
        // other bracket, because the slot was Free.
        unsafe {
            context.Begin(&set.disjoint);
            context.End(&set.begin);
        }
        set.state = SlotState::Open;
        Some(index)
    }

    /// Closes the bracket. `submitted_at` is when the copy was handed to the
    /// driver, which is what completion is measured from.
    pub fn close(&mut self, context: &ID3D11DeviceContext, index: usize, submitted_at: Timestamp) {
        let generation = self.generation;
        let set = &mut self.sets[index];
        // SAFETY: `open` issued the matching Begin/End on this same slot.
        unsafe {
            context.End(&set.end);
            context.End(&set.disjoint);
        }
        set.state = SlotState::Pending;
        set.submitted_at = submitted_at;
        set.generation = generation;
    }

    /// Records a copy and the CPU time its submission cost.
    pub fn submitted(&mut self, cpu: Nanos) {
        self.copies += 1;
        self.submit_cpu.push(cpu);
    }

    /// Collects whatever the GPU has finished. Never blocks, never flushes.
    pub fn poll(&mut self, context: &ID3D11DeviceContext, now: Timestamp) {
        for index in 0..self.sets.len() {
            if self.sets[index].state != SlotState::Pending {
                continue;
            }
            self.collect(context, index, now);
        }
    }

    fn collect(&mut self, context: &ID3D11DeviceContext, index: usize, now: Timestamp) {
        let mut disjoint = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
        let ready = read(
            context,
            &self.sets[index].disjoint,
            (&raw mut disjoint).cast::<c_void>(),
            size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() as u32,
        );
        match ready {
            Ok(true) => {}
            // Not ready: leave it pending and try again next time round.
            Ok(false) => return,
            // A query the driver refuses is not a measurement; free the slot
            // rather than leaking it for the rest of the run.
            Err(_) => {
                self.sets[index].state = SlotState::Free;
                self.disjoint_discarded += 1;
                return;
            }
        }

        let mut begin = 0u64;
        let mut end = 0u64;
        let both = read(
            context,
            &self.sets[index].begin,
            (&raw mut begin).cast::<c_void>(),
            size_of::<u64>() as u32,
        )
        .unwrap_or(false)
            && read(
                context,
                &self.sets[index].end,
                (&raw mut end).cast::<c_void>(),
                size_of::<u64>() as u32,
            )
            .unwrap_or(false);

        let set = &mut self.sets[index];
        set.state = SlotState::Free;
        let stale = set.generation != self.generation;
        let submitted_at = set.submitted_at;

        if stale {
            return;
        }
        // The GPU changed clock rate across the interval, so the tick delta is
        // not a duration. Counted rather than converted into a wrong number.
        if disjoint.Disjoint.as_bool() || disjoint.Frequency == 0 || !both || end < begin {
            self.disjoint_discarded += 1;
            return;
        }

        let ticks = end - begin;
        let nanos = (ticks as u128 * 1_000_000_000u128 / disjoint.Frequency as u128) as u64;
        self.gpu.push(Nanos(nanos));
        self.completion.push(now.saturating_since(submitted_at));
        self.resolved += 1;
    }

    /// Polls until everything outstanding has answered or the budget runs out.
    ///
    /// Whatever is still pending is counted as unresolved. Waiting longer
    /// would need a flush, and a flush is the one thing this must not do.
    pub fn drain(&mut self, context: &ID3D11DeviceContext) {
        let deadline = Timestamp::now().add(DRAIN_BUDGET);
        loop {
            let now = Timestamp::now();
            self.poll(context, now);
            let pending = self
                .sets
                .iter()
                .filter(|set| set.state == SlotState::Pending)
                .count();
            if pending == 0 || now >= deadline {
                self.unresolved += pending as u64;
                for set in &mut self.sets {
                    set.state = SlotState::Free;
                }
                return;
            }
            core::hint::spin_loop();
        }
    }

    /// Starts a new measurement window. Results from the previous one are
    /// discarded as they arrive rather than being attributed to this one.
    pub fn begin_window(&mut self) {
        self.generation += 1;
        self.copies = 0;
        self.resolved = 0;
        self.disjoint_discarded = 0;
        self.exhausted = 0;
        self.unresolved = 0;
        self.submit_cpu.clear();
        self.gpu.clear();
        self.completion.clear();
    }

    pub fn copies(&self) -> u64 {
        self.copies
    }

    pub fn resolved(&self) -> u64 {
        self.resolved
    }

    pub fn disjoint_discarded(&self) -> u64 {
        self.disjoint_discarded
    }

    pub fn exhausted(&self) -> u64 {
        self.exhausted
    }

    pub fn unresolved(&self) -> u64 {
        self.unresolved
    }

    pub fn submit_cpu(&self) -> Summary {
        self.submit_cpu.summary()
    }

    pub fn gpu(&self) -> Summary {
        self.gpu.summary()
    }

    pub fn completion(&self) -> Summary {
        self.completion.summary()
    }
}

fn query(
    device: &ID3D11Device,
    kind: windows::Win32::Graphics::Direct3D11::D3D11_QUERY,
) -> Result<ID3D11Query, CaptureError> {
    let desc = D3D11_QUERY_DESC {
        Query: kind,
        MiscFlags: 0,
    };
    let mut query = None;
    // SAFETY: the description is fully initialised and the out-pointer is
    // valid for the duration of the call.
    unsafe { device.CreateQuery(&desc, Some(&mut query)) }.map_err(|error| CaptureError::Api {
        call: "ID3D11Device::CreateQuery",
        hresult: error.code().0,
    })?;
    query.ok_or_else(|| CaptureError::Unsupported("CreateQuery returned no query".into()))
}

/// `Ok(true)` when the result was written, `Ok(false)` when the GPU has not
/// got there yet, `Err` when the driver rejected the read.
///
/// Calls the vtable entry rather than the `windows` wrapper because the
/// wrapper maps `S_FALSE` — the entire "not ready" signal — onto `Ok(())`.
fn read(
    context: &ID3D11DeviceContext,
    query: &ID3D11Query,
    data: *mut c_void,
    size: u32,
) -> Result<bool, HRESULT> {
    // SAFETY: `data` points at `size` bytes of a live local of the type this
    // query returns, and the query belongs to this context's device. The
    // DONOTFLUSH flag is what keeps the call from perturbing the pipeline.
    let hresult = unsafe {
        (Interface::vtable(context).GetData)(
            Interface::as_raw(context),
            Interface::as_raw(query),
            data,
            size,
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
        )
    };
    if hresult.is_err() {
        return Err(hresult);
    }
    Ok(hresult != NOT_READY)
}
