//! One rendering of the human block, sent to two places.
//!
//! Capture has to run in the interactive session, which on this host means
//! being launched through a scheduled task. A process started that way has no
//! stdout anyone can read, so a benchmark that only printed would produce
//! results nobody could see. The block is therefore built once into a buffer
//! and then written to both stdout and a file, which also makes the two
//! byte-identical by construction rather than by discipline.
//!
//! The file is written on every exit path. The run that fails is exactly the
//! run whose block is worth reading.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct Output {
    buffer: String,
    path: Option<PathBuf>,
}

impl Output {
    pub fn new(path: Option<PathBuf>) -> Self {
        Output {
            buffer: String::new(),
            path,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Prints the block and writes the log. Consuming, so it cannot be called
    /// twice and cannot be left half-done while the value is still usable.
    pub fn finish(self) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(self.buffer.as_bytes())?;
        stdout.flush()?;

        if let Some(path) = &self.path {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::File::create(path)?;
            file.write_all(self.buffer.as_bytes())?;
            file.flush()?;
            // The reader of this file is on the other side of a session
            // boundary; a buffered write that never reached the disk would be
            // indistinguishable from a run that produced no block at all.
            file.sync_all()?;
        }
        Ok(())
    }
}

impl fmt::Write for Output {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.buffer.push_str(text);
        Ok(())
    }
}

/// Where the human block goes when `--log` was not given.
///
/// A run that emits a JSON the operator can read and a block they cannot is
/// the failure mode this exists to prevent, so `--report` implies a log beside
/// it.
pub fn resolve_log_path(log: Option<PathBuf>, report: Option<&Path>) -> Option<PathBuf> {
    match log {
        Some(path) => Some(path),
        None => report.map(|path| path.with_extension("txt")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_log_path_wins() {
        let resolved = resolve_log_path(
            Some(PathBuf::from("a/explicit.log")),
            Some(Path::new("b/report.json")),
        );
        assert_eq!(resolved, Some(PathBuf::from("a/explicit.log")));
    }

    #[test]
    fn a_report_implies_a_log_beside_it() {
        let resolved = resolve_log_path(None, Some(Path::new("out/wgc-native.json")));
        assert_eq!(resolved, Some(PathBuf::from("out/wgc-native.txt")));
    }

    #[test]
    fn a_report_without_an_extension_still_gets_one() {
        assert_eq!(
            resolve_log_path(None, Some(Path::new("out/run"))),
            Some(PathBuf::from("out/run.txt"))
        );
    }

    #[test]
    fn no_report_and_no_log_means_stdout_only() {
        assert_eq!(resolve_log_path(None, None), None);
    }

    #[test]
    fn stdout_and_the_log_receive_the_same_bytes() {
        use std::fmt::Write;

        let directory =
            std::env::temp_dir().join(format!("capture-bench-output-{}", std::process::id()));
        let path = directory.join("nested/block.txt");

        let mut output = Output::new(Some(path.clone()));
        write!(output, "CAPTURE\n  frames  {}\n", 6_000).unwrap();
        let expected = "CAPTURE\n  frames  6000\n";
        output.finish().expect("log written");

        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        fs::remove_dir_all(&directory).ok();
    }
}
