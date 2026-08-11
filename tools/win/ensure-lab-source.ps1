# Brings up whatever the 1080p120 laboratory source needs, idempotently.
#
# Two processes stand behind every run: the IDD-LAB controller, which owns
# the software device and therefore the virtual monitor - the display exists
# for exactly as long as that process does - and a full-screen producer on
# that monitor, because Desktop Duplication only hands over a frame when the
# desktop actually changed.
#
# A sweep is twenty-one runs over half an hour. Depending on something
# started by hand before it began is how a sweep ends with a column of zeroes
# and no idea when they started.

$ErrorActionPreference = 'Stop'
$repo = 'C:\Users\luque\lanplay-rs'
$controller = Join-Path $repo 'windows\idd-lab\x64\Release\LanPlayIddLabCtl.exe'
$present = Join-Path $repo 'target\release\present-source.exe'

function Start-Missing {
    param($Name, $Path, $Arguments, $Prefix)

    $running = Get-Process $Name -ErrorAction SilentlyContinue
    if ($running) {
        Write-Output "$Name already running (pid $($running.Id))"
        return $running
    }
    # Splatted rather than passed positionally: Start-Process rejects an
    # empty -ArgumentList outright, and the controller takes no arguments.
    $parameters = @{
        FilePath               = $Path
        WindowStyle            = 'Hidden'
        PassThru               = $true
        RedirectStandardOutput = "$Prefix.stdout.log"
        RedirectStandardError  = "$Prefix.stderr.log"
    }
    if ($Arguments -and $Arguments.Count -gt 0) { $parameters.ArgumentList = $Arguments }
    $process = Start-Process @parameters
    Start-Sleep -Seconds 4
    if ($process.HasExited) { throw "$Name exited immediately with $($process.ExitCode)" }
    Write-Output "$Name started (pid $($process.Id))"
    return $process
}

Start-Missing -Name 'LanPlayIddLabCtl' -Path $controller -Prefix 'C:\Users\luque\idd-lab' | Out-Null

# The monitor arrives asynchronously after the device does; asking DXGI too
# early gets an honest "no such output" that has nothing to do with the driver.
$deadline = (Get-Date).AddSeconds(20)
do {
    $monitor = Get-CimInstance Win32_VideoController |
        Where-Object { $_.PNPDeviceID -like '*LANPLAYIDDLAB*' -and $_.CurrentRefreshRate }
    if ($monitor) { break }
    Start-Sleep -Milliseconds 500
} while ((Get-Date) -lt $deadline)

if (-not $monitor) { throw 'the IDD-LAB monitor never became active' }
Write-Output ("monitor {0}x{1}@{2}" -f $monitor.CurrentHorizontalResolution,
    $monitor.CurrentVerticalResolution, $monitor.CurrentRefreshRate)

Start-Missing -Name 'present-source' -Path $present `
    -Arguments @('--width', '1920', '--height', '1080', '--fps', '120', '--seconds', '0',
        '--fullscreen', '--monitor', '1') `
    -Prefix 'C:\Users\luque\idd-present' | Out-Null
