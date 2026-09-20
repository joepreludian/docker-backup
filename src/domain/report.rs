//! Results returned by the application services and rendered by the presentation layer.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::manifest::{Compression, DockerInfo, Manifest, ToolInfo};
use crate::domain::refs::ItemKind;
use crate::domain::verification::VerificationReport;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ItemOutcome {
    Done { size_bytes: u64 },
    Restored,
    SkippedExisting,
    SkippedVolatile,
    Failed { error: String },
}

impl ItemOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, ItemOutcome::Failed { .. })
    }

    pub fn label(&self) -> String {
        match self {
            ItemOutcome::Done { size_bytes } => format!("done ({})", human_size(*size_bytes)),
            ItemOutcome::Restored => "restored".to_string(),
            ItemOutcome::SkippedExisting => "skipped (exists)".to_string(),
            ItemOutcome::SkippedVolatile => "skipped (volatile)".to_string(),
            ItemOutcome::Failed { error } => format!("failed ({error})"),
        }
    }
}

/// Binary units; one decimal below 10 units, none above.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemResult {
    pub kind: ItemKind,
    pub name: String,
    pub file: Option<String>,
    #[serde(flatten)]
    pub outcome: ItemOutcome,
}

fn failed_count(items: &[ItemResult]) -> usize {
    items.iter().filter(|i| i.outcome.is_failure()).count()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupReport {
    pub output: PathBuf,
    pub single_archive: bool,
    pub compression: Compression,
    pub docker: DockerInfo,
    pub items: Vec<ItemResult>,
}

impl BackupReport {
    pub fn failed_count(&self) -> usize {
        failed_count(&self.items)
    }

    pub fn total_bytes(&self) -> u64 {
        self.items
            .iter()
            .map(|i| match i.outcome {
                ItemOutcome::Done { size_bytes } => size_bytes,
                _ => 0,
            })
            .sum()
    }

    pub fn exit_code(&self) -> i32 {
        if self.failed_count() > 0 { 1 } else { 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub source: PathBuf,
    pub items: Vec<ItemResult>,
}

impl RestoreReport {
    pub fn failed_count(&self) -> usize {
        failed_count(&self.items)
    }

    pub fn exit_code(&self) -> i32 {
        if self.failed_count() > 0 { 1 } else { 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoReport {
    pub source: PathBuf,
    pub manifest: Manifest,
    pub verification: VerificationReport,
}

impl InfoReport {
    pub fn exit_code(&self) -> i32 {
        if self.verification.is_ok() { 0 } else { 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStatus {
    pub name: String,
    pub required: bool,
    pub info: Option<ToolInfo>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCounts {
    pub created: usize,
    pub pulled: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeCounts {
    pub named: usize,
    pub volatile: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerCounts {
    pub running: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub docker: Option<DockerInfo>,
    pub docker_error: Option<String>,
    pub images: ImageCounts,
    pub volumes: VolumeCounts,
    pub containers: ContainerCounts,
    pub tools: Vec<ToolStatus>,
}

impl DoctorReport {
    pub fn is_healthy(&self) -> bool {
        self.docker.is_some() && self.tools.iter().all(|t| !t.required || t.info.is_some())
    }

    pub fn exit_code(&self) -> i32 {
        if self.is_healthy() { 0 } else { 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(312 * 1024 * 1024), "312 MiB");
        assert_eq!(
            human_size(5 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "5.5 GiB"
        );
    }

    #[test]
    fn outcome_labels() {
        assert_eq!(
            ItemOutcome::Done { size_bytes: 1024 }.label(),
            "done (1.0 KiB)"
        );
        assert_eq!(ItemOutcome::Restored.label(), "restored");
        assert_eq!(ItemOutcome::SkippedExisting.label(), "skipped (exists)");
        assert_eq!(ItemOutcome::SkippedVolatile.label(), "skipped (volatile)");
        assert_eq!(
            ItemOutcome::Failed {
                error: "boom".into()
            }
            .label(),
            "failed (boom)"
        );
        assert!(
            ItemOutcome::Failed {
                error: "boom".into()
            }
            .is_failure()
        );
        assert!(!ItemOutcome::SkippedExisting.is_failure());
    }

    #[test]
    fn outcome_serializes_with_status_tag() {
        let json = serde_json::to_value(ItemResult {
            kind: ItemKind::Image,
            name: "a".into(),
            file: None,
            outcome: ItemOutcome::Done { size_bytes: 3 },
        })
        .unwrap();
        assert_eq!(json["status"], "done");
        assert_eq!(json["size_bytes"], 3);
        let json = serde_json::to_value(ItemOutcome::SkippedExisting).unwrap();
        assert_eq!(json["status"], "skipped_existing");
    }

    #[test]
    fn backup_report_exit_code_depends_on_failures() {
        let ok = BackupReport {
            output: "out".into(),
            single_archive: false,
            compression: Compression::None,
            docker: DockerInfo::default(),
            items: vec![ItemResult {
                kind: ItemKind::Volume,
                name: "v".into(),
                file: Some("volumes/v.tar".into()),
                outcome: ItemOutcome::Done { size_bytes: 10 },
            }],
        };
        assert_eq!(ok.exit_code(), 0);
        assert_eq!(ok.total_bytes(), 10);
        let mut failed = ok.clone();
        failed.items.push(ItemResult {
            kind: ItemKind::Image,
            name: "i".into(),
            file: None,
            outcome: ItemOutcome::Failed { error: "x".into() },
        });
        assert_eq!(failed.failed_count(), 1);
        assert_eq!(failed.exit_code(), 1);
    }

    #[test]
    fn doctor_health_requires_docker_and_required_tools() {
        let base = DoctorReport {
            docker: Some(DockerInfo::default()),
            docker_error: None,
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
        };
        assert!(base.is_healthy());
        assert_eq!(base.exit_code(), 0);
        let mut no_docker = base.clone();
        no_docker.docker = None;
        no_docker.docker_error = Some("cannot connect".into());
        assert!(!no_docker.is_healthy());
        assert_eq!(no_docker.exit_code(), 1);
        let mut no_tar = base.clone();
        no_tar.tools.push(ToolStatus {
            name: "tar".into(),
            required: true,
            info: None,
        });
        assert!(!no_tar.is_healthy());
    }
}
