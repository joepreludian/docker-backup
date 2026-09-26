//! A dry-run summary of what a restore would do, built from a `RestorePlan`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::domain::platform::Platform;
use crate::domain::refs::ItemKind;

/// An item that goes back under another name than it was backed up as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remap {
    pub kind: ItemKind,
    pub from: String,
    pub to: String,
}

/// The compose side of a `restore --from-docker-compose` preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposePreview {
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_project: Option<String>,
    pub remaps: Vec<Remap>,
    pub not_in_backup: Vec<String>,
}

/// One image or container whose platform doesn't match the restore target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MismatchedItem {
    pub kind: ItemKind,
    pub name: String,
    pub platform: Platform,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePreview {
    pub source: PathBuf,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub backup_platform: Platform,
    pub target_platform: Platform,
    pub overwrite: bool,
    pub force_arch_mismatch: bool,
    /// Restore && !exists
    pub volumes_to_create: usize,
    /// Restore && exists
    pub volumes_to_overwrite: usize,
    /// SkipExisting + SkipVolatile
    pub volumes_skipped: usize,
    /// Images with action Restore
    pub images_to_load: usize,
    /// Containers with action Restore
    pub containers_to_import: usize,
    /// Every image/container whose platform is known and != target, regardless
    /// of force. An unknown platform can't prove a mismatch, so it's excluded.
    pub mismatches: Vec<MismatchedItem>,
    /// Set only for `restore --from-docker-compose`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose: Option<ComposePreview>,
}

impl RestorePreview {
    /// How many of `mismatches` will actually be skipped (0 when forced).
    pub fn skipped_for_arch(&self) -> usize {
        if self.force_arch_mismatch {
            0
        } else {
            self.mismatches.len()
        }
    }
}

/// What `restore-volume --overwrite` asks about before emptying an existing volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeOverwritePrompt {
    /// The volume that will be emptied and refilled.
    pub target: String,
    /// The archive it will be refilled from.
    pub source: PathBuf,
    /// When that archive was made.
    pub created_at: OffsetDateTime,
}
