# Rebuilds the indirect display driver solution, Release x64, and nothing else.
#
# It compiles whatever `windows\idd-lab\IddSampleDriver.sln` contains - the
# driver and its INF, the controller that creates the software device, and any
# sender kept beside them - and prints the binaries it produced, which is the
# manifest of what a later install would stage. A rebuild rather than a build,
# because an object file left over from an earlier header is indistinguishable
# in the output from a driver that has the new interface compiled in, and that
# mistake costs a measurement session to notice.
#
# What it does not do, deliberately: it neither signs, stages, installs nor
# replaces the package, it never restarts the controller, and so it never
# creates or removes the virtual monitor. Every video measurement in the
# laboratory needs the IDD-LAB monitor to exist, so replacing what is installed
# has to be a separate act, taken with a copy of the current package in hand and
# followed by a check that the monitor came back. A build script that quietly
# reinstalled would be able to leave the host with no display and a green log.
#
# One consequence of not touching the running lab is worth stating before it is
# met: a rebuild relinks the controller, and while the virtual display is up that
# executable is the running process that owns the software device, so the link
# fails with LNK1104 and this script exits non-zero with the driver itself built
# and the controller not. That is a lab that is up, not a broken solution. Stop
# LanPlayIddLabCtl and bring it back with `ensure-lab-source.ps1` if a fresh
# controller is what is wanted.
#
# Kept here because for a long while it existed only at
# C:\Users\luque\build-idd-lab.ps1, which meant the driver every latency number
# in this project was measured through had been built by a script nobody could
# read. Copied to that path the way `ensure-lab-source.ps1` is, and run from
# there rather than out of the repository, so that a build never depends on a
# sync having happened first.
#
# Compiling wants no desktop, so unlike `ensure-lab-source.ps1` this needs no
# scheduled task and runs over plain ssh:
#   tools/win-ssh.sh 'powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\luque\build-idd-lab.ps1'

$ErrorActionPreference = 'Stop'
$repo = 'C:\Users\luque\lanplay-rs'
$solution = Join-Path $repo 'windows\idd-lab\IddSampleDriver.sln'
$vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'

# Located rather than hardcoded: MSBuild moves with every Visual Studio update,
# and vswhere is the only path Microsoft supports for finding it. Each step is
# checked so that a missing toolchain says which piece is missing; without the
# checks an absent vswhere leaves $msbuild built from an empty string, and the
# invocation then fails with a message about a program that has no name.
if (-not (Test-Path $solution)) { throw "no solution at $solution; sync the repository first" }
if (-not (Test-Path $vswhere)) { throw "no vswhere at $vswhere; Visual Studio is not installed" }

$vs = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild -property installationPath
if (-not $vs) { throw 'vswhere found no Visual Studio installation carrying MSBuild' }

$msbuild = Join-Path $vs 'MSBuild\Current\Bin\amd64\MSBuild.exe'
if (-not (Test-Path $msbuild)) { throw "no MSBuild at $msbuild" }

# Said before the build rather than after it, because MSBuild's own account of
# this is one LNK1104 line among a hundred, and a caller reading a non-zero exit
# needs to know which of the two failures it is looking at.
if (Get-Process LanPlayIddLabCtl -ErrorAction SilentlyContinue) {
    Write-Output 'the lab display is up, so the controller cannot be relinked; expect LNK1104 on it'
}

& $msbuild $solution /m /t:Rebuild /p:Configuration=Release /p:Platform=x64 /verbosity:minimal
$built = $LASTEXITCODE

# Every binary under the tree, with its size, printed even after a failure and
# with MSBuild's exit code kept. A driver that compiled but was not copied where
# the INF expects it looks identical to a success in MSBuild's own output, the
# sizes are how a rebuild is told from a no-op, and the ordinary failure in a live
# laboratory is the controller alone: the listing is then the only thing that says
# the driver itself came out.
#
# Formatted through `Out-String` because the table formatter is asynchronous: it
# buffers rows to work out its column widths, and `exit` ends the process before
# that buffer is flushed, so the manifest vanishes from a redirected log while
# appearing perfectly on a console. That cost a run to notice.
Get-ChildItem (Join-Path $repo 'windows\idd-lab') -Recurse -File |
    Where-Object { $_.Extension -in '.dll', '.exe', '.inf', '.cat', '.cer' } |
    Select-Object FullName, Length |
    Format-Table -AutoSize |
    Out-String |
    Write-Output

exit $built
