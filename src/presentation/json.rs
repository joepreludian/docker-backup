use std::io::{self, Write};

use serde::Serialize;
use serde_json::json;

use crate::domain::error::AppError;
use crate::domain::report::{BackupReport, DoctorReport, InfoReport, RestoreReport};
use crate::presentation::Renderer;

pub struct JsonRenderer;

fn emit<T: Serialize>(value: &T, out: &mut dyn Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)
}

impl Renderer for JsonRenderer {
    fn render_backup(&self, report: &BackupReport, out: &mut dyn Write) -> io::Result<()> {
        emit(report, out)
    }

    fn render_restore(&self, report: &RestoreReport, out: &mut dyn Write) -> io::Result<()> {
        emit(report, out)
    }

    fn render_info(&self, report: &InfoReport, out: &mut dyn Write) -> io::Result<()> {
        emit(report, out)
    }

    fn render_doctor(&self, report: &DoctorReport, out: &mut dyn Write) -> io::Result<()> {
        emit(report, out)
    }

    fn render_error(
        &self,
        error: &AppError,
        stdout: &mut dyn Write,
        _stderr: &mut dyn Write,
    ) -> io::Result<()> {
        emit(
            &json!({ "error": { "kind": error.kind(), "message": error.to_string() } }),
            stdout,
        )
    }
}
