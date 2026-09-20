//! Info use case: read a backup's manifest and check its files. Never touches docker.

use std::path::Path;

use crate::application::ports::ArchiveStore;
use crate::application::verify::{locate_backup, read_manifest, verify_files};
use crate::domain::error::AppResult;
use crate::domain::report::InfoReport;

pub struct InfoService<'a> {
    pub store: &'a dyn ArchiveStore,
}

impl InfoService<'_> {
    pub fn run(&self, source: &Path) -> AppResult<InfoReport> {
        let located = locate_backup(self.store, source)?;
        let result = read_manifest(self.store, &located.root).and_then(|manifest| {
            let verification = verify_files(self.store, &located.root, &manifest)?;
            Ok(InfoReport {
                source: source.to_path_buf(),
                manifest,
                verification,
            })
        });
        located.cleanup(self.store);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::fakes::MemoryArchiveStore;
    use crate::domain::error::AppError;
    use crate::domain::manifest::{
        Compression, DockerInfo, MANIFEST_FILE, Manifest, Sha256Digest, VolumeEntry,
    };
    use crate::domain::verification::FileStatus;
    use serde_json::json;
    use std::path::Path;
    use time::macros::datetime;

    fn seed(store: &MemoryArchiveStore) {
        let mut m = Manifest::new(
            datetime!(2026-09-19 00:00:00 UTC),
            DockerInfo::default(),
            Compression::None,
        );
        store.put("/b/volumes/a.tar", b"A");
        store.put("/b/volumes/b.tar", b"WRONG");
        for name in ["a", "b"] {
            m.volumes.push(VolumeEntry {
                name: name.into(),
                file: format!("volumes/{name}.tar"),
                size_bytes: 1,
                sha256: Sha256Digest::of(name.to_uppercase().as_bytes()),
                volatile: false,
                inspect: json!({}),
            });
        }
        store
            .write_text(&Path::new("/b").join(MANIFEST_FILE), &m.to_json().unwrap())
            .unwrap();
    }

    #[test]
    fn info_reports_manifest_and_file_status() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let report = InfoService { store: &store }.run(Path::new("/b")).unwrap();
        assert_eq!(report.manifest.volumes.len(), 2);
        assert_eq!(report.verification.files[0].status, FileStatus::Ok);
        assert!(matches!(
            report.verification.files[1].status,
            FileStatus::Corrupt { .. }
        ));
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn info_on_archive_unpacks_then_cleans_up() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        store
            .pack_folder(Path::new("/b"), Path::new("/b.tar.bz2"))
            .unwrap();
        let report = InfoService { store: &store }
            .run(Path::new("/b.tar.bz2"))
            .unwrap();
        assert_eq!(report.source, Path::new("/b.tar.bz2"));
        assert_eq!(report.verification.files.len(), 2);
        assert!(
            store
                .paths_under(Path::new("/.docker-backup-tmp-1"))
                .is_empty()
        );
    }

    #[test]
    fn info_without_manifest_is_an_error() {
        let store = MemoryArchiveStore::new();
        let err = InfoService { store: &store }
            .run(Path::new("/nope"))
            .unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(_)));
    }
}
