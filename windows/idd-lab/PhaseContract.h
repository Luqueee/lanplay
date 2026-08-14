//
// VERDICT, recorded because this mechanism works and does not help.
//
// Holding IddCxSwapChainFinishedProcessingFrame back moves nothing the receiver
// can see. A 3.00 ms hold, confirmed by the counters below (moved 11.000 to
// 14.000 ms, 14.545 ms actually held), left a 79-sample phase trace on the client
// inside 1.00 to 2.04 ms with a largest movement between samples of 0.306 ms, on a
// healthy link. The header itself calls that call a progress report and the sample
// calls it a hint; it behaves like one.
//
// Nor is there anywhere else to look. The rate is set by the committed target
// mode's vSyncFreq over vSyncFreqDivider, a per-mode quantity; the acquire loop's
// only output is IDDCX_METADATA, every field [out], including PresentDisplayQPCTime
// which is the OS stating the phase it already chose, with no [in] counterpart;
// IddCxSwapChainReportFrameStatistics has no documented OS reaction and its
// predecessor is documented as ETW-log-only; IddCxMonitorUpdateModes changes the
// available list, not the active mode, so there is no fractional rate to walk a
// phase with; and the wireless transmission types live in EndPointDiagnostics,
// declared inert for runtime decisions.
//
// And the phase is not re-rolled by restarting: across six sessions every boundary
// was explained by elapsed time times the measured drift to within 0.28 ms of an
// 8.33 ms period, so re-establishing until the draw is good is not a lever either.
//
// This file and its IOCTL are kept as the instrument that proved that, not as a
// feature. Nothing sends a request by default.
//
#pragma once

// What the IDD-LAB driver and whoever moves its phase have to agree on.
//
// One header included by both sides rather than two copies of four constants.
// A sender and a driver that disagree about an IOCTL code fail as silence, and
// silence is precisely what a lever that does not work looks like from the
// outside; this whole line of work exists because those two were confused for
// each other once already.

#ifndef CTL_CODE
#include <winioctl.h>
#endif

// Exposed by the driver so a request can find the device without knowing
// anything about how it was enumerated. Its absence is the signature of a
// driver that did not load, which is a different fault from a driver that
// loaded and ignored the request, and the sender reports the two apart.
static const GUID GUID_DEVINTERFACE_IDD_LAB_PHASE =
    { 0x60ebfc7a, 0x1723, 0x41f3, { 0x9c, 0xc6, 0x19, 0xeb, 0xf0, 0xde, 0xbe, 0xd2 } };

// Hold the next frame back by a number of nanoseconds. The input buffer is
// exactly four bytes, a little-endian ULONG, there is no output buffer, and
// anything else is refused with STATUS_INVALID_PARAMETER rather than guessed
// at: a request whose meaning has to be inferred is worse than no request.
//
// The delay applies once, to the next frame only. It is never a persistent
// offset and never a rate change - the display keeps its 120 Hz and only the
// instant at which one period begins moves. A delay of a period or more is
// folded modulo the period rather than obeyed, because a request that stalls
// the display is a request that removes the laboratory.
//
// It is advisory in both directions. A driver that ignores it must still work,
// and a run that never sends one must behave exactly as a run on a driver that
// never heard of it.
#define IOCTL_IDD_LAB_PHASE_SHIFT \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_WRITE_DATA)

// A named shared section is how a driver with no stdout says what it did.
//
// The alternatives were all worse for the one question that matters, which is
// what the counters read at an arbitrary instant chosen by a script on another
// machine. WPP tracing is already wired into this driver and answers a
// different question: it needs a session started before the event and a
// formatting pass afterwards, so it reports what happened while somebody was
// watching rather than the running total since load. The device's registry key
// is readable at any time but puts a hive write in the swap-chain thread's
// path, and that thread has 8.33 ms for everything it does. Returning the
// counters in the IOCTL's output buffer is ruled out by the contract above and
// would be wrong anyway, because every read would also send a delay and so
// perturb the thing being measured. A file has the write cost of the registry
// plus the chance of being read halfway through an update.
//
// A section costs one page mapped once at load, interlocked adds thereafter,
// and it can be read at any instant by anyone with read access. It also has the
// property the lab needs most: it exists exactly as long as the driver is
// loaded, so a reading can never be a stale value left behind by a driver that
// has since gone away.
#define IDD_LAB_PHASE_SECTION_NAME L"Global\\LanPlayIddLabPhase"

// Stamped by the driver before the section is published, checked by the reader
// before a single counter is believed. A section under this name that does not
// carry these is somebody else's memory, not this driver's report.
#define IDD_LAB_PHASE_MAGIC   0x4C505048ul
#define IDD_LAB_PHASE_VERSION 1ul

// Everything the lever has been asked for and everything that became of it.
//
// Counted separately rather than reduced to one number because the failures
// they distinguish look identical in a total. A request that never arrived
// leaves Requested where it was; one that arrived and was displaced by a newer
// one before the frame loop looked raises Superseded; one that arrived, was
// taken and folded to nothing raises Taken without raising Applied. Those are
// three different reasons for a phase that did not move, and a run that cannot
// tell them apart cannot say whether the lever works.
//
// Every field is 64 bits and updated with an interlocked add, so a reader
// racing the frame loop sees each field either before or after an update and
// never halfway through one. Nothing here is a snapshot of all fields at one
// instant, and nothing needs to be: these are monotone totals, so a reader that
// takes a copy before and after an action gets a difference that is at worst
// generous by whatever the loop did in between.
#pragma pack(push, 8)
struct IddLabPhaseCounters
{
    ULONG  Magic;
    ULONG  Version;
    ULONG  Size;
    // The modulus a delay is folded against, published rather than assumed so
    // a reader does not have to hard-code the driver's refresh rate to
    // interpret Folded.
    ULONG  PeriodNanos;

    // Well-formed requests that reached the inbox.
    LONG64 Requested;
    // Requests refused for a malformed buffer. Counted because a sender built
    // against a different idea of the contract otherwise looks like a sender
    // that never ran.
    LONG64 Rejected;
    // Requests a newer one displaced before the frame loop took them.
    LONG64 Superseded;
    // Requests the frame loop took and has finished serving. Raised after every
    // other field belonging to the same request, so a reader waiting for its own
    // request to be answered waits on this one and finds the rest already there.
    // While a delay is being held the request counts as neither pending nor
    // taken, which is a gap of under one refresh period in a total meant for
    // spotting requests that went unread for a whole run.
    LONG64 Taken;
    // Requests that moved the frame loop.
    LONG64 Applied;
    // Requests of a period or more, folded rather than obeyed.
    LONG64 Folded;
    // Phase asked for and granted, after folding.
    LONG64 MovedNanos;
    // Phase actually spent holding, measured on the performance counter. It
    // differs from MovedNanos by the timer's error, which is the only evidence
    // from inside the driver that the wait is accurate enough to be worth
    // making at all.
    LONG64 HeldNanos;
};
#pragma pack(pop)
