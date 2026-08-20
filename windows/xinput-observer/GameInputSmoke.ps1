$bridgePath = Join-Path $PSScriptRoot '..\hidmaestro-bridge\bin\Debug\net10.0-windows10.0.26100.0\win-x64\HidMaestroBridge.exe'
$observerPath = Join-Path $PSScriptRoot 'gameinput-observer.exe'
function Start-Redirected($path) {
    $info = [System.Diagnostics.ProcessStartInfo]::new($path)
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $info
    if (-not $process.Start()) { throw "cannot start $path" }
    $process
}
$bridge = Start-Redirected $bridgePath
try {
    if ($bridge.StandardOutput.ReadLine() -ne 'ready') { throw 'bridge not ready' }
    $bridge.StandardInput.WriteLine('create 0')
    if ($bridge.StandardOutput.ReadLine() -ne 'ok') { throw 'create refused' }
    $bridge.StandardInput.WriteLine('state 0 -32767 0 32767 0 0 65535 1 0')
    if ($bridge.StandardOutput.ReadLine() -ne 'ok') { throw 'state refused' }
    $observer = Start-Redirected $observerPath
    $observer.WaitForExit()
    $output = $observer.StandardOutput.ReadToEnd().Trim()
    $stderr = $observer.StandardError.ReadToEnd().Trim()
    if ($observer.ExitCode -ne 0) { throw "GameInput observer failed: $output $stderr" }
    $output
} finally {
    if ($bridge -and -not $bridge.HasExited) {
        $bridge.StandardInput.WriteLine('destroy 0')
        $null = $bridge.StandardOutput.ReadLine()
        $bridge.StandardInput.WriteLine('quit')
        $null = $bridge.StandardOutput.ReadLine()
        $bridge.WaitForExit()
    }
}
