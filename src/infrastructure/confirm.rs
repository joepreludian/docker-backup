//! Terminal confirmation prompts before restore and restore-volume.
//!
//! Holds `input`/`output` in `RefCell`s rather than locking `Stdin`/`Stderr`
//! for the run's duration: `StderrProgress`'s ticker thread writes to stderr
//! every 120ms, and holding a lock here would deadlock against it.

use std::cell::RefCell;
use std::io::{BufRead, Write};

use crate::application::ports::ConfirmPort;
use crate::domain::error::{AppError, AppResult};
use crate::domain::preview::{RestorePreview, VolumeOverwritePrompt};
use crate::presentation::{
    format_arch_warning, format_restore_preview, format_volume_overwrite_prompt,
};

pub struct TerminalConfirm<R: BufRead, W: Write> {
    input: RefCell<R>,
    output: RefCell<W>,
    assume_yes: bool,
    interactive: bool,
    color: bool,
}

impl<R: BufRead, W: Write> TerminalConfirm<R, W> {
    pub fn new(input: R, output: W, assume_yes: bool, interactive: bool, color: bool) -> Self {
        Self {
            input: RefCell::new(input),
            output: RefCell::new(output),
            assume_yes,
            interactive,
            color,
        }
    }

    /// Writes `question`, then either accepts on `--yes`, aborts when stdin
    /// isn't a terminal, or reads one line and checks it for `y`/`yes`.
    fn ask(&self, question: &str) -> AppResult<()> {
        if self.assume_yes {
            writeln!(self.output.borrow_mut(), "{question}yes (--yes)")?;
            return Ok(());
        }
        if !self.interactive {
            return Err(AppError::Aborted(
                "not running interactively (--json or stdin is not a terminal); pass --yes to restore without confirmation".into(),
            ));
        }

        {
            let mut output = self.output.borrow_mut();
            write!(output, "{question}")?;
            output.flush()?;
        }

        let mut line = String::new();
        let bytes_read = self.input.borrow_mut().read_line(&mut line)?;
        let answer = line.trim().to_ascii_lowercase();
        if bytes_read > 0 && (answer == "y" || answer == "yes") {
            Ok(())
        } else {
            Err(AppError::Aborted("cancelled by user".into()))
        }
    }
}

impl<R: BufRead, W: Write> ConfirmPort for TerminalConfirm<R, W> {
    fn confirm_restore(&self, preview: &RestorePreview) -> AppResult<()> {
        writeln!(
            self.output.borrow_mut(),
            "{}",
            format_restore_preview(preview, self.color)
        )?;
        self.ask("Proceed? [y/N] ")
    }

    fn confirm_arch_mismatch(&self, preview: &RestorePreview) -> AppResult<()> {
        writeln!(
            self.output.borrow_mut(),
            "{}",
            format_arch_warning(preview, self.color)
        )?;
        self.ask("Are you sure? [y/N] ")
    }

    fn confirm_volume_overwrite(&self, prompt: &VolumeOverwritePrompt) -> AppResult<()> {
        self.ask(&format_volume_overwrite_prompt(prompt))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use time::macros::datetime;

    use super::*;
    use crate::application::ports::ConfirmPort;
    use crate::domain::error::AppError;
    use crate::domain::platform::Platform;
    use crate::domain::preview::{MismatchedItem, RestorePreview, VolumeOverwritePrompt};
    use crate::domain::refs::ItemKind;

    fn preview() -> RestorePreview {
        RestorePreview {
            source: "/backups/out".into(),
            created_at: datetime!(2026-09-19 14:03:11 UTC),
            backup_platform: Platform::new("linux", "amd64"),
            target_platform: Platform::new("linux", "arm64"),
            overwrite: false,
            force_arch_mismatch: false,
            volumes_to_create: 1,
            volumes_to_overwrite: 0,
            volumes_skipped: 1,
            images_to_load: 1,
            containers_to_import: 0,
            mismatches: vec![MismatchedItem {
                kind: ItemKind::Image,
                name: "app:latest".into(),
                platform: Platform::new("linux", "arm64"),
            }],
        }
    }

    #[test]
    fn yes_answer_proceeds() {
        let confirm =
            TerminalConfirm::new(Cursor::new(b"y\n".to_vec()), Vec::new(), false, true, false);
        confirm.confirm_restore(&preview()).unwrap();
        let output = String::from_utf8(confirm.output.into_inner()).unwrap();
        assert!(output.contains("Restore preview"));
        assert!(output.contains("Proceed? [y/N]"));
    }

    #[test]
    fn empty_answer_aborts() {
        let confirm =
            TerminalConfirm::new(Cursor::new(b"\n".to_vec()), Vec::new(), false, true, false);
        let error = confirm.confirm_restore(&preview()).unwrap_err();
        assert!(matches!(error, AppError::Aborted(_)));
    }

    #[test]
    fn non_interactive_without_yes_aborts_with_hint() {
        let confirm =
            TerminalConfirm::new(Cursor::new(Vec::new()), Vec::new(), false, false, false);
        let error = confirm.confirm_restore(&preview()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("--yes"));
        // Must not blame "stdin" alone: `--json` in a real terminal is also non-interactive.
        assert!(message.contains("not running interactively (--json or stdin is not a terminal)"));
    }

    #[test]
    fn assume_yes_skips_reading_but_prints_preview() {
        let confirm = TerminalConfirm::new(Cursor::new(Vec::new()), Vec::new(), true, true, false);
        confirm.confirm_restore(&preview()).unwrap();
        let output = String::from_utf8(confirm.output.into_inner()).unwrap();
        assert!(output.contains("Restore preview"));
    }

    #[test]
    fn arch_prompt_prints_warning() {
        let confirm = TerminalConfirm::new(
            Cursor::new(b"yes\n".to_vec()),
            Vec::new(),
            false,
            true,
            false,
        );
        confirm.confirm_arch_mismatch(&preview()).unwrap();
        let output = String::from_utf8(confirm.output.into_inner()).unwrap();
        assert!(output.contains("Architecture mismatch"));
    }

    const VOLUME_QUESTION: &str = "Volume pgdata will be emptied and refilled from \
        /backups/pgdata-20260926T141500Z.tar.bz2 (backed up 2026-09-26 14:15 UTC). \
        Continue? [y/N] ";

    fn volume_prompt() -> VolumeOverwritePrompt {
        VolumeOverwritePrompt {
            target: "pgdata".into(),
            source: "/backups/pgdata-20260926T141500Z.tar.bz2".into(),
            created_at: datetime!(2026-09-26 14:15:00 UTC),
        }
    }

    #[test]
    fn volume_overwrite_asks_the_question_and_accepts_yes() {
        let confirm =
            TerminalConfirm::new(Cursor::new(b"y\n".to_vec()), Vec::new(), false, true, false);
        confirm.confirm_volume_overwrite(&volume_prompt()).unwrap();
        let output = String::from_utf8(confirm.output.into_inner()).unwrap();
        assert_eq!(output, VOLUME_QUESTION);
    }

    #[test]
    fn volume_overwrite_declined_aborts() {
        let confirm =
            TerminalConfirm::new(Cursor::new(b"n\n".to_vec()), Vec::new(), false, true, false);
        let error = confirm
            .confirm_volume_overwrite(&volume_prompt())
            .unwrap_err();
        assert!(matches!(error, AppError::Aborted(_)));
    }

    #[test]
    fn volume_overwrite_with_yes_answers_without_reading() {
        let confirm = TerminalConfirm::new(Cursor::new(Vec::new()), Vec::new(), true, false, false);
        confirm.confirm_volume_overwrite(&volume_prompt()).unwrap();
        let output = String::from_utf8(confirm.output.into_inner()).unwrap();
        assert_eq!(output, format!("{VOLUME_QUESTION}yes (--yes)\n"));
    }

    #[test]
    fn volume_overwrite_without_a_terminal_aborts_with_a_hint() {
        let confirm =
            TerminalConfirm::new(Cursor::new(Vec::new()), Vec::new(), false, false, false);
        let error = confirm
            .confirm_volume_overwrite(&volume_prompt())
            .unwrap_err();
        assert!(matches!(&error, AppError::Aborted(_)));
        assert!(error.to_string().contains("--yes"));
    }
}
