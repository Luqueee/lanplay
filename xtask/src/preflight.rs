//! Everything that is cheap to check and expensive to get wrong.
//!
//! A ten minute run that dies on minute nine because the host had no fixture
//! is ten minutes gone, so every precondition is settled first, and each one
//! prints its own line rather than hiding inside a single "ready".

use std::fs::File;
use std::io::Read;
use std::net::UdpSocket;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};

use crate::Abort;

/// Where the sending machine keeps its checkout. Fixed by the rig, not by us.
pub const REMOTE_ROOT: &str = r"C:\Users\luque\lanplay-rs";

/// The one fixture the baseline is defined against. Every number in the report
/// is a number about this bitstream.
pub const FIXTURE_NAME: &str = "motion-1920x1080@120-10s-50M.h264";

pub fn remote_net_bench() -> String {
    format!(r"{REMOTE_ROOT}\target\release\net-bench.exe")
}

pub fn remote_fixture() -> String {
    format!(r"{REMOTE_ROOT}\fixtures\{FIXTURE_NAME}")
}

/// Records preflight outcomes and decides whether the run may start.
pub struct Preflight {
    failures: usize,
    /// Failures already announced, so a second `finish` after the client's
    /// own items stays quiet about the ones it already reported.
    announced: usize,
    keep_going: bool,
}

impl Preflight {
    pub fn new(keep_going: bool) -> Self {
        Self {
            failures: 0,
            announced: 0,
            keep_going,
        }
    }

    /// A line that is neither pass nor fail, such as "building the client".
    pub fn note(&self, message: &str) {
        println!("preflight: {message}");
    }

    /// Records one item. The bool lets a caller stop early when a later check
    /// would be meaningless without this one.
    pub fn check(&mut self, item: &str, outcome: Result<(), String>) -> bool {
        match outcome {
            Ok(()) => {
                println!("preflight: ok {item}");
                true
            }
            Err(why) => {
                println!("preflight: FAIL {item} — {why}");
                self.failures += 1;
                false
            }
        }
    }

    /// Mirrors a preflight line the client printed about itself, and counts it
    /// if the client called it a failure.
    pub fn mirror(&mut self, line: &str) {
        println!("{line}");
        if line.starts_with("preflight: FAIL") {
            self.failures += 1;
        }
    }

    pub fn finish(&mut self) -> Result<(), Abort> {
        if self.failures == 0 {
            return Ok(());
        }
        if self.keep_going {
            if self.failures > self.announced {
                eprintln!(
                    "gate-1c: continuing past {} failed preflight item(s); \
                     this run cannot be a baseline",
                    self.failures
                );
                self.announced = self.failures;
            }
            return Ok(());
        }
        Err(Abort::new(format!(
            "{} preflight item(s) failed",
            self.failures
        )))
    }
}

/// Runs a command on the sending machine. `BatchMode` means a host that wants
/// a password fails in a second instead of blocking the run forever.
pub fn ssh(host: &str, command: &[&str]) -> Result<Output, String> {
    Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .arg(host)
        .args(command)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("could not run ssh: {err}"))
}

/// The last thing a failing remote command said. Windows writes its console
/// output in the OEM codepage, so this is deliberately lossy.
pub fn last_line(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output")
        .to_string()
}

pub fn host_reachable(host: &str) -> Result<(), String> {
    let output = ssh(host, &["ver"])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("ssh {host} ver failed: {}", last_line(&output)))
    }
}

pub fn remote_file_exists(host: &str, path: &str) -> Result<(), String> {
    let output = ssh(host, &["dir", "/b", &format!("\"{path}\"")])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{path} is not on {host}: {}", last_line(&output)))
    }
}

/// SHA-256 of a file on the sending machine, via the only hasher Windows
/// ships by default.
pub fn remote_sha256(host: &str, path: &str) -> Result<String, String> {
    let output = ssh(
        host,
        &["certutil", "-hashfile", &format!("\"{path}\""), "SHA256"],
    )?;
    if !output.status.success() {
        return Err(format!(
            "certutil could not hash {path}: {}",
            last_line(&output)
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_certutil_hash(&text).ok_or_else(|| {
        format!(
            "certutil printed no hash for {path}: {}",
            last_line(&output)
        )
    })
}

/// certutil's wrapper text is localised and its hash is sometimes printed
/// byte-spaced, so the hash is found by shape rather than by position.
pub fn parse_certutil_hash(text: &str) -> Option<String> {
    text.lines()
        .map(|line| {
            line.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        })
        .find(|line| line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|line| line.to_ascii_lowercase())
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut hasher = Sha256::new();
    // The fixture is 59 MB; hashing it through a fixed buffer keeps the check
    // off the heap-allocation path entirely.
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Nothing may hold the port: a second listener would take some of the
/// datagrams and the run would report a loss the link never caused.
pub fn udp_port_free(port: u16) -> Result<(), String> {
    match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(socket) => {
            drop(socket);
            Ok(())
        }
        Err(err) => Err(format!("udp/{port} is not bindable: {err}")),
    }
}

/// The client has to be the version that writes the report and refuses a
/// dirty display; an older binary would run happily and produce nothing.
pub fn client_supports_gate(binary: &Path) -> Result<(), String> {
    let output = Command::new(binary)
        .arg("--help")
        .output()
        .map_err(|err| format!("could not run {}: {err}", binary.display()))?;
    let help = String::from_utf8_lossy(&output.stdout);
    let missing: Vec<&str> = ["--report", "--require-clean-display"]
        .into_iter()
        .filter(|flag| !help.contains(flag))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} does not accept {}; the client slice has not landed, \
         so nothing would write target/gate-1c.json",
        binary.display(),
        missing.join(" or ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certutil_hash_is_found_under_a_localised_wrapper() {
        let spanish = "SHA256 hash de C:\\fixtures\\motion.h264:\n\
             98bf078e1aac26829fd879331636ab4c359b6800602eb085d5ccf3ea21e1baa7\n\
             CertUtil: -hashfile comando completado correctamente.\n";
        assert_eq!(
            parse_certutil_hash(spanish).as_deref(),
            Some("98bf078e1aac26829fd879331636ab4c359b6800602eb085d5ccf3ea21e1baa7")
        );
    }

    #[test]
    fn byte_spaced_and_uppercase_hashes_normalise() {
        let spaced = "SHA256 hash of file:\n\
             98 bf 07 8e 1a ac 26 82 9f d8 79 33 16 36 ab 4c \
             35 9b 68 00 60 2e b0 85 d5 cc f3 ea 21 e1 ba A7\n\
             CertUtil: -hashfile command completed successfully.\n";
        assert_eq!(
            parse_certutil_hash(spaced).as_deref(),
            Some("98bf078e1aac26829fd879331636ab4c359b6800602eb085d5ccf3ea21e1baa7")
        );
    }

    #[test]
    fn a_missing_hash_is_not_invented() {
        let failed = "CertUtil: -hashfile command FAILED: 0x80070002\n\
             CertUtil: The system cannot find the file specified.\n";
        assert!(parse_certutil_hash(failed).is_none());
    }

    #[test]
    fn a_64_character_word_that_is_not_hex_is_rejected() {
        let noise = format!("{}\n", "z".repeat(64));
        assert!(parse_certutil_hash(&noise).is_none());
    }

    #[test]
    fn keep_going_downgrades_failures_but_plain_mode_aborts() {
        let mut strict = Preflight::new(false);
        strict.check("host", Err("unreachable".into()));
        assert!(strict.finish().is_err());

        let mut lenient = Preflight::new(true);
        lenient.check("host", Err("unreachable".into()));
        assert!(lenient.finish().is_ok());
    }

    #[test]
    fn a_mirrored_client_failure_counts_against_the_run() {
        let mut preflight = Preflight::new(false);
        preflight.mirror("preflight: ok display");
        assert!(preflight.finish().is_ok());
        preflight.mirror("preflight: FAIL occlusion — the window is covered");
        assert!(preflight.finish().is_err());
    }

    #[test]
    fn a_bound_port_is_reported_as_taken() {
        let held = UdpSocket::bind(("0.0.0.0", 0)).expect("bind an ephemeral port");
        let port = held.local_addr().expect("local addr").port();
        assert!(udp_port_free(port).is_err());
        drop(held);
        assert!(udp_port_free(port).is_ok());
    }
}
