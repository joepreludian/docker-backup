//! Tables for terminals. Colour is opt-in so piped output stays clean.

use std::io::{self, Write};

use comfy_table::{Cell, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use console::style;

use crate::domain::error::AppError;
use crate::domain::refs::ItemKind;
use crate::domain::report::{
    BackupReport, DoctorReport, InfoReport, ItemOutcome, ItemResult, RestoreReport,
    VolumeBackupReport, VolumeInfoReport, VolumeRestoreAction, VolumeRestoreReport, human_size,
};
use crate::domain::verification::{FileStatus, VerificationReport};
use crate::presentation::Renderer;

pub struct HumanRenderer {
    pub color: bool,
}

impl HumanRenderer {
    fn table(&self, header: &[&str]) -> Table {
        let mut table = Table::new();
        table
            .load_style(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(header.to_vec());
        if self.color {
            // Tests (and piped output) aren't a tty; without this, comfy-table
            // silently drops the `.fg(..)` styling applied to cells below.
            table.enforce_styling();
        }
        table
    }

    /// Colours non-table text. The `color` flag alone decides — never `console`'s
    /// own tty autodetection, which would ignore `color: true` on non-tty output.
    fn paint(&self, text: &str, tone: Tone) -> String {
        if !self.color {
            return text.to_string();
        }
        match tone {
            Tone::Good => style(text).force_styling(true).green().to_string(),
            Tone::Warn => style(text).force_styling(true).yellow().to_string(),
            Tone::Bad => style(text).force_styling(true).red().to_string(),
        }
    }

    /// Colours a table cell via comfy-table's own styling instead of embedding
    /// raw ANSI in the cell text, which would corrupt comfy-table's column-width
    /// measurement.
    fn cell(&self, text: &str, tone: Tone) -> Cell {
        let cell = Cell::new(text);
        if !self.color {
            return cell;
        }
        match tone {
            Tone::Good => cell.fg(comfy_table::Color::Green),
            Tone::Warn => cell.fg(comfy_table::Color::Yellow),
            Tone::Bad => cell.fg(comfy_table::Color::Red),
        }
    }

    fn outcome_cell(&self, outcome: &ItemOutcome) -> Cell {
        self.cell(&outcome.label(), outcome_tone(outcome))
    }

    fn items_table(&self, items: &[ItemResult]) -> Table {
        let mut table = self.table(&["Kind", "Name", "File", "Status"]);
        for item in items {
            let name = match &item.target {
                Some(target) => format!("{} → {target}", item.name),
                None => item.name.clone(),
            };
            table.add_row(vec![
                Cell::new(item.kind.to_string()),
                Cell::new(name),
                Cell::new(item.file.as_deref().unwrap_or("-")),
                self.outcome_cell(&item.outcome),
            ]);
        }
        table
    }

    /// One row per checked file; both kinds of `info` draw their files with this.
    /// Volumes named in `external` are marked as such.
    fn verification_table(&self, verification: &VerificationReport, external: &[&str]) -> Table {
        let mut files = self.table(&["Kind", "Name", "File", "Size", "Status"]);
        for check in &verification.files {
            let name = if check.kind == ItemKind::Volume && external.contains(&check.name.as_str())
            {
                format!("{} (external)", check.name)
            } else {
                check.name.clone()
            };
            let (label, tone) = match &check.status {
                FileStatus::Ok => ("ok", Tone::Good),
                FileStatus::Missing => ("missing", Tone::Bad),
                FileStatus::Corrupt { .. } => ("corrupt", Tone::Bad),
            };
            files.add_row(vec![
                Cell::new(check.kind.to_string()),
                Cell::new(name),
                Cell::new(check.file.clone()),
                Cell::new(human_size(check.size_bytes)),
                self.cell(label, tone),
            ]);
        }
        files
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Good,
    Warn,
    Bad,
}

fn outcome_tone(outcome: &ItemOutcome) -> Tone {
    match outcome {
        ItemOutcome::Done { .. } | ItemOutcome::Restored => Tone::Good,
        ItemOutcome::SkippedExisting
        | ItemOutcome::SkippedVolatile
        | ItemOutcome::SkippedNotFound => Tone::Warn,
        ItemOutcome::SkippedArchMismatch { .. } | ItemOutcome::Failed { .. } => Tone::Bad,
    }
}

impl Renderer for HumanRenderer {
    fn render_backup(&self, report: &BackupReport, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "Backup written to {}", report.output.display())?;
        if let Some(compose) = &report.compose {
            writeln!(
                out,
                "Compose project {} ({})",
                compose.project,
                compose.files.join(", ")
            )?;
        }
        writeln!(out, "{}", self.items_table(&report.items))?;
        writeln!(
            out,
            "{} items, {} failed, {} stored (docker {})",
            report.items.len(),
            report.failed_count(),
            human_size(report.total_bytes()),
            report.docker.server_version
        )
    }

    fn render_restore(&self, report: &RestoreReport, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "Restored from {}", report.source.display())?;
        writeln!(out, "{}", self.items_table(&report.items))?;
        writeln!(
            out,
            "{} items, {} failed",
            report.items.len(),
            report.failed_count()
        )
    }

    fn render_volume_backup(
        &self,
        report: &VolumeBackupReport,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        writeln!(out, "Archives written to {}", report.output_dir.display())?;
        let mut table = self.table(&["Status", "Volume", "Archive", "Size"]);
        for item in &report.items {
            let (status, size) = match &item.outcome {
                ItemOutcome::Done { size_bytes } => ("done".to_string(), human_size(*size_bytes)),
                other => (other.label(), "-".to_string()),
            };
            table.add_row(vec![
                self.cell(&status, outcome_tone(&item.outcome)),
                Cell::new(&item.name),
                Cell::new(item.file.as_deref().unwrap_or("-")),
                Cell::new(size),
            ]);
        }
        writeln!(out, "{table}")?;
        writeln!(
            out,
            "{} volumes, {} failed, {} stored",
            report.items.len(),
            report.failed_count(),
            human_size(report.total_bytes())
        )
    }

    fn render_volume_restore(
        &self,
        report: &VolumeRestoreReport,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        let route = format!("{} → {}", report.volume, report.target);
        match &report.outcome {
            ItemOutcome::Failed { error } => {
                writeln!(out, "{} {route}: {error}", self.paint("✗", Tone::Bad))?;
            }
            _ => {
                let done = match report.action {
                    VolumeRestoreAction::Create => "created",
                    VolumeRestoreAction::Overwrite => "overwritten",
                };
                writeln!(out, "{} {route} ({done})", self.paint("✓", Tone::Good))?;
            }
        }
        if let Some(hint) = &report.hint {
            writeln!(out, "hint: {hint}")?;
        }
        Ok(())
    }

    fn render_info(&self, report: &InfoReport, out: &mut dyn Write) -> io::Result<()> {
        let m = &report.manifest;
        let mut meta = self.table(&["Field", "Value"]);
        let created = m
            .created_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        meta.add_row(vec![
            Cell::new("Source"),
            Cell::new(report.source.display().to_string()),
        ]);
        meta.add_row(vec![Cell::new("Created at"), Cell::new(&created)]);
        meta.add_row(vec![
            Cell::new("Tool"),
            Cell::new(format!("{} {}", m.tool.name, m.tool.version)),
        ]);
        if let Some(compose) = &m.compose {
            meta.add_row(vec![
                Cell::new("Compose project"),
                Cell::new(format!(
                    "{} ({})",
                    compose.project,
                    compose.files.join(", ")
                )),
            ]);
        }
        meta.add_row(vec![
            Cell::new("Docker server"),
            Cell::new(&m.docker.server_version),
        ]);
        meta.add_row(vec![
            Cell::new("Docker client"),
            Cell::new(&m.docker.client_version),
        ]);
        meta.add_row(vec![Cell::new("Docker host"), Cell::new(&m.docker.host)]);
        meta.add_row(vec![
            Cell::new("Docker context"),
            Cell::new(&m.docker.context),
        ]);
        meta.add_row(vec![
            Cell::new("Platform"),
            Cell::new(format!("{}/{}", m.docker.os, m.docker.arch)),
        ]);
        meta.add_row(vec![
            Cell::new("Compression"),
            Cell::new(m.compression.label()),
        ]);
        meta.add_row(vec![
            Cell::new("Items"),
            Cell::new(format!(
                "{} volumes, {} images, {} containers",
                m.volumes.len(),
                m.images.len(),
                m.containers.len()
            )),
        ]);
        meta.add_row(vec![
            Cell::new("Total size"),
            Cell::new(human_size(m.total_bytes())),
        ]);
        writeln!(out, "{meta}")?;
        let external: Vec<&str> = m
            .volumes
            .iter()
            .filter(|v| v.compose.as_ref().is_some_and(|c| c.external))
            .map(|v| v.name.as_str())
            .collect();
        writeln!(
            out,
            "{}",
            self.verification_table(&report.verification, &external)
        )?;
        writeln!(out, "{}", report.verification.summary())
    }

    fn render_volume_info(&self, report: &VolumeInfoReport, out: &mut dyn Write) -> io::Result<()> {
        let m = &report.manifest;
        let created = m
            .created_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let mut meta = self.table(&["Field", "Value"]);
        meta.add_row(vec![
            Cell::new("Source"),
            Cell::new(report.source.display().to_string()),
        ]);
        meta.add_row(vec![Cell::new("Type"), Cell::new("single volume")]);
        meta.add_row(vec![Cell::new("Volume"), Cell::new(&m.volume)]);
        meta.add_row(vec![Cell::new("Created at"), Cell::new(&created)]);
        meta.add_row(vec![
            Cell::new("Tool"),
            Cell::new(format!("{} {}", m.tool.name, m.tool.version)),
        ]);
        meta.add_row(vec![Cell::new("Size"), Cell::new(human_size(m.size_bytes))]);
        meta.add_row(vec![Cell::new("SHA-256"), Cell::new(m.sha256.to_string())]);
        writeln!(out, "{meta}")?;
        writeln!(
            out,
            "{}",
            self.verification_table(&report.verification, &[])
        )
    }

    fn render_doctor(&self, report: &DoctorReport, out: &mut dyn Write) -> io::Result<()> {
        let mut docker = self.table(&["Docker", "Value"]);
        match (&report.docker, &report.docker_error) {
            (Some(info), _) => {
                docker.add_row(vec![
                    Cell::new("Status"),
                    self.cell("available", Tone::Good),
                ]);
                docker.add_row(vec![Cell::new("Context"), Cell::new(&info.context)]);
                docker.add_row(vec![Cell::new("Host"), Cell::new(&info.host)]);
                docker.add_row(vec![
                    Cell::new("Server version"),
                    Cell::new(&info.server_version),
                ]);
                docker.add_row(vec![
                    Cell::new("Client version"),
                    Cell::new(&info.client_version),
                ]);
                docker.add_row(vec![
                    Cell::new("Platform"),
                    Cell::new(format!("{}/{}", info.os, info.arch)),
                ]);
            }
            (None, error) => {
                docker.add_row(vec![
                    Cell::new("Status"),
                    self.cell("unavailable", Tone::Bad),
                ]);
                docker.add_row(vec![
                    Cell::new("Error"),
                    Cell::new(error.as_deref().unwrap_or("unknown")),
                ]);
            }
        }
        docker.add_row(vec![
            Cell::new("Images"),
            Cell::new(format!(
                "{} created, {} pulled, {} total",
                report.images.created, report.images.pulled, report.images.total
            )),
        ]);
        docker.add_row(vec![
            Cell::new("Volumes"),
            Cell::new(format!(
                "{} named, {} volatile, {} total",
                report.volumes.named, report.volumes.volatile, report.volumes.total
            )),
        ]);
        docker.add_row(vec![
            Cell::new("Containers"),
            Cell::new(format!(
                "{} running, {} total",
                report.containers.running, report.containers.total
            )),
        ]);
        writeln!(out, "{docker}")?;

        let mut tools = self.table(&["Tool", "Required", "Status", "Path", "Version"]);
        for tool in &report.tools {
            let (status_label, tone, path, version) = match &tool.info {
                Some(info) => ("found", Tone::Good, info.path.clone(), info.version.clone()),
                None if tool.required => ("missing", Tone::Bad, "-".to_string(), "-".to_string()),
                None => ("missing", Tone::Warn, "-".to_string(), "-".to_string()),
            };
            tools.add_row(vec![
                Cell::new(tool.name.clone()),
                Cell::new(if tool.required { "yes" } else { "no" }),
                self.cell(status_label, tone),
                Cell::new(path),
                Cell::new(version),
            ]);
        }
        writeln!(out, "{tools}")?;
        writeln!(
            out,
            "{}",
            if report.is_healthy() {
                self.paint("healthy", Tone::Good)
            } else {
                self.paint("problems found", Tone::Bad)
            }
        )
    }

    fn render_error(
        &self,
        error: &AppError,
        _stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> io::Result<()> {
        writeln!(stderr, "error: {error}")
    }
}
