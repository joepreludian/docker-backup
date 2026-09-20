//! Shared helpers: find a backup (folder or archive), read its manifest, check its files.

use std::path::{Path, PathBuf};

use crate::application::ports::ArchiveStore;
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::{MANIFEST_FILE, Manifest};
use crate::domain::naming::is_archive;
use crate::domain::verification::{FileCheck, FileStatus, VerificationReport};

#[derive(Debug)]
pub struct LocatedBackup {
    pub root: PathBuf,
    pub temp: Option<PathBuf>,
}

impl LocatedBackup {
    /// Best-effort: scratch cleanup must never mask the operation's own result.
    pub fn cleanup(&self, store: &dyn ArchiveStore) {
        if let Some(temp) = &self.temp {
            let _ = store.remove_dir_all(temp);
        }
    }
}

/// A folder is used as-is; a `.tar.bz2` is unpacked into a temp dir beside it.
pub fn locate_backup(store: &dyn ArchiveStore, source: &Path) -> AppResult<LocatedBackup> {
    if !is_archive(source) {
        return Ok(LocatedBackup {
            root: source.to_path_buf(),
            temp: None,
        });
    }
    let parent = source
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let temp = store.make_temp_dir(&parent)?;
    if let Err(error) = store.unpack_archive(source, &temp) {
        let _ = store.remove_dir_all(&temp);
        return Err(error);
    }
    Ok(LocatedBackup {
        root: temp.clone(),
        temp: Some(temp),
    })
}

pub fn read_manifest(store: &dyn ArchiveStore, root: &Path) -> AppResult<Manifest> {
    let path = root.join(MANIFEST_FILE);
    if !store.exists(&path) {
        return Err(AppError::ManifestInvalid(format!(
            "no {MANIFEST_FILE} in {}",
            root.display()
        )));
    }
    Manifest::from_json(&store.read_text(&path)?)
}

pub fn verify_files(
    store: &dyn ArchiveStore,
    root: &Path,
    manifest: &Manifest,
) -> AppResult<VerificationReport> {
    let mut files = Vec::with_capacity(manifest.item_count());
    for entry in manifest.files() {
        let actual = store.hash_file(&root.join(&entry.file))?;
        files.push(FileCheck {
            kind: entry.kind,
            name: entry.name,
            file: entry.file,
            size_bytes: entry.size_bytes,
            status: FileStatus::classify(&entry.sha256, actual.as_ref()),
        });
    }
    Ok(VerificationReport { files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::fakes::MemoryArchiveStore;
    use crate::domain::manifest::{Compression, DockerInfo, Sha256Digest, VolumeEntry};
    use crate::domain::verification::FileStatus;
    use serde_json::json;
    use time::macros::datetime;

    fn manifest_with(files: &[(&str, &[u8])]) -> Manifest {
        let mut m = Manifest::new(
            datetime!(2026-09-19 00:00:00 UTC),
            DockerInfo::default(),
            Compression::None,
        );
        for (name, content) in files {
            m.volumes.push(VolumeEntry {
                name: name.to_string(),
                file: format!("volumes/{name}.tar"),
                size_bytes: content.len() as u64,
                sha256: Sha256Digest::of(content),
                volatile: false,
                inspect: json!({}),
            });
        }
        m
    }

    #[test]
    fn classifies_ok_missing_and_corrupt() {
        let store = MemoryArchiveStore::new();
        store.put("/b/volumes/good.tar", b"good");
        store.put("/b/volumes/bad.tar", b"tampered");
        let manifest = manifest_with(&[("good", b"good"), ("bad", b"bad"), ("gone", b"gone")]);
        let report = verify_files(&store, Path::new("/b"), &manifest).unwrap();
        assert_eq!(report.files[0].status, FileStatus::Ok);
        assert!(matches!(report.files[1].status, FileStatus::Corrupt { .. }));
        assert_eq!(report.files[2].status, FileStatus::Missing);
        assert_eq!(report.files[1].file, "volumes/bad.tar");
    }

    #[test]
    fn locate_backup_uses_folder_directly() {
        let store = MemoryArchiveStore::new();
        store.put("/b/manifest.json", b"{}");
        let located = locate_backup(&store, Path::new("/b")).unwrap();
        assert_eq!(located.root, Path::new("/b"));
        assert!(located.temp.is_none());
    }

    #[test]
    fn locate_backup_unpacks_archives_into_a_temp_dir() {
        let store = MemoryArchiveStore::new();
        store.put("/x/out/manifest.json", b"{}");
        store
            .pack_folder(Path::new("/x/out"), Path::new("/x/out.tar.bz2"))
            .unwrap();
        let located = locate_backup(&store, Path::new("/x/out.tar.bz2")).unwrap();
        assert!(located.temp.is_some());
        assert!(store.exists(&located.root.join("manifest.json")));
        located.cleanup(&store);
        assert!(!store.exists(&located.root));
    }

    #[test]
    fn unpack_failure_removes_the_temp_dir_and_returns_the_error() {
        let store = MemoryArchiveStore::new();
        store.put("/x/out/manifest.json", b"{}");
        store
            .pack_folder(Path::new("/x/out"), Path::new("/x/out.tar.bz2"))
            .unwrap();
        store.fail_unpack.set(true);
        let err = locate_backup(&store, Path::new("/x/out.tar.bz2")).unwrap_err();
        assert!(matches!(err, AppError::ToolFailed { .. }));
        assert!(!store.exists(Path::new("/x/.docker-backup-tmp-1")));
    }

    #[test]
    fn read_manifest_reports_missing_file() {
        let store = MemoryArchiveStore::new();
        let err = read_manifest(&store, Path::new("/nothing")).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("manifest.json")));
    }
}
