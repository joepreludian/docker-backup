//! Interfaces the application services depend on. Infrastructure implements
//! them; tests substitute in-memory fakes.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use time::OffsetDateTime;

use crate::domain::error::AppResult;
use crate::domain::manifest::{Compression, DockerInfo, Sha256Digest, ToolInfo};
use crate::domain::preview::RestorePreview;
use crate::domain::refs::{ContainerRef, ImageRef, ItemKind, VolumeRef};
use crate::domain::report::ItemOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Backup,
    Restore,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Operation::Backup => "BACKUP",
            Operation::Restore => "RESTORE",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
}

/// Everything we ask of the `docker` CLI.
pub trait DockerPort {
    fn engine_info(&self) -> AppResult<DockerInfo>;
    fn list_volumes(&self) -> AppResult<Vec<VolumeRef>>;
    fn inspect_volume(&self, name: &str) -> AppResult<Value>;
    fn list_images(&self) -> AppResult<Vec<ImageRef>>;
    fn inspect_image(&self, id: &str) -> AppResult<Value>;
    fn list_containers(&self) -> AppResult<Vec<ContainerRef>>;
    fn inspect_container(&self, name: &str) -> AppResult<Value>;
    /// Pull the helper image used for volume export/import if it is absent.
    fn ensure_helper_image(&self) -> AppResult<()>;
    fn export_volume(&self, name: &str, sink: &mut dyn Write) -> AppResult<()>;
    fn save_image(&self, reference: &str, sink: &mut dyn Write) -> AppResult<()>;
    fn export_container(&self, name: &str, sink: &mut dyn Write) -> AppResult<()>;
    fn create_volume(&self, name: &str) -> AppResult<()>;
    /// Stream a tar into the volume; `wipe` empties it first.
    fn import_volume(&self, name: &str, source: &mut dyn Read, wipe: bool) -> AppResult<()>;
    fn load_image(&self, source: &mut dyn Read) -> AppResult<()>;
    fn import_container_fs(&self, source: &mut dyn Read, tag: &str) -> AppResult<()>;
}

/// Files on disk (or in memory) that make up a backup folder.
pub trait ArchiveStore {
    fn create_dir_all(&self, path: &Path) -> AppResult<()>;
    fn remove_dir_all(&self, path: &Path) -> AppResult<()>;
    fn remove_file(&self, path: &Path) -> AppResult<()>;
    fn exists(&self, path: &Path) -> bool;
    fn write_text(&self, path: &Path, contents: &str) -> AppResult<()>;
    fn read_text(&self, path: &Path) -> AppResult<String>;
    /// Run `producer` against a sink that ends up at `path` (compressed when asked),
    /// atomically (temp file + rename). Returns size and digest of the stored bytes.
    fn write_item(
        &self,
        path: &Path,
        compression: Compression,
        producer: &mut dyn FnMut(&mut dyn Write) -> AppResult<()>,
    ) -> AppResult<StoredFile>;
    /// Open a stored item for reading, decompressing when needed.
    fn open_item(&self, path: &Path, compression: Compression) -> AppResult<Box<dyn Read>>;
    /// `None` when the file does not exist.
    fn hash_file(&self, path: &Path) -> AppResult<Option<Sha256Digest>>;
    /// `tar -cjf archive -C folder.parent folder.file_name`.
    fn pack_folder(&self, folder: &Path, archive: &Path) -> AppResult<()>;
    /// `tar -xjf archive -C into --strip-components=1`.
    fn unpack_archive(&self, archive: &Path, into: &Path) -> AppResult<()>;
    /// A fresh empty directory inside `parent`; caller removes it.
    fn make_temp_dir(&self, parent: &Path) -> AppResult<PathBuf>;
}

pub trait ToolLocator {
    fn locate(&self, name: &str) -> Option<ToolInfo>;
}

pub trait ProgressSink {
    fn start(&self, operation: Operation, total: usize);
    fn item_started(&self, kind: ItemKind, name: &str, index: usize);
    fn item_finished(&self, kind: ItemKind, name: &str, outcome: &ItemOutcome);
    fn finish(&self);
}

/// Asked before restore writes anything. `Err(AppError::Aborted(..))` stops the restore.
pub trait ConfirmPort {
    /// Shows the preview and asks to proceed.
    fn confirm_restore(&self, preview: &RestorePreview) -> AppResult<()>;
    /// Called only when `preview.mismatches` is not empty; asks "are you sure".
    fn confirm_arch_mismatch(&self, preview: &RestorePreview) -> AppResult<()>;
}

pub trait Clock {
    fn now_utc(&self) -> OffsetDateTime;
}
