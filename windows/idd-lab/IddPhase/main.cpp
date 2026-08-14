// Moves the phase of the IDD-LAB virtual display's vblank once, and says what
// the driver made of it.
//
// One invocation both acts and reports, because the instrument that calls this
// runs it over ssh in the middle of a timed run and a second call would be a
// second round trip landing at an unknown instant.
//
// Every way this can fail exits with its own code and names itself, and that is
// the point of the program rather than a nicety. A missing interface means the
// driver did not load; a refused IOCTL means it loaded and disagreed; a request
// that arrives and moves nothing means it agreed and folded it. Reported as one
// undifferentiated failure, a driver that never loaded reads as a lever that
// does not work, and mistaking those two for each other is the confusion this
// whole line of work exists to end.
//
//   0  the driver accepted the delay
//   2  the delay was missing, unreadable or out of range
//   3  no device exposes the interface: the driver is not loaded
//   4  the interface is there but the device would not open
//   5  the driver refused the request
//   6  the delay was accepted but the driver's counters could not be read

#include <windows.h>
#include <cfgmgr32.h>

#include <math.h>
#include <stdio.h>
#include <stdlib.h>

#include "..\PhaseContract.h"

static const int EXIT_ACCEPTED = 0;
static const int EXIT_ARGUMENT = 2;
static const int EXIT_ABSENT = 3;
static const int EXIT_OPEN = 4;
static const int EXIT_REFUSED = 5;
static const int EXIT_UNREADABLE = 6;

// A delay is carried as nanoseconds in an unsigned 32-bit word, so this is the
// largest thing that can be asked for at all. Anything beyond it is a mistake
// in the caller rather than a large request, and is refused instead of being
// truncated into a small one.
static const double MAX_DELAY_MS = 4294967295.0 / 1000000.0;

struct CounterView
{
    HANDLE Section;
    const IddLabPhaseCounters* Counters;
};

static bool OpenCounters(CounterView* View)
{
    View->Section = OpenFileMappingW(FILE_MAP_READ, FALSE, IDD_LAB_PHASE_SECTION_NAME);
    if (View->Section == nullptr)
    {
        return false;
    }

    const void* Mapped = MapViewOfFile(View->Section, FILE_MAP_READ, 0, 0, sizeof(IddLabPhaseCounters));
    if (Mapped == nullptr)
    {
        CloseHandle(View->Section);
        View->Section = nullptr;
        return false;
    }

    View->Counters = static_cast<const IddLabPhaseCounters*>(Mapped);

    // A section under this name that does not carry the driver's stamp belongs
    // to something else, and reading somebody else's memory as a phase report
    // would produce numbers that look plausible and mean nothing.
    if (View->Counters->Magic != IDD_LAB_PHASE_MAGIC ||
        View->Counters->Version != IDD_LAB_PHASE_VERSION ||
        View->Counters->Size != sizeof(IddLabPhaseCounters))
    {
        UnmapViewOfFile(Mapped);
        CloseHandle(View->Section);
        View->Section = nullptr;
        View->Counters = nullptr;
        return false;
    }

    return true;
}

static void Report(const char* Label, const IddLabPhaseCounters& Counters)
{
    printf("%-9s requested %lld  rejected %lld  superseded %lld  taken %lld  applied %lld  folded %lld  moved %.3f ms  held %.3f ms\n",
        Label,
        Counters.Requested, Counters.Rejected, Counters.Superseded, Counters.Taken,
        Counters.Applied, Counters.Folded,
        Counters.MovedNanos / 1000000.0, Counters.HeldNanos / 1000000.0);
}

int __cdecl wmain(int argc, wchar_t* argv[])
{
    if (argc != 2)
    {
        printf("idd-phase: argument  a delay in milliseconds is required, for example 3 or 2.5\n");
        return EXIT_ARGUMENT;
    }

    wchar_t* End = nullptr;
    const double Milliseconds = wcstod(argv[1], &End);
    if (End == argv[1] || *End != L'\0' || !isfinite(Milliseconds))
    {
        printf("idd-phase: argument  \"%ls\" is not a number of milliseconds\n", argv[1]);
        return EXIT_ARGUMENT;
    }
    if (Milliseconds < 0.0 || Milliseconds > MAX_DELAY_MS)
    {
        printf("idd-phase: argument  %.6f ms is outside 0 to %.6f ms, which is every delay this driver can carry\n",
            Milliseconds, MAX_DELAY_MS);
        return EXIT_ARGUMENT;
    }

    ULONG DelayNanos = static_cast<ULONG>(Milliseconds * 1000000.0 + 0.5);

    // The list is asked for its size and then read, and both can change between
    // the two calls if the driver is loading or unloading right now. Retried
    // rather than reported, because a device arriving mid-query is not a fault
    // and reporting it as an absent driver would be a lie told at the worst
    // possible moment.
    GUID Interface = GUID_DEVINTERFACE_IDD_LAB_PHASE;
    wchar_t* List = nullptr;
    CONFIGRET Result = CR_BUFFER_SMALL;
    for (int Attempt = 0; Attempt < 4 && Result == CR_BUFFER_SMALL; Attempt++)
    {
        ULONG Length = 0;
        Result = CM_Get_Device_Interface_List_SizeW(&Length, &Interface, nullptr,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT);
        if (Result != CR_SUCCESS)
        {
            break;
        }

        free(List);
        List = static_cast<wchar_t*>(calloc(Length, sizeof(wchar_t)));
        if (List == nullptr)
        {
            printf("idd-phase: absent    out of memory listing the interface\n");
            return EXIT_ABSENT;
        }

        Result = CM_Get_Device_Interface_ListW(&Interface, nullptr, List, Length,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT);
    }

    if (Result != CR_SUCCESS || List == nullptr || List[0] == L'\0')
    {
        free(List);
        printf("idd-phase: absent    no present device exposes {60EBFC7A-1723-41F3-9CC6-19EBF0DEBED2} (CONFIGRET 0x%lx).\n",
            static_cast<unsigned long>(Result));
        printf("                     that is what a driver which did not load looks like, not a lever that does not work.\n");
        printf("                     the IDD-LAB display is probably gone too; bring it back before reading anything else.\n");
        return EXIT_ABSENT;
    }

    printf("device    %ls\n", List);

    const HANDLE Device = CreateFileW(List, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
        nullptr, OPEN_EXISTING, 0, nullptr);
    free(List);
    if (Device == INVALID_HANDLE_VALUE)
    {
        printf("idd-phase: open      the interface exists but the device refused to open (error %lu)\n", GetLastError());
        return EXIT_OPEN;
    }

    CounterView View = {};
    IddLabPhaseCounters Before = {};
    const bool Readable = OpenCounters(&View);
    if (Readable)
    {
        Before = *View.Counters;
        Report("before", Before);
    }

    DWORD Returned = 0;
    const BOOL Sent = DeviceIoControl(Device, IOCTL_IDD_LAB_PHASE_SHIFT,
        &DelayNanos, sizeof(DelayNanos), nullptr, 0, &Returned, nullptr);
    const DWORD SendError = GetLastError();
    CloseHandle(Device);

    if (!Sent)
    {
        printf("idd-phase: refused   the driver rejected %lu ns (error %lu); the interface was there and answered\n",
            DelayNanos, SendError);
        if (Readable)
        {
            Report("after", *View.Counters);
        }
        return EXIT_REFUSED;
    }

    printf("accepted  %.3f ms (%lu ns) to hold the next frame back by\n", Milliseconds, DelayNanos);

    if (!Readable)
    {
        printf("idd-phase: counters  the delay was accepted but %ls could not be read, so what became of it is unknown\n",
            IDD_LAB_PHASE_SECTION_NAME);
        return EXIT_UNREADABLE;
    }

    // The request is served by the swap-chain thread on its next frame, which
    // at 120 Hz is up to a period away, so the counters immediately after the
    // send would show it arrived and nothing more. Waited for rather than
    // guessed at, and reported either way: a request sitting unread because
    // nothing is drawing to the display is a real finding about the lab, and
    // reporting it as a lever that did nothing would send the next reader after
    // the wrong thing entirely.
    LARGE_INTEGER Frequency = {};
    LARGE_INTEGER Start = {};
    QueryPerformanceFrequency(&Frequency);
    QueryPerformanceCounter(&Start);

    double WaitedMs = 0.0;
    bool Observed = false;
    for (int Poll = 0; Poll < 100; Poll++)
    {
        LARGE_INTEGER Now = {};
        QueryPerformanceCounter(&Now);
        WaitedMs = Frequency.QuadPart > 0
            ? ((Now.QuadPart - Start.QuadPart) * 1000.0) / Frequency.QuadPart
            : 0.0;

        if (View.Counters->Taken > Before.Taken || View.Counters->Superseded > Before.Superseded)
        {
            Observed = true;
            break;
        }
        Sleep(5);
    }

    Report("after", *View.Counters);

    if (Observed)
    {
        printf("took      the frame loop took it %.1f ms after the send\n", WaitedMs);
        if (View.Counters->Applied == Before.Applied)
        {
            printf("inert     it was taken and moved nothing: zero, or an exact multiple of one refresh period\n");
        }
    }
    else
    {
        printf("pending   it arrived but no frame was processed within %.0f ms, so nothing has taken it yet.\n", WaitedMs);
        printf("          the frame loop only runs when the desktop changes; check something is drawing on IDD-LAB.\n");
    }

    return EXIT_ACCEPTED;
}
