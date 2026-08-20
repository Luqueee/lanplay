using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Globalization;
using System.Net;
using System.Net.Sockets;
using HIDMaestro;

var feedbackTarget = ParseFeedbackTarget(args);
var feedbackSession = ParseFeedbackSession(args);
var feedback = feedbackTarget is null ? null : new FeedbackSink(feedbackTarget, feedbackSession);
using var context = new HMContext();
context.LoadDefaultProfiles();
context.InstallDriver();
HMContext.RemoveAllVirtualControllers();
var profile = context.GetProfile("xbox-360-wired")
    ?? throw new InvalidOperationException("HIDMaestro lacks xbox-360-wired");
var controllers = new Dictionary<byte, HMController>();
Console.Out.WriteLine("ready");
Console.Out.Flush();

try
{
    string? line;
    while ((line = Console.In.ReadLine()) is not null)
    {
        var fields = line.Split(' ', StringSplitOptions.RemoveEmptyEntries);
        if (fields.Length == 0)
            continue;
        switch (fields[0])
        {
            case "create" when fields.Length is 2 or 3:
            {
                var slot = ParseByte(fields[1]);
                var generation = fields.Length == 3 ? ParseUInt32(fields[2]) : 0;
                if (controllers.ContainsKey(slot))
                    throw new InvalidOperationException($"slot {slot} already exists");
                var controller = context.CreateController(profile);
                if (feedback is not null)
                {
                    controller.OutputReceived += (_, packet) =>
                        feedback.Send(slot, generation, packet);
                }
                controllers.Add(slot, controller);
                Reply("ok");
                break;
            }
            case "state" when fields.Length == 10:
            {
                var slot = ParseByte(fields[1]);
                if (!controllers.TryGetValue(slot, out var controller))
                    throw new InvalidOperationException($"slot {slot} is absent");
                var state = new HMGamepadState
                {
                    Axes = HMGamepadStateHelpers.StandardAxes(
                        controller.Profile,
                        Stick(ParseInt16(fields[2])), Stick(ParseInt16(fields[3])),
                        Stick(ParseInt16(fields[4])), Stick(ParseInt16(fields[5])),
                        Trigger(ParseUInt16(fields[6])), Trigger(ParseUInt16(fields[7]))),
                    Buttons = (HMButton)ParseUInt16(fields[8]),
                    Hat = (HMHat)ParseByte(fields[9]),
                };
                controller.SubmitState(in state);
                Reply("ok");
                break;
            }
            case "destroy" when fields.Length == 2:
            {
                var slot = ParseByte(fields[1]);
                if (!controllers.Remove(slot, out var controller))
                    throw new InvalidOperationException($"slot {slot} is absent");
                controller.Dispose();
                Reply("ok");
                break;
            }
            case "quit" when fields.Length == 1:
                Reply("ok");
                return;
            default:
                throw new InvalidOperationException("invalid bridge command");
        }
    }
}
finally
{
    foreach (var controller in controllers.Values)
        controller.Dispose();
}

static IPEndPoint? ParseFeedbackTarget(string[] arguments)
{
    var index = Array.IndexOf(arguments, "--feedback");
    return index >= 0 && index + 1 < arguments.Length
        ? IPEndPoint.Parse(arguments[index + 1])
        : null;
}

static uint ParseFeedbackSession(string[] arguments)
{
    var index = Array.IndexOf(arguments, "--session");
    return index >= 0 && index + 1 < arguments.Length
        ? ParseUInt32(arguments[index + 1])
        : 1;
}


static uint ParseUInt32(string value) => uint.Parse(value, CultureInfo.InvariantCulture);
static float Stick(short value) => Math.Clamp((value + 32767f) / 65534f, 0f, 1f);
static float Trigger(ushort value) => value / (float)ushort.MaxValue;
static byte ParseByte(string value) => byte.Parse(value, CultureInfo.InvariantCulture);
static short ParseInt16(string value) => short.Parse(value, CultureInfo.InvariantCulture);
static ushort ParseUInt16(string value) => ushort.Parse(value, CultureInfo.InvariantCulture);
static void Reply(string message)
{
    Console.Out.WriteLine(message);
    Console.Out.Flush();
}
sealed class FeedbackSink
{
    private readonly UdpClient client = new();
    private readonly IPEndPoint target;
    private readonly uint session;
    private uint datagramSequence;
    private readonly uint[] stateSequences = new uint[4];

    public FeedbackSink(IPEndPoint target, uint session)
    {
        this.target = target;
        this.session = session;
    }

    public void Send(byte slot, uint generation, HMOutputPacket packet)
    {
        if (packet.Source != HMOutputSource.XInput || packet.Data.Length < 4)
            return;
        var data = packet.Data.Span;
        var low = (ushort)(data[2] * 257);
        var high = (ushort)(data[3] * 257);
        var stateSequence = ++stateSequences[slot];
        var output = new byte[33];
        output[0] = 2;
        output[1] = 12;
        BinaryPrimitives.WriteUInt32BigEndian(output.AsSpan(4), session);
        BinaryPrimitives.WriteUInt32BigEndian(output.AsSpan(8), datagramSequence++);
        BinaryPrimitives.WriteUInt32BigEndian(output.AsSpan(20), generation);
        output[24] = slot;
        BinaryPrimitives.WriteUInt32BigEndian(output.AsSpan(25), stateSequence);
        BinaryPrimitives.WriteUInt16BigEndian(output.AsSpan(29), low);
        BinaryPrimitives.WriteUInt16BigEndian(output.AsSpan(31), high);
        client.Send(output, output.Length, target);
    }
}
