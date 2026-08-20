Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class XInputRumble {
    [StructLayout(LayoutKind.Sequential)]
    public struct Vibration { public ushort Low; public ushort High; }
    [DllImport("xinput1_4.dll")]
    public static extern uint XInputSetState(uint index, ref Vibration vibration);
}
'@
for ($index = 0; $index -lt 5; $index++) {
    $vibration = [XInputRumble+Vibration]::new()
    $vibration.Low = [uint16](($index + 1) * 12000)
    $vibration.High = [uint16](($index + 1) * 10000)
    $result = [XInputRumble]::XInputSetState(0, [ref]$vibration)
    Write-Output "rumble $index result=$result low=$($vibration.Low) high=$($vibration.High)"
    Start-Sleep -Milliseconds 1000
}
$vibration = [XInputRumble+Vibration]::new()
$result = [XInputRumble]::XInputSetState(0, [ref]$vibration)
Write-Output "rumble stop result=$result"
