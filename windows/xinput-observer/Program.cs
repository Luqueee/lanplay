using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;


return await Observer.RunAsync(args);

internal static class Observer
{
    private const int DefaultTimeoutMilliseconds = 250;
    private const int DefaultPollIntervalMilliseconds = 2;
    private const ushort SupportedButtonMask = 0x07ff;
    private const int AxisTolerance = 1;
    private const int TriggerTolerance = 1;

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
    };

    public static async Task<int> RunAsync(string[] args)
    {
        if (args.Length == 1 && args[0] == "--self-test")
            return RunSelfTest();

        Options options;
        try
        {
            options = ParseOptions(args);
        }
        catch (ArgumentException error)
        {
            Console.Error.WriteLine($"xinput-observer: {error.Message}");
            WriteSummary(new Summary { InvalidStates = 1, Verdict = "fail", Error = error.Message });
            return 2;
        }

        var summary = new Summary();
        ComparisonFailure? firstFailure = null;
        string? line;
        while ((line = await Console.In.ReadLineAsync()) is not null)
        {
            if (string.IsNullOrWhiteSpace(line))
                continue;

            ExpectedState? expected;
            try
            {
                expected = JsonSerializer.Deserialize<ExpectedState>(line, JsonOptions);
                Validate(expected);
            }
            catch (Exception error) when (error is JsonException or ArgumentException)
            {
                summary.InvalidStates++;
                summary.Error ??= error.Message;
                continue;
            }

            summary.ExpectedStates++;
            var result = ObserveExpectedState(expected!, options, summary);
            if (result.Matched)
            {
                summary.MatchedStates++;
                continue;
            }

            summary.MismatchedStates++;
            if (result.Unavailable)
                summary.UnavailableStates++;
            else
                summary.TimeoutStates++;
            firstFailure ??= result.Failure;
        }

        summary.FirstMismatch = firstFailure;
        summary.Verdict = summary.InvalidStates == 0
            && summary.ExpectedStates > 0
            && summary.ObservedStates > 0
            && summary.MismatchedStates == 0
            ? "pass"
            : "fail";
        WriteSummary(summary);
        return summary.Verdict == "pass" ? 0 : 1;
    }

    private static ObservationResult ObserveExpectedState(ExpectedState expected, Options options, Summary summary)
    {
        var stopwatch = Stopwatch.StartNew();
        ComparisonFailure? latestFailure = null;
        var observed = false;
        while (stopwatch.ElapsedMilliseconds <= options.TimeoutMilliseconds)
        {
            var status = Native.XInputGetState(expected.Slot, out var nativeState);
            if (status == Native.ErrorSuccess)
            {
                observed = true;
                summary.ObservedStates++;
                var actual = ObservedState.FromNative(nativeState.Gamepad);
                var comparison = Compare(expected, actual);
                if (comparison is null)
                    return ObservationResult.Match;
                latestFailure = comparison;
            }
            else
            {
                summary.XInputErrors++;
                latestFailure = ComparisonFailure.Unavailable(expected, status);
            }

            Thread.Sleep(options.PollIntervalMilliseconds);
        }

        return new ObservationResult(false, !observed, latestFailure ?? ComparisonFailure.Unavailable(expected, 0));
    }

    private static ComparisonFailure? Compare(ExpectedState expected, ObservedState actual)
    {
        var expectedButtons = (ushort)(expected.Buttons & SupportedButtonMask);
        if (expectedButtons != actual.Buttons)
            return ComparisonFailure.For(expected, actual, "buttons differ");
        if (expected.Dpad != actual.Dpad)
            return ComparisonFailure.For(expected, actual, "dpad differs");
        if (!AxisMatches(expected.LeftX, actual.LeftX)
            || !AxisMatches(expected.LeftY, actual.LeftY)
            || !AxisMatches(expected.RightX, actual.RightX)
            || !AxisMatches(expected.RightY, actual.RightY))
            return ComparisonFailure.For(expected, actual, "stick sign or quantized value differs");
        if (!TriggerMatches(expected.LeftTrigger, actual.LeftTrigger)
            || !TriggerMatches(expected.RightTrigger, actual.RightTrigger))
            return ComparisonFailure.For(expected, actual, "trigger quantized value differs");
        return null;
    }

    internal static bool AxisMatches(short expected, short actual)
    {
        if (expected < 0 && actual > 0 || expected > 0 && actual < 0)
            return false;
        return Math.Abs((int)expected - actual) <= AxisTolerance;
    }

    internal static byte QuantizeTrigger(ushort value) =>
        (byte)Math.Round(value * 255.0 / ushort.MaxValue, MidpointRounding.AwayFromZero);

    internal static bool TriggerMatches(ushort expected, byte actual) =>
        Math.Abs(QuantizeTrigger(expected) - actual) <= TriggerTolerance;

    private static void Validate(ExpectedState? state)
    {
        if (state is null)
            throw new ArgumentException("expected a JSON GamepadStateV1 object");
        if (state.Slot > 3)
            throw new ArgumentException("slot must be an XInput user index from 0 through 3");
        if ((state.Buttons & ~0x07ff) != 0)
            throw new ArgumentException("buttons contain bits outside GamepadStateV1");
        if (state.Dpad > 8)
            throw new ArgumentException("dpad must be a GamepadStateV1 value from 0 through 8");
    }

    private static Options ParseOptions(string[] args)
    {
        var timeout = DefaultTimeoutMilliseconds;
        var interval = DefaultPollIntervalMilliseconds;
        for (var index = 0; index < args.Length; index += 2)
        {
            if (index + 1 >= args.Length)
                throw new ArgumentException("usage: xinput-observer [--timeout-ms N] [--poll-interval-ms N] < expected-states.ndjson");
            if (!int.TryParse(args[index + 1], out var value) || value < 1)
                throw new ArgumentException("observer intervals must be positive integers");

            switch (args[index])
            {
                case "--timeout-ms":
                    timeout = value;
                    break;
                case "--poll-interval-ms":
                    interval = value;
                    break;
                default:
                    throw new ArgumentException("usage: xinput-observer [--timeout-ms N] [--poll-interval-ms N] < expected-states.ndjson");
            }
        }
        return new Options(timeout, interval);
    }

    private static int RunSelfTest()
    {
        try
        {
            Assert(QuantizeTrigger(0) == 0, "trigger zero");
            Assert(QuantizeTrigger(ushort.MaxValue) == byte.MaxValue, "trigger maximum");
            Assert(QuantizeTrigger(32768) == 128, "trigger midpoint rounds to nearest XInput byte");
            Assert(AxisMatches(-1, -2), "one axis unit is quantization noise");
            Assert(!AxisMatches(-1, 1), "axis sign must survive quantization");
            Assert(!AxisMatches(0, 2), "axis error larger than one unit fails");
            Assert(ObservedState.ProtocolButtonsFromXInput(Native.A | Native.Back | Native.LeftShoulder | Native.Guide) == 0x0511, "button mapping");
            Assert(ObservedState.DpadFromButtons(Native.DpadUp | Native.DpadRight) == 2, "northeast dpad");
        }
        catch (InvalidOperationException error)
        {
            Console.Error.WriteLine($"xinput-observer self-test: {error.Message}");
            Console.Out.WriteLine("{\"type\":\"xinput-observer-self-test\",\"verdict\":\"fail\"}");
            return 1;
        }

        Console.Out.WriteLine("{\"type\":\"xinput-observer-self-test\",\"assertions\":8,\"verdict\":\"pass\"}");
        return 0;
    }

    private static void Assert(bool condition, string name)
    {
        if (!condition)
            throw new InvalidOperationException($"failed assertion: {name}");
    }

    private static void WriteSummary(Summary summary) =>
        Console.Out.WriteLine(JsonSerializer.Serialize(summary, JsonOptions));

    private sealed record Options(int TimeoutMilliseconds, int PollIntervalMilliseconds);

    private sealed class ExpectedState
    {
        [JsonPropertyName("controller_slot")]
        public uint Slot { get; init; }
        [JsonPropertyName("sequence")]
        public uint Sequence { get; init; }
        [JsonPropertyName("buttons")]
        public ushort Buttons { get; init; }
        [JsonPropertyName("dpad")]
        public byte Dpad { get; init; }
        [JsonPropertyName("session_generation")]
        public uint SessionGeneration { get; init; }
        public short LeftX { get; init; }
        [JsonPropertyName("left_y")]
        public short LeftY { get; init; }
        [JsonPropertyName("right_x")]
        public short RightX { get; init; }
        [JsonPropertyName("right_y")]
        public short RightY { get; init; }
        [JsonPropertyName("left_trigger")]
        public ushort LeftTrigger { get; init; }
        [JsonPropertyName("right_trigger")]
        public ushort RightTrigger { get; init; }
    }

    private sealed class Summary
    {
        [JsonPropertyName("type")]
        public string Type { get; init; } = "xinput-observer-summary";
        [JsonPropertyName("expected_states")]
        public int ExpectedStates { get; set; }
        [JsonPropertyName("observed_states")]
        public int ObservedStates { get; set; }
        [JsonPropertyName("matched_states")]
        public int MatchedStates { get; set; }
        [JsonPropertyName("mismatched_states")]
        public int MismatchedStates { get; set; }
        [JsonPropertyName("unavailable_states")]
        public int UnavailableStates { get; set; }
        [JsonPropertyName("timeout_states")]
        public int TimeoutStates { get; set; }
        [JsonPropertyName("xinput_errors")]
        public int XInputErrors { get; set; }
        [JsonPropertyName("invalid_states")]
        public int InvalidStates { get; set; }
        [JsonPropertyName("first_mismatch")]
        public ComparisonFailure? FirstMismatch { get; set; }
        [JsonPropertyName("error")]
        public string? Error { get; set; }
        [JsonPropertyName("verdict")]
        public string Verdict { get; set; } = "fail";
    }

    private sealed class ComparisonFailure
    {
        [JsonPropertyName("reason")]
        public required string Reason { get; init; }
        [JsonPropertyName("sequence")]
        public uint Sequence { get; init; }
        [JsonPropertyName("slot")]
        public uint Slot { get; init; }
        [JsonPropertyName("expected")]
        public ExpectedState Expected { get; init; } = null!;
        [JsonPropertyName("actual")]
        public ObservedState? Actual { get; init; }
        [JsonPropertyName("xinput_status")]
        public uint? XInputStatus { get; init; }

        public static ComparisonFailure For(ExpectedState expected, ObservedState actual, string reason) => new()
        {
            Reason = reason,
            Sequence = expected.Sequence,
            Slot = expected.Slot,
            Expected = expected,
            Actual = actual,
        };

        public static ComparisonFailure Unavailable(ExpectedState expected, uint status) => new()
        {
            Reason = "XInputGetState did not return a controller state",
            Sequence = expected.Sequence,
            Slot = expected.Slot,
            Expected = expected,
            XInputStatus = status,
        };
    }

    private sealed class ObservationResult(bool matched, bool unavailable, ComparisonFailure failure)
    {
        public static readonly ObservationResult Match = new(true, false, null!);
        public bool Matched { get; } = matched;
        public bool Unavailable { get; } = unavailable;
        public ComparisonFailure Failure { get; } = failure;
    }

    private sealed class ObservedState
    {
        [JsonPropertyName("buttons")]
        public required ushort Buttons { get; init; }
        [JsonPropertyName("dpad")]
        public required byte Dpad { get; init; }
        [JsonPropertyName("left_x")]
        public required short LeftX { get; init; }
        [JsonPropertyName("left_y")]
        public required short LeftY { get; init; }
        [JsonPropertyName("right_x")]
        public required short RightX { get; init; }
        [JsonPropertyName("right_y")]
        public required short RightY { get; init; }
        [JsonPropertyName("left_trigger")]
        public required byte LeftTrigger { get; init; }
        [JsonPropertyName("right_trigger")]
        public required byte RightTrigger { get; init; }

        public static ObservedState FromNative(Native.Gamepad state) => new()
        {
            Buttons = ProtocolButtonsFromXInput(state.Buttons),
            Dpad = DpadFromButtons(state.Buttons),
            LeftX = state.LeftX,
            LeftY = state.LeftY,
            RightX = state.RightX,
            RightY = state.RightY,
            LeftTrigger = state.LeftTrigger,
            RightTrigger = state.RightTrigger,
        };

        internal static ushort ProtocolButtonsFromXInput(ushort buttons)
        {
            ushort protocol = 0;
            if ((buttons & Native.A) != 0) protocol |= 1 << 0;
            if ((buttons & Native.B) != 0) protocol |= 1 << 1;
            if ((buttons & Native.X) != 0) protocol |= 1 << 2;
            if ((buttons & Native.Guide) != 0) protocol |= 1 << 10;
            if ((buttons & Native.Y) != 0) protocol |= 1 << 3;
            if ((buttons & Native.LeftShoulder) != 0) protocol |= 1 << 4;
            if ((buttons & Native.RightShoulder) != 0) protocol |= 1 << 5;
            if ((buttons & Native.LeftThumb) != 0) protocol |= 1 << 6;
            if ((buttons & Native.RightThumb) != 0) protocol |= 1 << 7;
            if ((buttons & Native.Back) != 0) protocol |= 1 << 8;
            if ((buttons & Native.Start) != 0) protocol |= 1 << 9;
            return protocol;
        }

        internal static byte DpadFromButtons(ushort buttons)
        {
            var up = (buttons & Native.DpadUp) != 0;
            var down = (buttons & Native.DpadDown) != 0;
            var left = (buttons & Native.DpadLeft) != 0;
            var right = (buttons & Native.DpadRight) != 0;
            return (up, down, left, right) switch
            {
                (true, false, false, false) => 1,
                (true, false, false, true) => 2,
                (false, false, false, true) => 3,
                (false, true, false, true) => 4,
                (false, true, false, false) => 5,
                (false, true, true, false) => 6,
                (false, false, true, false) => 7,
                (true, false, true, false) => 8,
                _ => 0,
            };
        }
    }

    private static class Native
    {
        internal const uint ErrorSuccess = 0;
        internal const ushort DpadUp = 0x0001;
        internal const ushort Guide = 0x0400;
        internal const ushort DpadDown = 0x0002;
        internal const ushort DpadLeft = 0x0004;
        internal const ushort DpadRight = 0x0008;
        internal const ushort Start = 0x0010;
        internal const ushort Back = 0x0020;
        internal const ushort LeftThumb = 0x0040;
        internal const ushort RightThumb = 0x0080;
        internal const ushort LeftShoulder = 0x0100;
        internal const ushort RightShoulder = 0x0200;
        internal const ushort A = 0x1000;
        internal const ushort B = 0x2000;
        internal const ushort X = 0x4000;
        internal const ushort Y = 0x8000;

        [DllImport("xinput1_4.dll", ExactSpelling = true)]
        internal static extern uint XInputGetState(uint userIndex, out State state);

        [StructLayout(LayoutKind.Sequential)]
        internal struct State
        {
            internal uint PacketNumber;
            internal Gamepad Gamepad;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct Gamepad
        {
            internal ushort Buttons;
            internal byte LeftTrigger;
            internal byte RightTrigger;
            internal short LeftX;
            internal short LeftY;
            internal short RightX;
            internal short RightY;
        }
    }
}
