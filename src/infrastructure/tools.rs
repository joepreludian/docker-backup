//! Finds external binaries on PATH and asks them for a version string.

use std::process::{Command, Stdio};

use crate::application::ports::ToolLocator;
use crate::domain::manifest::ToolInfo;

pub struct WhichToolLocator;

pub fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

/// A line is fit to show as a version string when none of its characters are
/// control characters other than whitespace (rules out binary noise like a
/// compressed stream written to the wrong file descriptor).
fn is_printable(line: &str) -> bool {
    line.chars().all(|c| !c.is_control() || c.is_whitespace())
}

/// Picks the version banner out of a tool's `--version` output: the first
/// non-empty, printable line from stdout, falling back to stderr when stdout
/// is empty or unprintable (e.g. `bzip2 --version` prints its banner to
/// stderr and then writes compressed bytes for the empty stdin to stdout).
pub fn pick_version_line(stdout: &str, stderr: &str) -> String {
    let candidate = first_line(stdout);
    if !candidate.is_empty() && is_printable(&candidate) {
        return candidate;
    }
    let candidate = first_line(stderr);
    if is_printable(&candidate) {
        return candidate;
    }
    String::new()
}

impl ToolLocator for WhichToolLocator {
    fn locate(&self, name: &str) -> Option<ToolInfo> {
        let path = which::which(name).ok()?;
        // stdin must be null: `bzip2 --version` waits for input on a TTY.
        let version = Command::new(&path)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .ok()
            .map(|out| {
                pick_version_line(
                    &String::from_utf8_lossy(&out.stdout),
                    &String::from_utf8_lossy(&out.stderr),
                )
            })
            .unwrap_or_default();
        Some(ToolInfo {
            path: path.display().to_string(),
            version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_trims() {
        assert_eq!(
            first_line("bzip2, a block-sorting file compressor.  Version 1.0.8\nmore\n"),
            "bzip2, a block-sorting file compressor.  Version 1.0.8"
        );
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn locates_an_existing_tool_and_misses_a_fake_one() {
        let locator = WhichToolLocator;
        let tar = locator.locate("tar").expect("tar exists on dev machines");
        assert!(tar.path.ends_with("tar"));
        assert!(locator.locate("definitely-not-a-tool-xyz").is_none());
    }

    #[test]
    fn pick_version_line_prefers_printable_stdout() {
        assert_eq!(
            pick_version_line("Docker version 29\n", ""),
            "Docker version 29"
        );
    }

    #[test]
    fn pick_version_line_falls_back_to_stderr_when_stdout_is_binary_noise() {
        assert_eq!(
            pick_version_line(
                "BZh9\u{17}rE8P\u{0}\u{0}",
                "bzip2, a block-sorting file compressor.  Version 1.0.8\n"
            ),
            "bzip2, a block-sorting file compressor.  Version 1.0.8"
        );
    }

    #[test]
    fn pick_version_line_of_empty_output_is_empty() {
        assert_eq!(pick_version_line("", ""), "");
    }
}
