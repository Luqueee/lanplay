//! What this machine, and the lab host, can satisfy right now.
//!
//! Almost everything here shells out, so almost nothing here is testable on an
//! arbitrary machine; the parsing of what the probes said is separated out for
//! exactly that reason and is tested. The exception is the hardware-decode
//! question, which is a library call into `lanplay-capabilities` rather than a
//! child process, because the client's preflight asks VideoToolbox the same
//! thing and two copies of one query are two chances to disagree. The verdicts
//! built on top of this live in `gates`, where they can be exercised with the
//! host switched off.
//!
//! Two rules run through all of it. Nothing may hang: a listing that blocks
//! for half a minute on a host that is not there is a listing nobody runs, so
//! every probe carries its own wall-clock deadline and is killed at it. And a
//! probe that could not be taken reports `Unknown`, never `Absent`: the four
//! host-side requirements are properties of a machine that did not answer, and
//! nobody looked at them.

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use lanplay_protocol::VideoCodec;

use crate::gates::{Environment, Satisfaction};

/// The Wi-Fi interface a radio gate sends over. Fixed by the rig, like the
/// host name, and named here rather than guessed from the routing table.
pub const WIFI_INTERFACE: &str = "en0";

/// Long enough for a host that is awake on this LAN to complete a TCP
/// handshake and a key exchange, short enough that a listing against a host
/// that is switched off still returns while somebody is looking at it. The
/// unreachable case costs the full five seconds and nothing costs more.
const SSH_DEADLINE: Duration = Duration::from_secs(5);

/// The capability query is one PowerShell start-up plus four cheap lookups,
/// and PowerShell alone is a second on this host. Ten seconds is slack for a
/// loaded machine rather than a budget anything is expected to use.
const HOST_QUERY_DEADLINE: Duration = Duration::from_secs(10);

/// Local probes answer in a fifth of a second when they answer at all.
const LOCAL_DEADLINE: Duration = Duration::from_secs(5);

/// A panel below this is not a 120 Hz panel; a panel at 120 Hz reports itself
/// as 119.88 or 119.97 as often as as 120.00, because the mode is derived from
/// a pixel clock and not from the round number in the marketing.
const HIGH_REFRESH_HZ: f64 = 119.0;

/// Everything the index asked about, and nothing else. Requirements are
/// detected in dependency order: the host answers first because four other
/// requirements are properties of that host and one more is a property of the
/// path to it.
pub fn detect(host: &str, wanted: &BTreeSet<&str>) -> Environment {
    let host_side = [
        "nvidia-nvenc",
        "virtual-display",
        "lab-source",
        "audio-endpoint",
    ];
    let needs_host = wanted.contains("windows-host")
        || wanted.contains("radio")
        || host_side.iter().any(|name| wanted.contains(name));

    let reachable = needs_host.then(|| host_reachable(host));
    let mut found: BTreeMap<&str, Satisfaction> = BTreeMap::new();

    if let Some(reachable) = &reachable {
        found.insert("windows-host", reachable.clone());
    }

    if host_side.iter().any(|name| wanted.contains(name)) {
        let asked = match &reachable {
            Some(Satisfaction::Present(_)) => host_capabilities(host),
            // Not absent: a property of a machine nobody reached is a property
            // nobody looked at, and a gate skipped for a reason nobody checked
            // is how a suite quietly shrinks.
            Some(other) => host_side
                .iter()
                .map(|name| {
                    (
                        *name,
                        Satisfaction::Unknown(format!("the host was not asked: {}", other.why())),
                    )
                })
                .collect(),
            None => BTreeMap::new(),
        };
        for name in host_side {
            if !wanted.contains(name) {
                continue;
            }
            let answer = asked.get(name).cloned().unwrap_or_else(|| {
                Satisfaction::Unknown("the host answered but said nothing about this".to_string())
            });
            found.insert(name, answer);
        }
    }

    if wanted.contains("radio") {
        found.insert("radio", radio(&reachable));
    }
    if wanted.contains("mac-display") {
        found.insert("mac-display", mac_display());
    }
    if wanted.contains("mac-h264-decode") {
        found.insert("mac-h264-decode", mac_h264_decode());
    }
    if wanted.contains("audio-output") {
        found.insert("audio-output", audio_output());
    }
    if wanted.contains("quiet-machine") {
        found.insert("quiet-machine", quiet_machine());
    }
    if wanted.contains("human-attention") {
        found.insert(
            "human-attention",
            // Never satisfiable by a probe, and that is the point of naming it
            // in the index: an agent working unattended has to be able to tell
            // these apart from the rest mechanically rather than by reading
            // the harness and forming an opinion.
            Satisfaction::Absent(
                "a person has to be at the machine; no probe can stand in for one".to_string(),
            ),
        );
    }

    // Anything the index requires that this program has no probe for stays out
    // of the map, and `Gate::verdict` reports it as unknown rather than met.
    found
        .into_iter()
        .map(|(name, satisfaction)| (name.to_string(), satisfaction))
        .collect()
}

fn host_reachable(host: &str) -> Satisfaction {
    match ssh(host, "ver", SSH_DEADLINE) {
        Ok(output) if output.status.success() => {
            Satisfaction::Present(format!("ssh {host} answered"))
        }
        Ok(output) => Satisfaction::Absent(format!(
            "ssh {host} failed: {}",
            crate::preflight::last_line(&output)
        )),
        Err(why) => Satisfaction::Absent(format!("ssh {host}: {why}")),
    }
}

/// Traffic addressed to this machine's own routable address never reaches the
/// air: the kernel short-circuits it onto loopback and the run measures a
/// memory copy. So a radio gate needs an address on the Wi-Fi interface *and*
/// a second machine to send to, and an hour was spent finding that out the
/// other way round.
fn radio(host: &Option<Satisfaction>) -> Satisfaction {
    let address = match run("ipconfig", &["getifaddr", WIFI_INTERFACE], LOCAL_DEADLINE) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        Ok(_) => String::new(),
        Err(why) => {
            return Satisfaction::Unknown(format!("could not read {WIFI_INTERFACE}: {why}"));
        }
    };
    if address.is_empty() {
        return Satisfaction::Absent(format!("{WIFI_INTERFACE} has no address"));
    }
    match host {
        Some(Satisfaction::Present(_)) => Satisfaction::Present(format!(
            "{WIFI_INTERFACE} is {address} and the host answers"
        )),
        Some(other) => Satisfaction::Absent(format!(
            "{WIFI_INTERFACE} is {address} but there is no second endpoint: {}",
            other.why()
        )),
        None => Satisfaction::Unknown(format!(
            "{WIFI_INTERFACE} is {address} but the host was not checked"
        )),
    }
}

fn mac_display() -> Satisfaction {
    let output = match run(
        "system_profiler",
        &["-json", "SPDisplaysDataType"],
        LOCAL_DEADLINE,
    ) {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return Satisfaction::Unknown(format!(
                "system_profiler failed: {}",
                crate::preflight::last_line(&output)
            ));
        }
        Err(why) => return Satisfaction::Unknown(format!("system_profiler: {why}")),
    };
    match parse_displays(&String::from_utf8_lossy(&output.stdout)) {
        Ok(Some(found)) => Satisfaction::Present(found),
        Ok(None) => Satisfaction::Absent(format!(
            "no display offers {HIGH_REFRESH_HZ:.0} Hz or better"
        )),
        Err(why) => Satisfaction::Unknown(why),
    }
}

/// The claim the whole video phase rests on: a run against a machine that fell
/// back to software decode is a run measuring something nobody asked about, and
/// nothing downstream would say so. It is detected here rather than asserted in
/// a unit test because a machine without the hardware has not failed anything -
/// it is a machine nobody put the question to, which is what a hosted runner
/// is - and the honest answers to that are `Absent` and `Unknown`, neither of
/// which a test can express.
///
/// The only probe here that starts no child process, so the only one with no
/// deadline to enforce: it is a library call, and it cannot outlast the
/// VideoToolbox query inside it.
fn mac_h264_decode() -> Satisfaction {
    if !lanplay_capabilities::client_probes_supported() {
        // Not `Absent`. Decoder discovery is implemented for macOS, so any
        // other build has not looked rather than looked and found nothing, and
        // the two must not print the same.
        return Satisfaction::Unknown(
            "hardware-decode discovery is implemented for macOS only".to_string(),
        );
    }
    judge_decoders(&lanplay_capabilities::client().hardware_decode)
}

/// Two absences, stated apart because they mean different things. An empty list
/// is a machine with no hardware decoder at all, which is ordinary on anything
/// that is not a Mac with a working GPU. A list that holds other codecs and not
/// H.264 is a machine whose decoder answered three times and left out the one
/// every gate here encodes, and that is a finding rather than a configuration.
fn judge_decoders(codecs: &[VideoCodec]) -> Satisfaction {
    let named = codecs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if codecs.contains(&VideoCodec::H264) {
        Satisfaction::Present(format!("VideoToolbox decodes {named} in hardware"))
    } else if codecs.is_empty() {
        Satisfaction::Absent("VideoToolbox reports no hardware decoder".to_string())
    } else {
        Satisfaction::Absent(format!(
            "VideoToolbox decodes {named} in hardware but not H.264"
        ))
    }
}

fn audio_output() -> Satisfaction {
    let output = match run(
        "system_profiler",
        &["-json", "SPAudioDataType"],
        LOCAL_DEADLINE,
    ) {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return Satisfaction::Unknown(format!(
                "system_profiler failed: {}",
                crate::preflight::last_line(&output)
            ));
        }
        Err(why) => return Satisfaction::Unknown(format!("system_profiler: {why}")),
    };
    match parse_audio_outputs(&String::from_utf8_lossy(&output.stdout)) {
        Ok(Some(found)) => Satisfaction::Present(found),
        Ok(None) => Satisfaction::Absent("no output device on this machine".to_string()),
        Err(why) => Satisfaction::Unknown(why),
    }
}

/// The refresh rate is stated inside the current mode, as `1512 x 982 @
/// 120.00Hz`, so it is read out of that string rather than from a field of its
/// own, which the tool does not offer.
fn parse_displays(json: &str) -> Result<Option<String>, String> {
    let document: serde_json::Value =
        serde_json::from_str(json).map_err(|err| format!("unreadable display report: {err}"))?;
    let cards = document["SPDisplaysDataType"]
        .as_array()
        .ok_or_else(|| "the display report named no graphics device".to_string())?;
    let mut best: Option<(f64, String)> = None;
    for card in cards {
        let Some(displays) = card["spdisplays_ndrvs"].as_array() else {
            continue;
        };
        for display in displays {
            let Some(mode) = display["_spdisplays_resolution"].as_str() else {
                continue;
            };
            let Some(hz) = refresh_hz(mode) else { continue };
            let name = display["_name"].as_str().unwrap_or("a display");
            if best.as_ref().is_none_or(|(seen, _)| hz > *seen) {
                best = Some((hz, format!("{name} at {mode}")));
            }
        }
    }
    Ok(best
        .filter(|(hz, _)| *hz >= HIGH_REFRESH_HZ)
        .map(|(_, what)| what))
}

fn refresh_hz(mode: &str) -> Option<f64> {
    let (_, after) = mode.rsplit_once('@')?;
    let digits: String = after
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

/// The stream every audio gate here carries. Stated as two numbers rather than
/// taken from `lanplay-audio-codec`, because that crate vendors libopus in C
/// and xtask is built on machines with no cmake to build it with.
const CONTRACT_HZ: u64 = 48_000;
const CONTRACT_CHANNELS: u64 = 2;

/// An entry with a non-zero `coreaudio_device_output` is a device that can
/// play; the input-only microphones in the same list cannot, and a gate that
/// wants something audible needs the former. The same field carries how many
/// channels it plays, and `coreaudio_device_srate` the rate it mixes at.
///
/// The default device's format is stated and not only its name, because the
/// receiver that renders a gate's audio refuses a device that cannot carry the
/// stream, and a listing that said only that output devices exist was true of a
/// machine whose default was a pair of 44100 Hz headphones - which is a run
/// that refuses thirty-seven seconds in rather than a requirement that reads as
/// unmet before it starts.
///
/// It remains present with the caveat in the sentence rather than becoming
/// absent, and that is a judgement about what this requirement names. It names
/// an output device on this Mac, and a default mixing at 44100 Hz is one: it
/// plays. Neither gate that requires it is stopped by that rate either - the
/// A5 render probe generates its tone at whatever rate the device mixes at, and
/// A6 now names its own device instead of inheriting one - so marking it absent
/// would block two gates over a system-wide setting neither of them reads,
/// which is how a suite shrinks without anybody deciding it should. What the
/// reader needs is the format in front of them before they start, and that is
/// what this sentence gives them.
fn parse_audio_outputs(json: &str) -> Result<Option<String>, String> {
    let document: serde_json::Value =
        serde_json::from_str(json).map_err(|err| format!("unreadable audio report: {err}"))?;
    let groups = document["SPAudioDataType"]
        .as_array()
        .ok_or_else(|| "the audio report named no device group".to_string())?;
    let mut count = 0;
    let mut default = None;
    for group in groups {
        let Some(devices) = group["_items"].as_array() else {
            continue;
        };
        for device in devices {
            let channels = device["coreaudio_device_output"].as_u64().unwrap_or(0);
            if channels == 0 {
                continue;
            }
            count += 1;
            let name = device["_name"].as_str().unwrap_or("an output device");
            if device["coreaudio_default_audio_output_device"] == "spaudio_yes" {
                default = Some((
                    name.to_string(),
                    device["coreaudio_device_srate"].as_u64(),
                    channels,
                ));
            }
        }
    }
    if count == 0 {
        return Ok(None);
    }
    Ok(Some(match default {
        Some((name, rate, channels)) => format!(
            "{count} output device(s), {name} by default {}",
            mixes(rate, channels)
        ),
        None => format!("{count} output device(s), none of them the default"),
    }))
}

/// What the default mixes at, and whether that is the stream a gate would send
/// it. A device that did not state its rate is said to have not stated it: the
/// alternative is to assume the contract and report a machine as ready on the
/// strength of a field nobody read.
fn mixes(rate: Option<u64>, channels: u64) -> String {
    match rate {
        Some(rate) if rate == CONTRACT_HZ && channels == CONTRACT_CHANNELS => {
            format!("at {rate} Hz {channels} ch")
        }
        Some(rate) => format!(
            "at {rate} Hz {channels} ch, which cannot carry the {CONTRACT_HZ} Hz \
             {CONTRACT_CHANNELS} ch stream without a converter this project refuses; a gate that \
             names its own device is not held up by it"
        ),
        None => format!(
            "which did not state the rate it mixes at, so whether it can carry the {CONTRACT_HZ} \
             Hz {CONTRACT_CHANNELS} ch stream is not answered here"
        ),
    }
}

/// Cores a cadence measurement needs to itself.
///
/// One for the paced producer, which spins out the tail of every period and has
/// to be resident when its deadline arrives; one for whatever it is being
/// measured against; one for everything else the machine is doing. A run taken
/// with fewer measures the scheduler and reports it as though it were the
/// subject, which is what a three-core hosted runner did to a 120 Hz claim
/// before that claim became a gate.
const FREE_CORES_WANTED: f64 = 3.0;

/// Whether this machine has those cores free.
///
/// A prediction rather than a verdict, and the difference matters. The load
/// average is a run queue length averaged over the last minute, so it lags a
/// build that has just finished and misses one about to start; what it is good
/// for is planning, which is what this listing is for. The gates that need it
/// take their own evidence from the run itself and refuse on that, so a
/// disagreement between this word and a gate's refusal is the two of them
/// answering different questions rather than one of them being wrong.
fn quiet_machine() -> Satisfaction {
    let output = match run(
        "sysctl",
        &["-n", "hw.logicalcpu", "vm.loadavg"],
        LOCAL_DEADLINE,
    ) {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return Satisfaction::Unknown(format!(
                "sysctl failed: {}",
                crate::preflight::last_line(&output)
            ));
        }
        Err(why) => return Satisfaction::Unknown(format!("sysctl: {why}")),
    };
    match parse_headroom(&String::from_utf8_lossy(&output.stdout)) {
        Ok((cores, load)) => judge_headroom(cores, load),
        Err(why) => Satisfaction::Unknown(why),
    }
}

/// `hw.logicalcpu` on the first line and `vm.loadavg` on the second, which
/// prints as `{ 3.80 4.11 3.90 }`. The one-minute figure is the one taken: the
/// five and fifteen minute averages describe a machine that has already stopped
/// being the one a gate is about to run on.
fn parse_headroom(text: &str) -> Result<(u32, f64), String> {
    let mut lines = text.lines();
    let cores = lines
        .next()
        .and_then(|line| line.trim().parse::<u32>().ok())
        .ok_or_else(|| format!("sysctl named no core count: {text:?}"))?;
    let load = lines
        .next()
        .and_then(|line| {
            line.split(|c: char| !(c.is_ascii_digit() || c == '.'))
                .find(|field| !field.is_empty())
                .and_then(|field| field.parse::<f64>().ok())
        })
        .ok_or_else(|| format!("sysctl named no load average: {text:?}"))?;
    Ok((cores, load))
}

fn judge_headroom(cores: u32, load: f64) -> Satisfaction {
    let free = f64::from(cores) - load;
    if free >= FREE_CORES_WANTED {
        Satisfaction::Present(format!(
            "{cores} cores at a load of {load:.2}, so about {free:.1} are free"
        ))
    } else {
        Satisfaction::Absent(format!(
            "{cores} cores at a load of {load:.2} leaves about {free:.1} free, and a cadence \
             needs {FREE_CORES_WANTED:.0}"
        ))
    }
}

/// One round trip for all four host-side requirements. Four would each pay for
/// a connection and a PowerShell start-up, which is most of a listing's whole
/// budget spent on the same handshake four times.
fn host_capabilities(host: &str) -> BTreeMap<&'static str, Satisfaction> {
    // Quoted as one argument because sshd on Windows hands the command line to
    // cmd.exe, which would otherwise eat the pipes and the braces. Everything
    // inside is single-quoted for the same reason: a double quote here ends
    // the string cmd.exe is holding.
    let script = concat!(
        "\"$ErrorActionPreference='SilentlyContinue'; ",
        "$gpu = @(nvidia-smi --query-gpu=name --format=csv,noheader)[0]; ",
        "if ($gpu) { 'nvidia-nvenc yes ' + $gpu.Trim() } else { 'nvidia-nvenc no' }; ",
        "$idd = @(Get-CimInstance Win32_VideoController | Where-Object ",
        "{ $_.PNPDeviceID -like '*LANPLAYIDDLAB*' -and $_.CurrentRefreshRate })[0]; ",
        "if ($idd) { 'virtual-display yes ' + $idd.CurrentHorizontalResolution + 'x' + ",
        "$idd.CurrentVerticalResolution + '@' + $idd.CurrentRefreshRate } ",
        "else { 'virtual-display no' }; ",
        "$src = @(Get-Process present-source); ",
        "if ($src.Count -gt 0) { 'lab-source yes ' + $src.Count + ' present-source' } ",
        "else { 'lab-source no' }; ",
        "$out = @(Get-PnpDevice -Class AudioEndpoint -Status OK | Where-Object ",
        // A render endpoint's MMDevice id begins {0.0.0.…}; a capture
        // endpoint's begins {0.0.1.…}. The PnP class holds both, and a host
        // with only a microphone must not read as having somewhere to play.
        "{ $_.InstanceId -like '*{0.0.0.00000000}*' }); ",
        "if ($out.Count -gt 0) { 'audio-endpoint yes ' + $out[0].FriendlyName } ",
        "else { 'audio-endpoint no' }\"",
    );
    let command = format!("powershell -NoProfile -Command {script}");
    match ssh(host, &command, HOST_QUERY_DEADLINE) {
        Ok(output) if output.status.success() => {
            parse_host_report(&String::from_utf8_lossy(&output.stdout))
        }
        Ok(output) => unknown_host_answers(&format!(
            "the capability query failed: {}",
            crate::preflight::last_line(&output)
        )),
        Err(why) => unknown_host_answers(&format!("the capability query did not finish: {why}")),
    }
}

fn unknown_host_answers(why: &str) -> BTreeMap<&'static str, Satisfaction> {
    [
        "nvidia-nvenc",
        "virtual-display",
        "lab-source",
        "audio-endpoint",
    ]
    .into_iter()
    .map(|name| (name, Satisfaction::Unknown(why.to_string())))
    .collect()
}

/// `<requirement> yes <what was found>` or `<requirement> no`. A line in any
/// other shape is left out, and the caller reports the requirement as unknown:
/// a report this program cannot read is not evidence of absence.
fn parse_host_report(text: &str) -> BTreeMap<&'static str, Satisfaction> {
    let known = [
        "nvidia-nvenc",
        "virtual-display",
        "lab-source",
        "audio-endpoint",
    ];
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((requirement, rest)) = line.split_once(' ') else {
            continue;
        };
        let Some(name) = known.into_iter().find(|known| *known == requirement) else {
            continue;
        };
        let (verdict, detail) = match rest.split_once(' ') {
            Some((verdict, detail)) => (verdict, detail.trim()),
            None => (rest, ""),
        };
        let satisfaction = match verdict {
            "yes" if detail.is_empty() => Satisfaction::Present("the host has it".to_string()),
            "yes" => Satisfaction::Present(detail.to_string()),
            "no" => Satisfaction::Absent("the host does not have it".to_string()),
            _ => continue,
        };
        found.insert(name, satisfaction);
    }
    found
}

fn ssh(host: &str, command: &str, deadline: Duration) -> Result<Output, String> {
    run(
        "ssh",
        &[
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=3",
            host,
            command,
        ],
        deadline,
    )
}

/// Runs a command and kills it at the deadline. Polling rather than blocking
/// on `wait` is what makes the deadline real: a child that has filled its pipe
/// and stopped is killed at the same moment as one that is merely slow, and
/// either way this returns.
fn run(program: &str, args: &[&str], deadline: Duration) -> Result<Output, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run {program}: {err}"))?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(err) => return Err(format!("{program} could not be waited for: {err}")),
        }
        if started.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no answer within {} s", deadline.as_secs()));
        }
        sleep(Duration::from_millis(20));
    }
    child
        .wait_with_output()
        .map_err(|err| format!("{program} produced no output: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_report_states_presence_and_absence_apart() {
        let answered = "nvidia-nvenc yes NVIDIA GeForce RTX 4070\r\n\
             virtual-display yes 1920x1080@120\r\n\
             lab-source no\r\n\
             audio-endpoint yes Speakers\r\n";
        let found = parse_host_report(answered);
        assert_eq!(
            found["nvidia-nvenc"],
            Satisfaction::Present("NVIDIA GeForce RTX 4070".to_string())
        );
        assert!(matches!(found["lab-source"], Satisfaction::Absent(_)));
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn a_report_line_nobody_can_read_leaves_the_requirement_unsaid() {
        // Not `Absent`: the caller turns a missing entry into `Unknown`, and a
        // requirement this program failed to read must never look measured.
        let mangled = "nvidia-nvenc perhaps\nvirtual-display\nnoise\nlab-source no\n";
        let found = parse_host_report(mangled);
        assert_eq!(found.len(), 1);
        assert!(found.contains_key("lab-source"));
    }

    #[test]
    fn an_unreachable_host_makes_its_four_requirements_unknown() {
        let found = unknown_host_answers("the host did not answer");
        assert_eq!(found.len(), 4);
        assert!(
            found
                .values()
                .all(|satisfaction| satisfaction.state() == "unknown")
        );
    }

    #[test]
    fn the_refresh_rate_is_read_out_of_the_mode_string() {
        assert_eq!(refresh_hz("1512 x 982 @ 120.00Hz"), Some(120.0));
        assert_eq!(refresh_hz("3840 x 2160 @ 59.94Hz"), Some(59.94));
        assert_eq!(refresh_hz("1920 x 1080"), None);
    }

    #[test]
    fn a_sixty_hertz_panel_does_not_satisfy_a_display_gate() {
        let sixty = r#"{"SPDisplaysDataType":[{"spdisplays_ndrvs":[
            {"_name":"Office","_spdisplays_resolution":"2560 x 1440 @ 60.00Hz"}]}]}"#;
        assert_eq!(parse_displays(sixty).expect("readable"), None);

        let fast = r#"{"SPDisplaysDataType":[{"spdisplays_ndrvs":[
            {"_name":"Office","_spdisplays_resolution":"2560 x 1440 @ 60.00Hz"},
            {"_name":"Color LCD","_spdisplays_resolution":"1512 x 982 @ 119.88Hz"}]}]}"#;
        assert_eq!(
            parse_displays(fast).expect("readable").as_deref(),
            Some("Color LCD at 1512 x 982 @ 119.88Hz")
        );
    }

    #[test]
    fn a_machine_with_only_a_microphone_has_no_output() {
        let input_only = r#"{"SPAudioDataType":[{"_items":[
            {"_name":"Microphone","coreaudio_device_input":1}]}]}"#;
        assert_eq!(parse_audio_outputs(input_only).expect("readable"), None);

        let speakers = r#"{"SPAudioDataType":[{"_items":[
            {"_name":"Microphone","coreaudio_device_input":1},
            {"_name":"Speakers","coreaudio_device_output":2,"coreaudio_device_srate":48000,
             "coreaudio_default_audio_output_device":"spaudio_yes"}]}]}"#;
        assert_eq!(
            parse_audio_outputs(speakers).expect("readable").as_deref(),
            Some("1 output device(s), Speakers by default at 48000 Hz 2 ch")
        );
    }

    /// The listing this whole probe exists for. A6 spent thirty-seven seconds
    /// of a run discovering that the default mixed at 44100 Hz, having read a
    /// line that said only how many output devices there were.
    #[test]
    fn a_default_that_cannot_carry_the_stream_says_so_and_still_counts_as_present() {
        let headphones = r#"{"SPAudioDataType":[{"_items":[
            {"_name":"ULT WEAR","coreaudio_device_output":2,"coreaudio_device_srate":44100,
             "coreaudio_default_audio_output_device":"spaudio_yes"},
            {"_name":"Speakers","coreaudio_device_output":2,"coreaudio_device_srate":48000}]}]}"#;
        let found = parse_audio_outputs(headphones)
            .expect("readable")
            .expect("two devices that play");
        assert!(
            found.starts_with("2 output device(s), ULT WEAR by default"),
            "{found}"
        );
        assert!(found.contains("44100 Hz 2 ch"), "{found}");
        assert!(
            found.contains("cannot carry the 48000 Hz 2 ch stream"),
            "{found}"
        );
        // Present, because a gate naming its own device renders through the
        // other one and is not held up by what the system happens to point at.
        assert!(found.contains("names its own device"), "{found}");
    }

    #[test]
    fn a_default_that_did_not_state_its_rate_is_not_assumed_to_be_the_contract() {
        let quiet = r#"{"SPAudioDataType":[{"_items":[
            {"_name":"Odd Interface","coreaudio_device_output":2,
             "coreaudio_default_audio_output_device":"spaudio_yes"}]}]}"#;
        let found = parse_audio_outputs(quiet)
            .expect("readable")
            .expect("one device that plays");
        assert!(found.contains("did not state the rate"), "{found}");
    }

    #[test]
    fn a_decoder_that_leaves_out_h264_is_a_finding_and_no_decoder_is_not() {
        // The distinction the video gates rest on. A machine with nothing is a
        // machine nobody asked, and it reads as absent so that a gate is
        // excluded and says why; a machine that decodes HEVC in hardware and
        // not H.264 is absent too, but for a reason worth reading twice, and
        // the two must not print the same sentence.
        let nothing = judge_decoders(&[]);
        assert_eq!(nothing.state(), "absent");
        assert!(nothing.why().contains("no hardware decoder"), "{nothing:?}");

        let wrong_codec = judge_decoders(&[VideoCodec::Hevc, VideoCodec::Av1]);
        assert_eq!(wrong_codec.state(), "absent");
        assert!(
            wrong_codec.why().contains("but not H.264"),
            "an unexpected decoder set has to name itself: {wrong_codec:?}"
        );

        let usable = judge_decoders(&[VideoCodec::H264, VideoCodec::Hevc]);
        assert_eq!(usable.state(), "present");
        assert!(usable.why().contains("H.264"), "{usable:?}");
    }

    #[test]
    fn the_core_count_and_the_one_minute_load_are_read_out_of_what_sysctl_prints() {
        let (cores, load) = parse_headroom("10\n{ 3.80 4.11 3.90 }\n").expect("readable");
        assert_eq!(cores, 10);
        assert!((load - 3.80).abs() < f64::EPSILON, "read {load}");
        // The five and fifteen minute figures describe a machine that has
        // already stopped being the one a gate is about to run on, and reading
        // one of them by accident is a mistake nothing downstream could see.
        let (_, quiet) = parse_headroom("8\n{ 0.52 9.90 9.90 }").expect("readable");
        assert!((quiet - 0.52).abs() < f64::EPSILON, "read {quiet}");

        assert!(parse_headroom("").is_err());
        assert!(
            parse_headroom("10\n").is_err(),
            "a load nobody read is not a load of zero"
        );
    }

    #[test]
    fn a_machine_with_three_cores_free_is_quiet_and_a_hosted_runner_is_not() {
        let idle = judge_headroom(10, 0.4);
        assert_eq!(idle.state(), "present");
        assert!(idle.why().contains("9.6 are free"), "{idle:?}");

        // The runner this became a gate for: three cores, and the rest of a
        // test binary already running on them.
        let hosted = judge_headroom(3, 2.1);
        assert_eq!(hosted.state(), "absent");
        assert!(hosted.why().contains("needs 3"), "{hosted:?}");

        // Exactly enough is enough; a criterion that refused its own boundary
        // would be a different criterion from the one written down.
        assert_eq!(judge_headroom(4, 1.0).state(), "present");
    }

    #[test]
    fn a_deadline_is_enforced_rather_than_hoped_for() {
        let started = Instant::now();
        let outcome = run("sleep", &["30"], Duration::from_millis(200));
        assert!(outcome.is_err(), "a sleeping child must not be waited out");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_probe_that_cannot_be_started_is_an_error_rather_than_a_hang() {
        let outcome = run("a-program-this-machine-does-not-have", &[], LOCAL_DEADLINE);
        assert!(outcome.is_err());
    }
}
