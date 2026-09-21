//! Turns report structs into human tables or JSON. Never computes anything.

pub mod human;
pub mod json;
pub mod preview;

use std::io::{self, Write};

use crate::domain::error::AppError;
use crate::domain::report::{BackupReport, DoctorReport, InfoReport, RestoreReport};

pub use preview::{format_arch_warning, format_restore_preview};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    Json,
}

pub trait Renderer {
    fn render_backup(&self, report: &BackupReport, out: &mut dyn Write) -> io::Result<()>;
    fn render_restore(&self, report: &RestoreReport, out: &mut dyn Write) -> io::Result<()>;
    fn render_info(&self, report: &InfoReport, out: &mut dyn Write) -> io::Result<()>;
    fn render_doctor(&self, report: &DoctorReport, out: &mut dyn Write) -> io::Result<()>;
    fn render_error(
        &self,
        error: &AppError,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> io::Result<()>;
}

pub fn renderer_for(format: OutputFormat, color: bool) -> Box<dyn Renderer> {
    match format {
        OutputFormat::Human => Box::new(human::HumanRenderer { color }),
        OutputFormat::Json => Box::new(json::JsonRenderer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::AppError;
    use crate::domain::manifest::{
        Compression, DockerInfo, Manifest, Sha256Digest, ToolInfo, VolumeEntry,
    };
    use crate::domain::platform::Platform;
    use crate::domain::preview::{MismatchedItem, RestorePreview};
    use crate::domain::refs::ItemKind;
    use crate::domain::report::{
        BackupReport, ContainerCounts, DoctorReport, ImageCounts, InfoReport, ItemOutcome,
        ItemResult, RestoreReport, ToolStatus, VolumeCounts,
    };
    use crate::domain::verification::{FileCheck, FileStatus, VerificationReport};
    use serde_json::{Value, json};
    use time::format_description::well_known::Rfc3339;
    use time::macros::datetime;

    fn backup_report() -> BackupReport {
        BackupReport {
            output: "/backups/out".into(),
            single_archive: false,
            compression: Compression::None,
            docker: DockerInfo {
                server_version: "29.5.2".into(),
                ..DockerInfo::default()
            },
            items: vec![
                ItemResult {
                    kind: ItemKind::Volume,
                    name: "pgdata".into(),
                    file: Some("volumes/pgdata.tar".into()),
                    outcome: ItemOutcome::Done { size_bytes: 2048 },
                },
                ItemResult {
                    kind: ItemKind::Image,
                    name: "app:latest".into(),
                    file: None,
                    outcome: ItemOutcome::Failed {
                        error: "boom".into(),
                    },
                },
            ],
        }
    }

    fn info_report() -> InfoReport {
        let mut manifest = Manifest::new(
            datetime!(2026-09-19 14:03:11 UTC),
            DockerInfo::default(),
            Compression::None,
        );
        manifest.volumes.push(VolumeEntry {
            name: "pgdata".into(),
            file: "volumes/pgdata.tar".into(),
            size_bytes: 3,
            sha256: Sha256Digest::of(b"abc"),
            volatile: false,
            inspect: json!({}),
        });
        InfoReport {
            source: "/backups/out".into(),
            manifest,
            verification: VerificationReport {
                files: vec![FileCheck {
                    kind: ItemKind::Volume,
                    name: "pgdata".into(),
                    file: "volumes/pgdata.tar".into(),
                    size_bytes: 3,
                    status: FileStatus::Missing,
                }],
            },
        }
    }

    fn restore_preview() -> RestorePreview {
        RestorePreview {
            source: "/backups/out".into(),
            created_at: datetime!(2026-09-19 14:03:11 UTC),
            backup_platform: Platform::new("linux", "amd64"),
            target_platform: Platform::new("linux", "arm64"),
            overwrite: false,
            force_arch_mismatch: false,
            volumes_to_create: 1,
            volumes_to_overwrite: 0,
            volumes_skipped: 0,
            images_to_load: 0,
            containers_to_import: 0,
            mismatches: vec![MismatchedItem {
                kind: ItemKind::Image,
                name: "app:latest".into(),
                platform: Platform::new("linux", "amd64"),
            }],
        }
    }

    fn doctor_report() -> DoctorReport {
        DoctorReport {
            docker: None,
            docker_error: Some("cannot connect".into()),
            images: ImageCounts::default(),
            volumes: VolumeCounts::default(),
            containers: ContainerCounts::default(),
            tools: vec![
                ToolStatus {
                    name: "docker".into(),
                    required: true,
                    info: Some(ToolInfo {
                        path: "/usr/bin/docker".into(),
                        version: "29".into(),
                    }),
                },
                ToolStatus {
                    name: "bzip2".into(),
                    required: false,
                    info: None,
                },
            ],
        }
    }

    fn render_json<F: Fn(&dyn Renderer, &mut dyn Write) -> io::Result<()>>(f: F) -> Value {
        let renderer = renderer_for(OutputFormat::Json, false);
        let mut out = Vec::new();
        f(renderer.as_ref(), &mut out).unwrap();
        serde_json::from_slice(&out).expect("valid json")
    }

    fn render_human<F: Fn(&dyn Renderer, &mut dyn Write) -> io::Result<()>>(f: F) -> String {
        let renderer = renderer_for(OutputFormat::Human, false);
        let mut out = Vec::new();
        f(renderer.as_ref(), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn json_backup_is_one_document_with_items() {
        let value = render_json(|r, out| r.render_backup(&backup_report(), out));
        assert_eq!(value["output"], "/backups/out");
        assert_eq!(value["items"][0]["status"], "done");
        assert_eq!(value["items"][1]["status"], "failed");
        assert_eq!(value["items"][1]["error"], "boom");
    }

    #[test]
    fn json_restore_serializes_preview() {
        let restore = RestoreReport {
            source: "/b".into(),
            preview: Some(restore_preview()),
            items: vec![],
        };
        let value = render_json(|r, out| r.render_restore(&restore, out));
        assert_eq!(value["preview"]["target_platform"]["arch"], "arm64");
        assert_eq!(value["preview"]["mismatches"][0]["name"], "app:latest");
        let created_at = value["preview"]["created_at"]
            .as_str()
            .expect("created_at should be a string");
        assert!(
            time::OffsetDateTime::parse(created_at, &Rfc3339).is_ok(),
            "created_at should be an RFC 3339 string, got {created_at:?}"
        );
    }

    #[test]
    fn json_info_and_doctor_serialize() {
        let info = render_json(|r, out| r.render_info(&info_report(), out));
        assert_eq!(info["verification"]["files"][0]["status"], "missing");
        assert_eq!(info["manifest"]["schema_version"], 1);
        let doctor = render_json(|r, out| r.render_doctor(&doctor_report(), out));
        assert_eq!(doctor["docker_error"], "cannot connect");
        assert_eq!(doctor["tools"][1]["info"], Value::Null);
    }

    #[test]
    fn json_error_goes_to_stdout() {
        let renderer = renderer_for(OutputFormat::Json, false);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        renderer
            .render_error(
                &AppError::DockerUnavailable("down".into()),
                &mut out,
                &mut err,
            )
            .unwrap();
        let value: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["error"]["kind"], "docker_unavailable");
        assert!(value["error"]["message"].as_str().unwrap().contains("down"));
        assert!(err.is_empty());
    }

    #[test]
    fn human_backup_lists_items_and_summary() {
        let text = render_human(|r, out| r.render_backup(&backup_report(), out));
        assert!(text.contains("/backups/out"));
        assert!(text.contains("pgdata"));
        assert!(text.contains("2.0 KiB"));
        assert!(text.contains("failed"));
        assert!(text.contains("1 failed"));
        assert!(!text.contains("\u{1b}["), "no ANSI when color is off");
    }

    #[test]
    fn human_restore_info_and_doctor_render() {
        let restore = RestoreReport {
            source: "/b".into(),
            // Populated so JSON coverage exists elsewhere (json_restore_serializes_preview);
            // the human renderer must still ignore it and only show the items table below.
            preview: Some(restore_preview()),
            items: vec![ItemResult {
                kind: ItemKind::Volume,
                name: "v".into(),
                file: None,
                outcome: ItemOutcome::SkippedExisting,
            }],
        };
        let text = render_human(|r, out| r.render_restore(&restore, out));
        assert!(text.contains("skipped (exists)"));
        assert!(
            !text.contains("app:latest"),
            "human restore output ignores the preview"
        );

        let text = render_human(|r, out| r.render_info(&info_report(), out));
        assert!(text.contains("2026-09-19T14:03:11Z"));
        assert!(text.contains("missing"));
        assert!(text.contains("0 ok, 1 missing, 0 corrupt"));

        let text = render_human(|r, out| r.render_doctor(&doctor_report(), out));
        assert!(text.contains("cannot connect"));
        assert!(text.contains("bzip2"));
        assert!(text.contains("/usr/bin/docker"));
    }

    #[test]
    fn human_error_goes_to_stderr() {
        let renderer = renderer_for(OutputFormat::Human, false);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        renderer
            .render_error(&AppError::Conflict("nope".into()), &mut out, &mut err)
            .unwrap();
        assert!(out.is_empty());
        assert_eq!(String::from_utf8(err).unwrap(), "error: nope\n");
    }

    /// Removes `ESC [ ... m` SGR sequences so table alignment can be checked on
    /// the visible text alone, without pulling in a new dependency.
    fn strip_ansi(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' && chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                for c2 in chars.by_ref() {
                    if c2 == 'm' {
                        break;
                    }
                }
            } else {
                result.push(c);
            }
        }
        result
    }

    #[test]
    fn human_color_true_emits_ansi_and_keeps_tables_aligned() {
        let renderer = renderer_for(OutputFormat::Human, true);
        let mut out = Vec::new();
        renderer.render_backup(&backup_report(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\u{1b}["), "color=true should emit ANSI");

        let stripped = strip_ansi(&text);
        let mut widths = stripped
            .lines()
            .filter(|line| line.starts_with('\u{2502}'))
            .map(|line| line.chars().count());
        let first = widths.next().expect("at least one table row line");
        for width in widths {
            assert_eq!(
                width, first,
                "table rows must stay aligned once ANSI is stripped"
            );
        }
    }

    #[test]
    fn human_info_shows_readable_compression_label() {
        let mut report = info_report();
        report.manifest.compression = Compression::Bzip2PerFile;
        let text = render_human(|r, out| r.render_info(&report, out));
        assert!(text.contains("bzip2-per-file"));
    }
}
