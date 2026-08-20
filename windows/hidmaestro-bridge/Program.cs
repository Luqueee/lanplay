using System;
using System.Collections.Generic;
using System.Globalization;
using HIDMaestro;

using var context = new HMContext();
context.LoadDefaultProfiles();
context.InstallDriver();
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
            case "create" when fields.Length == 2:
            {
                var slot = ParseByte(fields[1]);
                if (controllers.ContainsKey(slot))
                    throw new InvalidOperationException($"slot {slot} already exists");
                controllers.Add(slot, context.CreateController(profile));
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
