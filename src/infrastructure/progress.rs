//! Progress on stderr: a spinner while an item is in flight, then one plain line.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::application::ports::{Operation, ProgressSink};
use crate::domain::refs::ItemKind;
use crate::domain::report::ItemOutcome;

pub fn format_progress_line(
    operation: Operation,
    index: usize,
    total: usize,
    kind: ItemKind,
    name: &str,
) -> String {
    format!("[ {operation} {index}/{total} ] {kind:<9} {name} ...")
}

pub struct StderrProgress {
    operation: Cell<Operation>,
    total: Cell<usize>,
    spinner: RefCell<Option<ProgressBar>>,
    current: RefCell<String>,
}

impl Default for StderrProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl StderrProgress {
    pub fn new() -> Self {
        Self {
            operation: Cell::new(Operation::Backup),
            total: Cell::new(0),
            spinner: RefCell::new(None),
            current: RefCell::new(String::new()),
        }
    }
}

impl ProgressSink for StderrProgress {
    fn start(&self, operation: Operation, total: usize) {
        self.operation.set(operation);
        self.total.set(total);
    }

    fn item_started(&self, kind: ItemKind, name: &str, index: usize) {
        let line = format_progress_line(self.operation.get(), index, self.total.get(), kind, name);
        let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
        bar.set_style(ProgressStyle::with_template("{msg} {spinner}").expect("static template"));
        bar.set_message(line.clone());
        bar.enable_steady_tick(Duration::from_millis(120));
        *self.spinner.borrow_mut() = Some(bar);
        *self.current.borrow_mut() = line;
    }

    fn item_finished(&self, _kind: ItemKind, _name: &str, outcome: &ItemOutcome) {
        if let Some(bar) = self.spinner.borrow_mut().take() {
            bar.finish_and_clear();
        }
        eprintln!("{} {}", self.current.borrow(), outcome.label());
    }

    fn finish(&self) {}
}

pub struct NullProgress;

impl ProgressSink for NullProgress {
    fn start(&self, _operation: Operation, _total: usize) {}
    fn item_started(&self, _kind: ItemKind, _name: &str, _index: usize) {}
    fn item_finished(&self, _kind: ItemKind, _name: &str, _outcome: &ItemOutcome) {}
    fn finish(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_matches_spec_format() {
        assert_eq!(
            format_progress_line(Operation::Backup, 1, 4, ItemKind::Volume, "pgdata"),
            "[ BACKUP 1/4 ] VOLUME    pgdata ..."
        );
        assert_eq!(
            format_progress_line(Operation::Restore, 12, 12, ItemKind::Container, "web"),
            "[ RESTORE 12/12 ] CONTAINER web ..."
        );
    }

    #[test]
    fn null_progress_is_silent() {
        let progress = NullProgress;
        progress.start(Operation::Backup, 1);
        progress.item_started(ItemKind::Image, "x", 1);
        progress.item_finished(ItemKind::Image, "x", &ItemOutcome::Restored);
        progress.finish();
    }
}
