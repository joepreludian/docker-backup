use std::io::{self, Write};

use serde::Serialize;
use serde_json::json;

use crate::domain::error::AppError;
use crate::domain::report::{
    BackupReport, DoctorReport, InfoReport, RestoreReport, VolumeBackupReport, VolumeInfoReport,
    VolumeRestoreReport,
};
use crate::presentation::Renderer;

pub struct JsonRenderer;

fn emit<T: Serialize>(value: &T, out: &mut dyn Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)
}

/// A report's own fields after a leading `"type"`, which tells a single-volume
/// `info` apart from a full backup's (whose JSON stays exactly as it was).
#[derive(Serialize)]
struct Tagged<'a, T> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(flatten)]
    report: &'a T,
}

impl Renderer for JsonRenderer {
    fn render_backup(&self, report: &BackupReport, out: &mut dyn Write) -> io::Result<()> {
        emit(report, out)
    }

    fn render_restore(&self, report: &RestoreReport, out: &mut dyn Write) -> io::Result<()> {
        emit(report, out)
    }

    fn render_volume_backup(
        &self,
        report: &VolumeBackupReport,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        emit(report, out)
    }

    fn render_volume_restore(
        &self,
        report: &VolumeRestoreReport,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        emit(report, out)
    }

    fn render_info(&self, report: &InfoReport, out: &mut dyn Write) -> io::Result<()> {
        emit(report, out)
    }

    fn render_volume_info(&self, report: &VolumeInfoReport, out: &mut dyn Write) -> io::Result<()> {
        emit(
            &Tagged {
                kind: "volume",
                report,
            },
            out,
        )
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
