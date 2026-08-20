param(
    [string]$BridgeExecutable = "..\hidmaestro-bridge\bin\Debug\net10.0-windows10.0.26100.0\win-x64\HidMaestroBridge.exe",
    [string]$ObserverExecutable = ".\bin\Debug\net10.0-windows10.0.26100.0\win-x64\XInputObserver.exe"
)

$bridgePath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot $BridgeExecutable))
$observerPath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot $ObserverExecutable))

$bridgeStart = [System.Diagnostics.ProcessStartInfo]::new($bridgePath)
$bridgeStart.UseShellExecute = $false
$bridgeStart.RedirectStandardInput = $true
$bridgeStart.RedirectStandardOutput = $true
$bridgeStart.RedirectStandardError = $true
$bridge = [System.Diagnostics.Process]::new()
$bridge.StartInfo = $bridgeStart
if (-not $bridge.Start()) { throw "cannot start $bridgePath" }
try {
    if ($bridge.StandardOutput.ReadLine() -ne "ready") { throw "bridge did not become ready" }
    $bridge.StandardInput.WriteLine("create 0")
    if ($bridge.StandardOutput.ReadLine() -ne "ok") { throw "bridge refused create" }
    $states = @(
        @{ buttons = 1; dpad = 0; left_x = -32767; left_y = 0; right_x = 32767; right_y = 0; left_trigger = 0; right_trigger = 65535 },
        @{ buttons = 2; dpad = 2; left_x = -16384; left_y = 16384; right_x = 16384; right_y = -16384; left_trigger = 16384; right_trigger = 49152 },
        @{ buttons = 4; dpad = 4; left_x = 0; left_y = 0; right_x = 0; right_y = 0; left_trigger = 32768; right_trigger = 32768 },
        @{ buttons = 8; dpad = 6; left_x = 16384; left_y = -16384; right_x = -16384; right_y = 16384; left_trigger = 49152; right_trigger = 16384 },
        @{ buttons = 0; dpad = 8; left_x = 32767; left_y = -32767; right_x = -32767; right_y = 32767; left_trigger = 65535; right_trigger = 0 }
    )

    $sequence = 1
    $summaries = @()
    foreach ($state in $states) {
        $bridge.StandardInput.WriteLine(
            "state 0 $($state['left_x']) $($state['left_y']) $($state['right_x']) $($state['right_y']) $($state['left_trigger']) $($state['right_trigger']) $($state['buttons']) $($state['dpad'])"
        )
        if ($bridge.StandardOutput.ReadLine() -ne "ok") { throw "bridge refused state" }
        Start-Sleep -Milliseconds 500

        $observerStart = [System.Diagnostics.ProcessStartInfo]::new($observerPath)
        $observerStart.UseShellExecute = $false
        $observerStart.RedirectStandardInput = $true
        $observerStart.RedirectStandardOutput = $true
        $observerStart.RedirectStandardError = $true
        $observer = [System.Diagnostics.Process]::new()
        $observer.StartInfo = $observerStart
        if (-not $observer.Start()) { throw "cannot start $observerPath" }
        $expected = @{
            session_generation = 1
            controller_slot = 0
            sequence = $sequence
            buttons = $state['buttons']
            dpad = $state['dpad']
            left_x = $state['left_x']
            left_y = -$state['left_y']
            right_x = $state['right_x']
            right_y = -$state['right_y']
            left_trigger = $state['left_trigger']
            right_trigger = $state['right_trigger']
        } | ConvertTo-Json -Compress
        $observer.StandardInput.WriteLine($expected)
        $observer.StandardInput.Close()
        $observer.WaitForExit()
        $output = $observer.StandardOutput.ReadToEnd().Trim()
        $stderr = $observer.StandardError.ReadToEnd().Trim()
        if ($observer.ExitCode -ne 0) { throw "XInput observer failed: $output $stderr" }
        $summaries += ($output | ConvertFrom-Json)
        $sequence++
    }
    [pscustomobject]@{
        type = "xinput-conformance-summary"
        states = $summaries.Count
        matched_states = @($summaries | Where-Object verdict -eq "pass").Count
        mismatched_states = @($summaries | Where-Object verdict -ne "pass").Count
        verdict = if (@($summaries | Where-Object verdict -ne "pass").Count -eq 0) { "pass" } else { "fail" }
    } | ConvertTo-Json -Compress
}
finally {
    if (-not $bridge.HasExited) {
        $bridge.StandardInput.WriteLine("destroy 0")
        $null = $bridge.StandardOutput.ReadLine()
        $bridge.StandardInput.WriteLine("quit")
        $null = $bridge.StandardOutput.ReadLine()
        $bridge.WaitForExit()
    }
}
