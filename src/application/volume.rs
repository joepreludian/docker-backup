//! Single-volume archives: `backup-volume`, `restore-volume`, and the check that
//! `info` shares with `restore-volume`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use crate::application::ports::{
    ArchiveStore, Clock, DockerPort, Operation, ProgressSink, StoredFile,
};
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::Compression;
use crate::domain::naming::{ARCHIVE_SUFFIX, utc_stamp, volume_archive_stem};
use crate::domain::refs::ItemKind;
use crate::domain::report::{ItemOutcome, ItemResult, VolumeBackupReport};
use crate::domain::verification::{FileCheck, FileStatus, VerificationReport};
use crate::domain::volume_manifest::{VOLUME_DATA_FILE, VOLUME_MANIFEST_FILE, VolumeManifest};

/// Reads `volume.json` in `root` and checks `backup.tar` against it. `info` and
/// `restore-volume` both call this, so they never disagree on what "verified" means.
pub fn verify_volume_archive(
    store: &dyn ArchiveStore,
    root: &Path,
) -> AppResult<(VolumeManifest, VerificationReport)> {
    let manifest = VolumeManifest::from_json(&store.read_text(&root.join(VOLUME_MANIFEST_FILE))?)?;
    let actual = store.hash_file(&root.join(VOLUME_DATA_FILE))?;
    let check = FileCheck {
        kind: ItemKind::Volume,
        name: manifest.volume.clone(),
        file: VOLUME_DATA_FILE.to_string(),
        size_bytes: manifest.size_bytes,
        status: FileStatus::classify(&manifest.sha256, actual.as_ref()),
    };
    Ok((manifest, VerificationReport { files: vec![check] }))
}

#[derive(Debug, Clone)]
pub struct VolumeBackupRequest {
    pub names: Vec<String>,
    pub output_dir: PathBuf,
}

pub struct VolumeBackupService<'a> {
    pub docker: &'a dyn DockerPort,
    pub store: &'a dyn ArchiveStore,
    pub progress: &'a dyn ProgressSink,
    pub clock: &'a dyn Clock,
}

/// One volume of a `backup-volume` run, and where its archive goes.
struct PlannedArchive<'a> {
    volume: &'a str,
    stem: String,
    path: PathBuf,
}

impl VolumeBackupService<'_> {
    pub fn run(&self, request: &VolumeBackupRequest) -> AppResult<VolumeBackupReport> {
        self.docker.engine_info()?;
        let names = unique_in_order(&request.names);
        let existing: HashSet<String> = self
            .docker
            .list_volumes()?
            .into_iter()
            .map(|volume| volume.name)
            .collect();
        if let Some(missing) = names.iter().find(|name| !existing.contains(**name)) {
            return Err(AppError::Conflict(format!("volume not found: {missing}")));
        }
        refuse_case_collisions(&names)?;

        self.store.create_dir_all(&request.output_dir)?;
        let created_at = self.clock.now_utc();
        let stamp = utc_stamp(created_at);
        let plan: Vec<PlannedArchive<'_>> = names
            .iter()
            .map(|&volume| {
                let stem = volume_archive_stem(volume, &stamp);
                let path = request.output_dir.join(format!("{stem}{ARCHIVE_SUFFIX}"));
                PlannedArchive { volume, stem, path }
            })
            .collect();
        if let Some(taken) = plan.iter().find(|planned| self.store.exists(&planned.path)) {
            return Err(AppError::Conflict(format!(
                "{} already exists; choose another output directory",
                taken.path.display()
            )));
        }

        self.docker.ensure_helper_image()?;
        self.progress.start(Operation::Backup, plan.len());
        let mut items = Vec::with_capacity(plan.len());
        for (index, planned) in plan.iter().enumerate() {
            self.progress
                .item_started(ItemKind::Volume, planned.volume, index + 1);
            let outcome = match self.back_up(planned, &request.output_dir, created_at) {
                Ok(stored) => ItemOutcome::Done {
                    size_bytes: stored.size_bytes,
                },
                Err(error) => ItemOutcome::Failed {
                    error: error.to_string(),
                },
            };
            self.progress
                .item_finished(ItemKind::Volume, planned.volume, &outcome);
            let file = (!outcome.is_failure()).then(|| planned.path.display().to_string());
            items.push(ItemResult {
                kind: ItemKind::Volume,
                name: planned.volume.to_string(),
                file,
                outcome,
            });
        }
        self.progress.finish();
        Ok(VolumeBackupReport {
            output_dir: request.output_dir.clone(),
            items,
        })
    }

    /// Exports into a scratch dir beside the result (same filesystem), then packs.
    /// The scratch dir goes on every path.
    fn back_up(
        &self,
        planned: &PlannedArchive<'_>,
        output_dir: &Path,
        created_at: OffsetDateTime,
    ) -> AppResult<StoredFile> {
        let temp = self.store.make_temp_dir(output_dir)?;
        let result = self.export_and_pack(planned, &temp, created_at);
        let _ = self.store.remove_dir_all(&temp);
        result
    }

    fn export_and_pack(
        &self,
        planned: &PlannedArchive<'_>,
        temp: &Path,
        created_at: OffsetDateTime,
    ) -> AppResult<StoredFile> {
        let folder = temp.join(&planned.stem);
        self.store.create_dir_all(&folder)?;
        let stored = self.store.write_item(
            &folder.join(VOLUME_DATA_FILE),
            Compression::None,
            &mut |sink| self.docker.export_volume(planned.volume, sink),
        )?;
        let manifest = VolumeManifest::new(
            planned.volume,
            created_at,
            stored.size_bytes,
            stored.sha256.clone(),
        );
        self.store
            .write_text(&folder.join(VOLUME_MANIFEST_FILE), &manifest.to_json()?)?;
        if let Err(error) = self.store.pack_folder(&folder, &planned.path) {
            // Best effort: a failed tar can leave a truncated archive behind.
            let _ = self.store.remove_file(&planned.path);
            return Err(error);
        }
        Ok(stored)
    }
}

/// Each name once, in the order first given.
fn unique_in_order(names: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    names
        .iter()
        .map(String::as_str)
        .filter(|name| seen.insert(*name))
        .collect()
}

/// Names that differ only in case get one archive on a case-insensitive
/// filesystem (the macOS default): the second would silently replace the first.
fn refuse_case_collisions(names: &[&str]) -> AppResult<()> {
    let mut seen = HashMap::new();
    for &name in names {
        if let Some(earlier) = seen.insert(name.to_ascii_lowercase(), name) {
            return Err(AppError::Conflict(format!(
                "volumes {earlier} and {name} differ only in case, so their archives would \
                 collide on a case-insensitive filesystem; back them up to separate directories"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::fakes::{
        FakeDocker, FixedClock, MemoryArchiveStore, RecordingProgress,
    };
    use crate::domain::manifest::{Sha256Digest, ToolMeta};
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-26 14:15:00 UTC);
    const PGDATA_ARCHIVE: &str = "/backups/pgdata-20260926T141500Z.tar.bz2";
    const CACHE_ARCHIVE: &str = "/backups/cache-20260926T141500Z.tar.bz2";

    fn calls(docker: &FakeDocker) -> Vec<String> {
        docker.calls.borrow().clone()
    }

    /// No `.docker-backup-tmp-*` scratch file or directory is left anywhere.
    fn no_scratch_left(store: &MemoryArchiveStore) -> bool {
        let scratch = |path: &PathBuf| path.to_string_lossy().contains(".docker-backup-tmp");
        !store.files.borrow().keys().any(scratch) && !store.dirs.borrow().iter().any(scratch)
    }

    fn volumes() -> FakeDocker {
        FakeDocker::default()
            .with_volume("pgdata", b"PGDATA")
            .with_volume("cache", b"CACHE")
    }

    fn backup_request(names: &[&str]) -> VolumeBackupRequest {
        VolumeBackupRequest {
            names: names.iter().map(|name| name.to_string()).collect(),
            output_dir: PathBuf::from("/backups"),
        }
    }

    fn back_up(
        docker: &FakeDocker,
        store: &MemoryArchiveStore,
        progress: &RecordingProgress,
        request: &VolumeBackupRequest,
    ) -> AppResult<VolumeBackupReport> {
        VolumeBackupService {
            docker,
            store,
            progress,
            clock: &FixedClock(NOW),
        }
        .run(request)
    }

    /// Unpacks `archive` (the fake store's JSON "tar") into /check/<archive name>.
    fn unpack(store: &MemoryArchiveStore, archive: &str) -> PathBuf {
        let root = Path::new("/check").join(Path::new(archive).file_name().unwrap());
        store.unpack_archive(Path::new(archive), &root).unwrap();
        root
    }

    #[test]
    fn each_volume_gets_its_own_self_describing_archive() {
        let (docker, store, progress) = (
            volumes(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let report = back_up(
            &docker,
            &store,
            &progress,
            &backup_request(&["pgdata", "cache"]),
        )
        .unwrap();

        assert_eq!(report.output_dir, Path::new("/backups"));
        let items: Vec<(&str, Option<&str>, &ItemOutcome)> = report
            .items
            .iter()
            .map(|i| (i.name.as_str(), i.file.as_deref(), &i.outcome))
            .collect();
        assert_eq!(
            items,
            vec![
                (
                    "pgdata",
                    Some(PGDATA_ARCHIVE),
                    &ItemOutcome::Done { size_bytes: 6 }
                ),
                (
                    "cache",
                    Some(CACHE_ARCHIVE),
                    &ItemOutcome::Done { size_bytes: 5 }
                ),
            ]
        );
        assert_eq!(report.exit_code(), 0);

        let root = unpack(&store, PGDATA_ARCHIVE);
        assert_eq!(store.file(root.join("backup.tar")).unwrap(), b"PGDATA");
        let manifest =
            VolumeManifest::from_json(&store.read_text(&root.join("volume.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest,
            VolumeManifest {
                schema_version: 1,
                volume: "pgdata".into(),
                created_at: NOW,
                size_bytes: 6,
                sha256: Sha256Digest::of(b"PGDATA"),
                tool: ToolMeta::current(),
            }
        );

        // One folder inside, named like the archive, as `tar -xjf` shows it.
        let packed = store.packed.borrow().clone();
        assert_eq!(packed[0].0.file_name().unwrap(), "pgdata-20260926T141500Z");
        assert_eq!(packed[0].1, Path::new(PGDATA_ARCHIVE));
        assert!(no_scratch_left(&store));
        let helper_pulls = calls(&docker)
            .iter()
            .filter(|c| *c == "ensure_helper_image")
            .count();
        assert_eq!(helper_pulls, 1);
    }

    #[test]
    fn a_name_given_twice_is_backed_up_once() {
        let (docker, store, progress) = (
            volumes(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let report = back_up(
            &docker,
            &store,
            &progress,
            &backup_request(&["pgdata", "pgdata"]),
        )
        .unwrap();
        assert_eq!(report.items.len(), 1);
        let exports = calls(&docker)
            .iter()
            .filter(|c| *c == "export_volume:pgdata")
            .count();
        assert_eq!(exports, 1);
    }

    #[test]
    fn an_unknown_volume_fails_before_anything_is_written() {
        let (docker, store, progress) = (
            volumes(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let err = back_up(
            &docker,
            &store,
            &progress,
            &backup_request(&["pgdata", "ghost"]),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.to_string(), "volume not found: ghost");
        assert!(store.files.borrow().is_empty());
        assert_eq!(calls(&docker), vec!["engine_info", "list_volumes"]);
    }

    #[test]
    fn an_existing_archive_fails_before_anything_is_written() {
        let (docker, store, progress) = (
            volumes(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        store.put(CACHE_ARCHIVE, b"OLD");
        let err = back_up(
            &docker,
            &store,
            &progress,
            &backup_request(&["pgdata", "cache"]),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(
            err.to_string(),
            format!("{CACHE_ARCHIVE} already exists; choose another output directory")
        );
        assert_eq!(store.file(CACHE_ARCHIVE).unwrap(), b"OLD");
        assert!(!store.exists(Path::new(PGDATA_ARCHIVE)));
        assert!(
            calls(&docker)
                .iter()
                .all(|c| !c.starts_with("export_volume"))
        );
    }

    #[test]
    fn names_that_differ_only_in_case_are_refused_before_anything_is_written() {
        let docker = FakeDocker::default()
            .with_volume("Data", b"A")
            .with_volume("data", b"B");
        let (store, progress) = (MemoryArchiveStore::new(), RecordingProgress::default());
        let err = back_up(
            &docker,
            &store,
            &progress,
            &backup_request(&["Data", "data"]),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("differ only in case"), "{err}");
        assert!(store.files.borrow().is_empty());
        assert!(
            calls(&docker)
                .iter()
                .all(|c| !c.starts_with("export_volume"))
        );
    }

    #[test]
    fn a_failing_volume_is_reported_and_the_next_one_still_runs() {
        let (docker, store, progress) = (
            volumes().failing("pgdata"),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let report = back_up(
            &docker,
            &store,
            &progress,
            &backup_request(&["pgdata", "cache"]),
        )
        .unwrap();
        assert!(matches!(
            report.items[0].outcome,
            ItemOutcome::Failed { .. }
        ));
        assert_eq!(report.items[0].file, None);
        assert_eq!(report.items[1].outcome, ItemOutcome::Done { size_bytes: 5 });
        assert_eq!(report.exit_code(), 1);
        assert!(!store.exists(Path::new(PGDATA_ARCHIVE)));
        assert!(store.exists(Path::new(CACHE_ARCHIVE)));
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn a_failed_pack_removes_the_partial_archive_and_the_scratch_dir() {
        let (docker, store, progress) = (
            volumes(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        store.fail_pack.set(true);
        store.partial_pack.set(true);
        let report = back_up(&docker, &store, &progress, &backup_request(&["pgdata"])).unwrap();
        assert!(matches!(
            &report.items[0].outcome,
            ItemOutcome::Failed { error } if error.contains("fake pack failure")
        ));
        assert_eq!(report.exit_code(), 1);
        assert!(!store.exists(Path::new(PGDATA_ARCHIVE)));
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn an_unreachable_daemon_fails_first() {
        let (docker, store, progress) = (
            FakeDocker::unavailable(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let err = back_up(&docker, &store, &progress, &backup_request(&["pgdata"])).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert_eq!(calls(&docker), vec!["engine_info"]);
        assert!(store.files.borrow().is_empty() && store.dirs.borrow().is_empty());
    }

    #[test]
    fn progress_reports_each_volume_in_the_order_given() {
        let (docker, store, progress) = (
            volumes(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        back_up(
            &docker,
            &store,
            &progress,
            &backup_request(&["pgdata", "cache"]),
        )
        .unwrap();
        assert_eq!(
            *progress.events.borrow(),
            vec![
                "start:BACKUP:2",
                "started:VOLUME:pgdata:1",
                "finished:VOLUME:pgdata:done (6 B)",
                "started:VOLUME:cache:2",
                "finished:VOLUME:cache:done (5 B)",
                "finish",
            ]
        );
    }
}
