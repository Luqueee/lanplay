//! Driving one gate 1C run: preflight, the two processes, the report.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::preflight::{self, Preflight};
use crate::report::{self, Report};
use crate::{Abort, Gate1c};

/// How often the orchestrator looks at its children.
const POLL: Duration = Duration::from_millis(250);

/// A ten minute run that says nothing for ten minutes looks hung.
const PROGRESS_EVERY: Duration = Duration::from_secs(30);

/// Time the client gets to bind, open its window, start the display link and
/// finish its own preflight. Opening a Metal layer is fast; a machine that
/// needs more than this has something else wrong with it.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);

/// Slack over the requested run length before both children are killed. It
/// has to cover the client's own startup grace and its drain.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(120);

/// How long the sender may outlive the client. Once the client is gone the
/// datagrams land nowhere, so this only exists to let net-bench finish
/// printing before it is stopped.
const SENDER_GRACE: Duration = Duration::from_secs(20);

pub fn gate_1c(args: &Gate1c) -> Result<bool, Abort> {
    let root = repo_root();
    let target = root.join("target");
    let client_binary = target.join("release/lanplay-client");
    let report_path = target.join("gate-1c.json");
    let local_fixture = root.join("fixtures").join(preflight::FIXTURE_NAME);

    fs::create_dir_all(&target)
        .map_err(|err| Abort::new(format!("{}: {err}", target.display())))?;

    let mut checks = Preflight::new(args.keep_going);
    local_and_host_preflight(args, &mut checks, &root, &client_binary, &local_fixture)?;
    checks.finish()?;

    // An earlier run's report must never be mistaken for this one's.
    if report_path.exists() {
        fs::remove_file(&report_path)
            .map_err(|err| Abort::new(format!("{}: {err}", report_path.display())))?;
    }

    let mut client = spawn_client(
        args,
        &root,
        &client_binary,
        &target.join("gate-1c.client.log"),
    )?;
    if let Err(abort) = await_client_ready(&mut client, &mut checks).and_then(|()| checks.finish())
    {
        let _ = client.child.kill();
        let _ = client.reader.join();
        return Err(abort);
    }

    let sender_log = target.join("gate-1c.sender.log");
    let mut sender = spawn_sender(args, &sender_log)?;
    let outcome = supervise(args, &mut client, &mut sender);
    // Both logs are collected whatever happened, so a failed run is still
    // something you can read afterwards.
    let sender_output = sender.output.join().unwrap_or_default();
    let _ = client.reader.join();
    let (client_status, sender_status) = outcome?;

    if !sender_status.success() {
        return Err(Abort::new(format!(
            "the sender exited with {}; see {}",
            code(&sender_status),
            sender_log.display()
        )));
    }

    let report = load_report(&report_path, &client_status)?;
    let totals = report::sender_totals(&sender_output);
    report::print(&report, &totals);
    let reasons = report::evaluate(&report);
    report::print_verdict(&reasons);
    Ok(reasons.is_empty())
}

/// The workspace root, wherever the alias was invoked from.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the xtask manifest lives one level under the workspace root")
        .to_path_buf()
}

fn local_and_host_preflight(
    args: &Gate1c,
    checks: &mut Preflight,
    root: &Path,
    client_binary: &Path,
    local_fixture: &Path,
) -> Result<(), Abort> {
    let built = if client_binary.exists() {
        Ok(())
    } else {
        checks.note(
            "target/release/lanplay-client is missing; \
             running cargo build --release -p lanplay-client",
        );
        build_client(root)
    };
    let have_client = checks.check(
        "client binary",
        built.and_then(|()| {
            if client_binary.exists() {
                Ok(())
            } else {
                Err(format!("{} was not produced", client_binary.display()))
            }
        }),
    );

    // The remaining checks are seconds of work, and a run that is going to be
    // refused should say everything that is wrong with it in one pass rather
    // than one item per attempt.
    let have_flags = have_client
        && checks.check(
            "client gate flags",
            preflight::client_supports_gate(client_binary),
        );

    let reachable = checks.check("ssh host", preflight::host_reachable(&args.host));
    let local_hash = preflight::sha256_file(local_fixture);
    checks.check(
        "local fixture",
        local_hash.as_ref().map(|_| ()).map_err(String::clone),
    );

    if reachable {
        checks.check(
            "host net-bench",
            preflight::remote_file_exists(&args.host, &preflight::remote_net_bench()),
        );
        let remote_hash = preflight::remote_sha256(&args.host, &preflight::remote_fixture());
        checks.check(
            "host fixture",
            remote_hash.as_ref().map(|_| ()).map_err(String::clone),
        );
        if let (Ok(local), Ok(remote)) = (&local_hash, &remote_hash) {
            checks.check(
                "fixture identical",
                if local == remote {
                    Ok(())
                } else {
                    Err(format!(
                        "host has {remote}, this machine has {local}; \
                         a different fixture silently changes every number"
                    ))
                },
            );
        }
    }

    checks.check("udp port", preflight::udp_port_free(args.port));

    // --keep-going can tolerate a dirty environment, but it cannot conjure a
    // client, and a client that writes no report would measure nothing.
    if !have_client {
        return Err(Abort::new("there is no client binary to run"));
    }
    if !have_flags {
        return Err(Abort::new(
            "the client cannot write a report, so this run would measure nothing",
        ));
    }
    Ok(())
}

fn build_client(root: &Path) -> Result<(), String> {
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(root)
        .args(["build", "--release", "-p", "lanplay-client"])
        .status()
        .map_err(|err| format!("could not run cargo: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo build exited with {}", code(&status)))
    }
}

/// A log that starts empty and can be appended to from several handles at
/// once, so a child's stderr and our own tee never overwrite each other.
fn fresh_log(path: &Path) -> Result<File, Abort> {
    let open = |result: std::io::Result<File>| {
        result.map_err(|err| Abort::new(format!("{}: {err}", path.display())))
    };
    open(File::create(path))?;
    open(OpenOptions::new().append(true).open(path))
}

struct ClientProcess {
    child: Child,
    lines: Receiver<String>,
    reader: JoinHandle<()>,
}

fn spawn_client(
    args: &Gate1c,
    root: &Path,
    binary: &Path,
    log_path: &Path,
) -> Result<ClientProcess, Abort> {
    let log = fresh_log(log_path)?;
    let mut sink = log
        .try_clone()
        .map_err(|err| Abort::new(format!("{}: {err}", log_path.display())))?;
    let mut child = Command::new(binary)
        .current_dir(root)
        .args(["--transport", "lan"])
        .arg("--bind")
        .arg(format!("0.0.0.0:{}", args.port))
        .arg("--seconds")
        .arg(format!("{}", args.seconds))
        .arg("--fps")
        .arg(args.fps.to_string())
        .args(["--mode", "display-link", "--require-clean-display"])
        .arg("--report")
        .arg("target/gate-1c.json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|err| Abort::new(format!("could not start {}: {err}", binary.display())))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let (sender, lines) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = writeln!(sink, "{line}");
            // A closed channel only means nobody is watching any more; the
            // log still has to be complete.
            let _ = sender.send(line);
        }
    });

    Ok(ClientProcess {
        child,
        lines,
        reader,
    })
}

/// What one line of the client's startup output means to the orchestrator.
/// The order matters: `aborted` and `complete` are terminators, and anything
/// else the client prefixes with `preflight:` is one of its own items, even
/// when the detail after the dash happens to mention listening.
#[derive(Debug, PartialEq, Eq)]
enum Marker {
    Listening,
    Complete,
    Aborted,
    Item,
    Other,
}

fn classify(line: &str) -> Marker {
    if line.starts_with("preflight: aborted") {
        Marker::Aborted
    } else if line.starts_with("preflight: complete") {
        Marker::Complete
    } else if line.starts_with("preflight:") {
        Marker::Item
    } else if line.contains("listening on ") {
        Marker::Listening
    } else {
        Marker::Other
    }
}

/// Waits for the client to bind and to finish its own preflight, mirroring
/// every line it prints about itself.
fn await_client_ready(client: &mut ClientProcess, checks: &mut Preflight) -> Result<(), Abort> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut listening = false;
    loop {
        match client.lines.recv_timeout(POLL) {
            Ok(line) => match classify(&line) {
                Marker::Listening => {
                    let address = line.rsplit_once("listening on ").expect("just matched").1;
                    println!(
                        "preflight: ok client listening — bound to {}",
                        address.trim()
                    );
                    listening = true;
                }
                Marker::Complete => {
                    if !listening {
                        checks.check(
                            "client listening",
                            Err("the client never said what it bound to".into()),
                        );
                    }
                    println!("{line}");
                    return Ok(());
                }
                Marker::Aborted => {
                    println!("{line}");
                    return Err(Abort::new(
                        "the client refused to run: its own preflight failed",
                    ));
                }
                Marker::Item => checks.mirror(&line),
                Marker::Other => {}
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Abort::new(
                    "the client closed its output before it was ready",
                ));
            }
        }
        if let Some(status) = exited(&mut client.child)? {
            return Err(Abort::new(format!(
                "the client exited with {} before it was ready; see target/gate-1c.client.log",
                code(&status)
            )));
        }
        if Instant::now() > deadline {
            let _ = client.child.kill();
            return Err(Abort::new(format!(
                "the client was not ready within {} s",
                STARTUP_TIMEOUT.as_secs()
            )));
        }
    }
}

struct SenderProcess {
    child: Child,
    output: JoinHandle<String>,
}

fn spawn_sender(args: &Gate1c, log_path: &Path) -> Result<SenderProcess, Abort> {
    let log = fresh_log(log_path)?;
    let mut sink = log
        .try_clone()
        .map_err(|err| Abort::new(format!("{}: {err}", log_path.display())))?;
    let remote = [
        format!("\"{}\"", preflight::remote_net_bench()),
        "send".to_string(),
        "--to".to_string(),
        format!("{}:{}", args.client_addr, args.port),
        "--fixture".to_string(),
        format!("\"{}\"", preflight::remote_fixture()),
        "--fps".to_string(),
        args.fps.to_string(),
        "--seconds".to_string(),
        format!("{}", args.seconds),
        "--pacer".to_string(),
        "burst".to_string(),
    ];
    eprintln!("gate-1c: starting the sender on {}", args.host);
    let mut child = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .arg(&args.host)
        .args(&remote)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|err| Abort::new(format!("could not start ssh: {err}")))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let output = thread::spawn(move || {
        let mut captured = String::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = writeln!(sink, "{line}");
            captured.push_str(&line);
            captured.push('\n');
        }
        captured
    });

    Ok(SenderProcess { child, output })
}

/// Runs both children to completion, or kills them and says why.
fn supervise(
    args: &Gate1c,
    client: &mut ClientProcess,
    sender: &mut SenderProcess,
) -> Result<(ExitStatus, ExitStatus), Abort> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(args.seconds) + SHUTDOWN_GRACE;
    let mut next_progress = PROGRESS_EVERY;
    let mut client_done: Option<ExitStatus> = None;
    let mut sender_done: Option<ExitStatus> = None;
    let mut client_ended_at: Option<Instant> = None;

    loop {
        if client_done.is_none() {
            client_done = exited(&mut client.child)?;
            if client_done.is_some() {
                client_ended_at = Some(Instant::now());
            }
        }
        if sender_done.is_none() {
            sender_done = exited(&mut sender.child)?;
        }
        // Keep the channel drained; the reader thread is what writes the log.
        while client.lines.try_recv().is_ok() {}

        if let (Some(client_status), Some(sender_status)) = (client_done, sender_done) {
            return Ok((client_status, sender_status));
        }

        let elapsed = started.elapsed();
        if elapsed >= next_progress {
            eprintln!(
                "gate-1c: {:.0} s of {:.0} s — client {}, sender {}",
                elapsed.as_secs_f64(),
                args.seconds,
                state(client_done),
                state(sender_done),
            );
            next_progress += PROGRESS_EVERY;
        }

        if let Some(ended) = client_ended_at
            && sender_done.is_none()
            && ended.elapsed() > SENDER_GRACE
        {
            let _ = sender.child.kill();
            return Err(Abort::new(format!(
                "the client exited with {} while the sender was still running; \
                 the sender was stopped, and both logs are in target/",
                code(&client_done.expect("the client ended"))
            )));
        }

        if Instant::now() > deadline {
            let _ = client.child.kill();
            let _ = sender.child.kill();
            return Err(Abort::new(format!(
                "the run did not finish within {:.0} s; both children were killed, \
                 and both logs are in target/",
                deadline.duration_since(started).as_secs_f64()
            )));
        }
        thread::sleep(POLL);
    }
}

fn load_report(path: &Path, client_status: &ExitStatus) -> Result<Report, Abort> {
    let text = fs::read_to_string(path).map_err(|err| {
        Abort::new(format!(
            "the client exited with {} and wrote no {}: {err}",
            code(client_status),
            path.display()
        ))
    })?;
    serde_json::from_str(&text)
        .map_err(|err| Abort::new(format!("{} is not a gate 1C report: {err}", path.display())))
}

fn exited(child: &mut Child) -> Result<Option<ExitStatus>, Abort> {
    child
        .try_wait()
        .map_err(|err| Abort::new(format!("could not wait for a child process: {err}")))
}

fn state(status: Option<ExitStatus>) -> String {
    match status {
        Some(status) => format!("exited with {}", code(&status)),
        None => "running".to_string(),
    }
}

fn code(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => code.to_string(),
        None => "a signal".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Marker, classify};

    #[test]
    fn the_transport_line_is_what_says_the_client_is_bound() {
        assert_eq!(
            classify("transport: RTP over UDP, listening on 0.0.0.0:5004"),
            Marker::Listening
        );
    }

    #[test]
    fn terminators_win_over_the_generic_preflight_prefix() {
        assert_eq!(classify("preflight: complete"), Marker::Complete);
        assert_eq!(classify("preflight: aborted (2 failed)"), Marker::Aborted);
    }

    #[test]
    fn the_clients_own_items_are_mirrored_not_mistaken_for_markers() {
        assert_eq!(
            classify("preflight: ok display — window is on \"Built-in Retina Display\""),
            Marker::Item
        );
        assert_eq!(
            classify("preflight: FAIL occlusion — the window is covered"),
            Marker::Item
        );
        // A detail that mentions listening is still an item, not the bind
        // line: acting on it would start the sender a step early.
        assert_eq!(
            classify("preflight: ok socket — listening on 0.0.0.0:5004"),
            Marker::Item
        );
    }

    #[test]
    fn ordinary_output_is_ignored() {
        assert_eq!(classify("decoded 2400 access units"), Marker::Other);
    }
}
