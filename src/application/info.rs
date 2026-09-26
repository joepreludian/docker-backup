//! Info use case: read a backup's `manifest.json`, or a single-volume archive's
//! `volume.json`, and check its files. Never touches docker.

use std::path::Path;

use crate::application::ports::ArchiveStore;
use crate::application::verify::{locate_backup, read_manifest, verify_files};
use crate::application::volume::verify_volume_archive;
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::MANIFEST_FILE;
use crate::domain::report::{Info, InfoReport, VolumeInfoReport};
use crate::domain::volume_manifest::VOLUME_MANIFEST_FILE;

pub struct InfoService<'a> {
    pub store: &'a dyn ArchiveStore,
}

impl InfoService<'_> {
    pub fn run(&self, source: &Path) -> AppResult<Info> {
        let located = locate_backup(self.store, source)?;
        let result = self.inspect(source, &located.root);
        located.cleanup(self.store);
        result
    }

    /// `manifest.json` means a full backup; otherwise `volume.json` means a
    /// single-volume archive, or the folder `tar -xjf` makes of one.
    fn inspect(&self, source: &Path, root: &Path) -> AppResult<Info> {
        if self.store.exists(&root.join(MANIFEST_FILE)) {
            let manifest = read_manifest(self.store, root)?;
            let verification = verify_files(self.store, root, &manifest)?;
            return Ok(Info::Backup(Box::new(InfoReport {
                source: source.to_path_buf(),
                manifest,
                verification,
            })));
        }
        if self.store.exists(&root.join(VOLUME_MANIFEST_FILE)) {
            let (manifest, verification) = verify_volume_archive(self.store, root)?;
            return Ok(Info::Volume(VolumeInfoReport {
                source: source.to_path_buf(),
                manifest,
                verification,
            }));
        }
        Err(AppError::ManifestInvalid(format!(
            "no {MANIFEST_FILE} or {VOLUME_MANIFEST_FILE} in {}",
            source.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::fakes::MemoryArchiveStore;
    use crate::domain::manifest::{
        Compression, DockerInfo, MANIFEST_FILE, Manifest, Sha256Digest, VolumeEntry,
    };
    use crate::domain::refs::ItemKind;
    use crate::domain::verification::{FileCheck, FileStatus};
    use crate::domain::volume_manifest::VolumeManifest;
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
                compose: None,
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
        let report = backup_report(InfoService { store: &store }.run(Path::new("/b")).unwrap());
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
        let report = backup_report(
            InfoService { store: &store }
                .run(Path::new("/b.tar.bz2"))
                .unwrap(),
        );
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

    const VOLUME_ARCHIVE: &str = "/backups/pgdata-20260926T141500Z.tar.bz2";

    fn backup_report(info: Info) -> InfoReport {
        match info {
            Info::Backup(report) => *report,
            other => panic!("expected a full backup, got {other:?}"),
        }
    }

    fn volume_report(info: Info) -> VolumeInfoReport {
        match info {
            Info::Volume(report) => report,
            other => panic!("expected a single-volume archive, got {other:?}"),
        }
    }

    fn volume_json(data: &[u8]) -> String {
        VolumeManifest::new(
            "pgdata",
            datetime!(2026-09-26 14:15:00 UTC),
            data.len() as u64,
            Sha256Digest::of(data),
        )
        .to_json()
        .unwrap()
    }

    /// Writes `files` into /work/pgdata-20260926T141500Z/ and packs that folder
    /// into VOLUME_ARCHIVE, the way backup-volume lays an archive out.
    fn pack_volume_archive(store: &MemoryArchiveStore, files: &[(&str, &[u8])]) {
        let folder = Path::new("/work/pgdata-20260926T141500Z");
        for (name, bytes) in files {
            store.put(folder.join(name), bytes);
        }
        store
            .pack_folder(folder, Path::new(VOLUME_ARCHIVE))
            .unwrap();
        store.remove_dir_all(Path::new("/work")).unwrap();
    }

    fn no_scratch_left(store: &MemoryArchiveStore) -> bool {
        !store.exists(Path::new("/backups/.docker-backup-tmp-1"))
    }

    #[test]
    fn a_volume_archive_is_reported_as_a_single_volume() {
        let store = MemoryArchiveStore::new();
        let json = volume_json(b"PG");
        pack_volume_archive(
            &store,
            &[("volume.json", json.as_bytes()), ("backup.tar", b"PG")],
        );
        let info = InfoService { store: &store }
            .run(Path::new(VOLUME_ARCHIVE))
            .unwrap();
        let report = volume_report(info);
        assert_eq!(report.source, Path::new(VOLUME_ARCHIVE));
        assert_eq!(report.manifest.volume, "pgdata");
        assert_eq!(
            report.verification.files,
            vec![FileCheck {
                kind: ItemKind::Volume,
                name: "pgdata".into(),
                file: "backup.tar".into(),
                size_bytes: 2,
                status: FileStatus::Ok,
            }]
        );
        assert_eq!(report.exit_code(), 0);
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn a_corrupt_backup_tar_is_reported_with_exit_code_1() {
        let store = MemoryArchiveStore::new();
        let json = volume_json(b"PG");
        pack_volume_archive(
            &store,
            &[
                ("volume.json", json.as_bytes()),
                ("backup.tar", b"TAMPERED"),
            ],
        );
        let info = InfoService { store: &store }
            .run(Path::new(VOLUME_ARCHIVE))
            .unwrap();
        let report = volume_report(info);
        assert!(matches!(
            report.verification.files[0].status,
            FileStatus::Corrupt { .. }
        ));
        assert_eq!(report.exit_code(), 1);
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn a_missing_backup_tar_is_reported_with_exit_code_1() {
        let store = MemoryArchiveStore::new();
        let json = volume_json(b"PG");
        pack_volume_archive(&store, &[("volume.json", json.as_bytes())]);
        let info = InfoService { store: &store }
            .run(Path::new(VOLUME_ARCHIVE))
            .unwrap();
        let report = volume_report(info);
        assert_eq!(report.verification.files[0].status, FileStatus::Missing);
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_hand_extracted_volume_folder_is_read_in_place() {
        let store = MemoryArchiveStore::new();
        let json = volume_json(b"PG");
        store.put("/x/pgdata-20260926T141500Z/volume.json", json.as_bytes());
        store.put("/x/pgdata-20260926T141500Z/backup.tar", b"PG");
        let info = InfoService { store: &store }
            .run(Path::new("/x/pgdata-20260926T141500Z"))
            .unwrap();
        assert_eq!(
            volume_report(info).verification.files[0].status,
            FileStatus::Ok
        );
        assert!(store.unpacked.borrow().is_empty());
    }

    #[test]
    fn a_folder_with_neither_manifest_is_an_error() {
        let store = MemoryArchiveStore::new();
        store.put("/x/notes.txt", b"hi");
        let err = InfoService { store: &store }
            .run(Path::new("/x"))
            .unwrap_err();
        assert!(matches!(
            err,
            AppError::ManifestInvalid(msg)
                if msg == "no manifest.json or volume.json in /x"
        ));
    }

    #[test]
    fn an_invalid_volume_json_is_an_error_and_the_scratch_dir_goes() {
        let store = MemoryArchiveStore::new();
        pack_volume_archive(
            &store,
            &[
                ("volume.json", br#"{"schema_version": 2}"#),
                ("backup.tar", b"PG"),
            ],
        );
        let err = InfoService { store: &store }
            .run(Path::new(VOLUME_ARCHIVE))
            .unwrap_err();
        assert!(matches!(err, AppError::ManifestUnsupportedVersion(2)));
        assert!(no_scratch_left(&store));
    }
}
