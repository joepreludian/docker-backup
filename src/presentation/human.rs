//! Tables for terminals. Colour is opt-in so piped output stays clean.

use std::io::{self, Write};

use comfy_table::{Cell, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use console::style;

use crate::domain::error::AppError;
use crate::domain::report::{
    BackupReport, DoctorReport, InfoReport, ItemOutcome, ItemResult, RestoreReport, human_size,
};
use crate::domain::verification::FileStatus;
use crate::presentation::Renderer;

pub struct HumanRenderer {
    pub color: bool,
}

impl HumanRenderer {
    fn table(&self, header: &[&str]) -> Table {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(header.to_vec());
        table
    }

    fn paint(&self, text: &str, tone: Tone) -> String {
        if !self.color {
            return text.to_string();
        }
        match tone {
            Tone::Good => style(text).green().to_string(),
            Tone::Warn => style(text).yellow().to_string(),
            Tone::Bad => style(text).red().to_string(),
        }
    }

    fn outcome_cell(&self, outcome: &ItemOutcome) -> String {
        let tone = match outcome {
            ItemOutcome::Done { .. } | ItemOutcome::Restored => Tone::Good,
            ItemOutcome::SkippedExisting | ItemOutcome::SkippedVolatile => Tone::Warn,
            ItemOutcome::Failed { .. } => Tone::Bad,
        };
        self.paint(&outcome.label(), tone)
    }

    fn items_table(&self, items: &[ItemResult]) -> Table {
        let mut table = self.table(&["Kind", "Name", "File", "Status"]);
        for item in items {
            table.add_row(vec![
                Cell::new(item.kind.to_string()),
                Cell::new(&item.name),
                Cell::new(item.file.as_deref().unwrap_or("-")),
                Cell::new(self.outcome_cell(&item.outcome)),
            ]);
        }
        table
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Good,
    Warn,
    Bad,
}

impl Renderer for HumanRenderer {
    fn render_backup(&self, report: &BackupReport, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "Backup written to {}", report.output.display())?;
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
            Cell::new(format!("{:?}", m.compression).to_lowercase()),
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

        let mut files = self.table(&["Kind", "Name", "File", "Size", "Status"]);
        for check in &report.verification.files {
            let status = match &check.status {
                FileStatus::Ok => self.paint("ok", Tone::Good),
                FileStatus::Missing => self.paint("missing", Tone::Bad),
                FileStatus::Corrupt { .. } => self.paint("corrupt", Tone::Bad),
            };
            files.add_row(vec![
                check.kind.to_string(),
                check.name.clone(),
                check.file.clone(),
                human_size(check.size_bytes),
                status,
            ]);
        }
        writeln!(out, "{files}")?;
        writeln!(out, "{}", report.verification.summary())
    }

    fn render_doctor(&self, report: &DoctorReport, out: &mut dyn Write) -> io::Result<()> {
        let mut docker = self.table(&["Docker", "Value"]);
        match (&report.docker, &report.docker_error) {
            (Some(info), _) => {
                docker.add_row(vec![
                    Cell::new("Status"),
                    Cell::new(self.paint("available", Tone::Good)),
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
                    Cell::new(self.paint("unavailable", Tone::Bad)),
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
            let (status, path, version) = match &tool.info {
                Some(info) => (
                    self.paint("found", Tone::Good),
                    info.path.clone(),
                    info.version.clone(),
                ),
                None if tool.required => (self.paint("missing", Tone::Bad), "-".into(), "-".into()),
                None => (self.paint("missing", Tone::Warn), "-".into(), "-".into()),
            };
            tools.add_row(vec![
                tool.name.clone(),
                if tool.required {
                    "yes".into()
                } else {
                    "no".into()
                },
                status,
                path,
                version,
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
