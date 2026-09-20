//! Finds external binaries on PATH and asks them for a version string.

use std::process::{Command, Stdio};

use crate::application::ports::ToolLocator;
use crate::domain::manifest::ToolInfo;

pub struct WhichToolLocator;

pub fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
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
                let stdout = first_line(&String::from_utf8_lossy(&out.stdout));
                if stdout.is_empty() {
                    first_line(&String::from_utf8_lossy(&out.stderr))
                } else {
                    stdout
                }
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
}
