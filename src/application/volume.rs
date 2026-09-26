//! Single-volume archives: `backup-volume`, `restore-volume`, and the check that
//! `info` shares with `restore-volume`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use crate::application::ports::{
    ArchiveStore, Clock, ConfirmPort, DockerPort, Operation, ProgressSink, StoredFile,
};
use crate::application::verify::locate_backup;
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::{Compression, MANIFEST_FILE, is_docker_name};
use crate::domain::naming::{ARCHIVE_SUFFIX, is_archive, utc_stamp, volume_archive_stem};
use crate::domain::preview::VolumeOverwritePrompt;
use crate::domain::refs::ItemKind;
use crate::domain::report::{
    ItemOutcome, ItemResult, VolumeBackupReport, VolumeRestoreAction, VolumeRestoreReport,
};
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

#[derive(Debug, Clone)]
pub struct VolumeRestoreRequest {
    pub source: PathBuf,
    /// `--as`: restore into this volume instead of the one the archive names.
    pub target: Option<String>,
    pub overwrite: bool,
}

pub struct VolumeRestoreService<'a> {
    pub docker: &'a dyn DockerPort,
    pub store: &'a dyn ArchiveStore,
    pub progress: &'a dyn ProgressSink,
    pub confirm: &'a dyn ConfirmPort,
}

impl VolumeRestoreService<'_> {
    pub fn run(&self, request: &VolumeRestoreRequest) -> AppResult<VolumeRestoreReport> {
        if !is_archive(&request.source) {
            return Err(AppError::Conflict(format!(
                "{} is not a .tar.bz2 archive",
                request.source.display()
            )));
        }
        if let Some(target) = &request.target
            && !is_docker_name(target)
        {
            return Err(AppError::Conflict(format!(
                "{target:?} is not a valid volume name"
            )));
        }
        let located = locate_backup(self.store, &request.source)?;
        let result = self.run_in(&located.root, request);
        located.cleanup(self.store);
        result
    }

    fn run_in(
        &self,
        root: &Path,
        request: &VolumeRestoreRequest,
    ) -> AppResult<VolumeRestoreReport> {
        let (manifest, verification) = self.read_archive(root, &request.source)?;
        if !verification.is_ok() {
            return Err(AppError::VerificationFailed {
                missing: verification.missing_count(),
                corrupt: verification.corrupt_count(),
            });
        }
        let target = request
            .target
            .clone()
            .unwrap_or_else(|| manifest.volume.clone());

        self.docker.engine_info()?;
        let exists = self
            .docker
            .list_volumes()?
            .iter()
            .any(|volume| volume.name == target);
        if exists && !request.overwrite {
            return Err(AppError::Conflict(format!(
                "volume {target} already exists; pass --overwrite to replace its contents"
            )));
        }
        if exists {
            self.confirm
                .confirm_volume_overwrite(&VolumeOverwritePrompt {
                    target: target.clone(),
                    source: request.source.clone(),
                    created_at: manifest.created_at,
                })?;
        }
        self.docker.ensure_helper_image()?;

        self.progress.start(Operation::Restore, 1);
        self.progress.item_started(ItemKind::Volume, &target, 1);
        let (created, filled) = self.fill(root, &target, exists);
        let outcome = match filled {
            Ok(()) => ItemOutcome::Done {
                size_bytes: manifest.size_bytes,
            },
            Err(error) => ItemOutcome::Failed {
                error: error.to_string(),
            },
        };
        self.progress
            .item_finished(ItemKind::Volume, &target, &outcome);
        self.progress.finish();

        let hint = (created && outcome.is_failure()).then(|| {
            format!(
                "volume {target} was created and may be partially filled; retry with --overwrite"
            )
        });
        Ok(VolumeRestoreReport {
            source: request.source.clone(),
            volume: manifest.volume,
            target,
            created_at: manifest.created_at,
            action: if exists {
                VolumeRestoreAction::Overwrite
            } else {
                VolumeRestoreAction::Create
            },
            outcome,
            hint,
        })
    }

    /// `volume.json` makes this a single-volume archive; `manifest.json` means
    /// the user wants `restore`; anything else is not an archive of ours.
    fn read_archive(
        &self,
        root: &Path,
        source: &Path,
    ) -> AppResult<(VolumeManifest, VerificationReport)> {
        if self.store.exists(&root.join(VOLUME_MANIFEST_FILE)) {
            return verify_volume_archive(self.store, root);
        }
        if self.store.exists(&root.join(MANIFEST_FILE)) {
            return Err(AppError::ManifestInvalid(format!(
                "{} is a full backup; use `docker-backup restore`",
                source.display()
            )));
        }
        Err(AppError::ManifestInvalid(format!(
            "no {VOLUME_MANIFEST_FILE} in {}",
            source.display()
        )))
    }

    /// Creates the volume unless it exists, then streams `backup.tar` into it,
    /// emptying it first when it existed. Also says whether this run created it.
    fn fill(&self, root: &Path, target: &str, exists: bool) -> (bool, AppResult<()>) {
        if !exists && let Err(error) = self.docker.create_volume(target) {
            return (false, Err(error));
        }
        let filled = self
            .store
            .open_item(&root.join(VOLUME_DATA_FILE), Compression::None)
            .and_then(|mut reader| self.docker.import_volume(target, &mut reader, exists));
        (!exists, filled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::fakes::{
        FakeConfirm, FakeDocker, FixedClock, MemoryArchiveStore, RecordingProgress,
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

    fn restore_request(target: Option<&str>, overwrite: bool) -> VolumeRestoreRequest {
        VolumeRestoreRequest {
            source: PathBuf::from(PGDATA_ARCHIVE),
            target: target.map(String::from),
            overwrite,
        }
    }

    fn restore(
        docker: &FakeDocker,
        store: &MemoryArchiveStore,
        progress: &RecordingProgress,
        confirm: &FakeConfirm,
        request: &VolumeRestoreRequest,
    ) -> AppResult<VolumeRestoreReport> {
        VolumeRestoreService {
            docker,
            store,
            progress,
            confirm,
        }
        .run(request)
    }

    /// Writes `files` into /work/pgdata-20260926T141500Z/ and packs that folder
    /// into PGDATA_ARCHIVE, the layout backup-volume writes.
    fn pack_archive(store: &MemoryArchiveStore, files: &[(&str, &[u8])]) {
        let folder = Path::new("/work/pgdata-20260926T141500Z");
        for (name, bytes) in files {
            store.put(folder.join(name), bytes);
        }
        store
            .pack_folder(folder, Path::new(PGDATA_ARCHIVE))
            .unwrap();
        store.remove_dir_all(Path::new("/work")).unwrap();
    }

    fn manifest_for(volume: &str, data: &[u8]) -> String {
        VolumeManifest::new(volume, NOW, data.len() as u64, Sha256Digest::of(data))
            .to_json()
            .unwrap()
    }

    /// An intact archive of the volume `pgdata` holding b"PG".
    fn intact_archive() -> MemoryArchiveStore {
        let store = MemoryArchiveStore::new();
        let json = manifest_for("pgdata", b"PG");
        pack_archive(
            &store,
            &[("volume.json", json.as_bytes()), ("backup.tar", b"PG")],
        );
        store
    }

    /// The docker calls that change something.
    fn docker_writes(docker: &FakeDocker) -> Vec<String> {
        calls(docker)
            .into_iter()
            .filter(|c| c.starts_with("create_volume") || c.starts_with("import_volume"))
            .collect()
    }

    #[test]
    fn a_new_volume_is_created_and_filled_without_a_prompt() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap();
        assert_eq!(
            report,
            VolumeRestoreReport {
                source: PathBuf::from(PGDATA_ARCHIVE),
                volume: "pgdata".into(),
                target: "pgdata".into(),
                created_at: NOW,
                action: VolumeRestoreAction::Create,
                outcome: ItemOutcome::Done { size_bytes: 2 },
                hint: None,
            }
        );
        assert_eq!(report.exit_code(), 0);
        assert_eq!(docker.volumes.borrow()["pgdata"], b"PG");
        assert_eq!(
            docker_writes(&docker),
            vec!["create_volume:pgdata", "import_volume:pgdata:wipe=false"]
        );
        assert!(calls(&docker).contains(&"ensure_helper_image".to_string()));
        assert!(confirm.asked.borrow().is_empty());
        assert_eq!(
            *progress.events.borrow(),
            vec![
                "start:RESTORE:1",
                "started:VOLUME:pgdata:1",
                "finished:VOLUME:pgdata:done (2 B)",
                "finish",
            ]
        );
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn an_existing_volume_without_overwrite_is_a_conflict() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default().with_volume("pgdata", b"OLD"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(
            err.to_string(),
            "volume pgdata already exists; pass --overwrite to replace its contents"
        );
        assert!(docker_writes(&docker).is_empty());
        assert_eq!(docker.volumes.borrow()["pgdata"], b"OLD");
        assert!(confirm.asked.borrow().is_empty());
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn overwrite_asks_then_empties_and_refills_the_volume() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default().with_volume("pgdata", b"OLD"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, true),
        )
        .unwrap();
        assert_eq!(report.action, VolumeRestoreAction::Overwrite);
        assert_eq!(report.outcome, ItemOutcome::Done { size_bytes: 2 });
        assert_eq!(docker.volumes.borrow()["pgdata"], b"PG");
        assert_eq!(
            docker_writes(&docker),
            vec!["import_volume:pgdata:wipe=true"]
        );
        assert_eq!(
            *confirm.volume_prompts.borrow(),
            vec![VolumeOverwritePrompt {
                target: "pgdata".into(),
                source: PathBuf::from(PGDATA_ARCHIVE),
                created_at: NOW,
            }]
        );
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn declining_the_overwrite_aborts_without_writing() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default().with_volume("pgdata", b"OLD"),
            RecordingProgress::default(),
            FakeConfirm::declining_volume_overwrite(),
        );
        let err = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, true),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(matches!(err, AppError::Aborted(_)));
        assert!(docker_writes(&docker).is_empty());
        assert_eq!(docker.volumes.borrow()["pgdata"], b"OLD");
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn as_restores_into_another_volume_and_leaves_the_original_alone() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default().with_volume("pgdata", b"LIVE"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(Some("pgdata2"), false),
        )
        .unwrap();
        assert_eq!(
            (report.volume.as_str(), report.target.as_str()),
            ("pgdata", "pgdata2")
        );
        assert_eq!(report.action, VolumeRestoreAction::Create);
        assert_eq!(docker.volumes.borrow()["pgdata2"], b"PG");
        assert_eq!(docker.volumes.borrow()["pgdata"], b"LIVE");
    }

    #[test]
    fn an_as_name_docker_would_refuse_is_rejected_before_unpacking() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        for bad in ["a/b", "-x", ""] {
            let err = restore(
                &docker,
                &store,
                &progress,
                &confirm,
                &restore_request(Some(bad), false),
            )
            .unwrap_err();
            assert_eq!(err.exit_code(), 2, "{bad:?}");
            assert!(err.to_string().contains("not a valid volume name"), "{err}");
        }
        assert!(store.unpacked.borrow().is_empty());
        assert!(calls(&docker).is_empty());
    }

    #[test]
    fn an_archive_without_volume_json_is_invalid() {
        let store = MemoryArchiveStore::new();
        pack_archive(&store, &[("notes.txt", b"hi")]);
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap_err();
        assert!(
            matches!(&err, AppError::ManifestInvalid(msg) if *msg == format!("no volume.json in {PGDATA_ARCHIVE}")),
            "{err:?}"
        );
        assert!(calls(&docker).is_empty());
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn a_full_backup_points_to_restore() {
        let store = MemoryArchiveStore::new();
        pack_archive(&store, &[("manifest.json", b"{}")]);
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap_err();
        assert!(
            matches!(&err, AppError::ManifestInvalid(msg) if *msg == format!("{PGDATA_ARCHIVE} is a full backup; use `docker-backup restore`")),
            "{err:?}"
        );
        assert!(calls(&docker).is_empty());
    }

    #[test]
    fn a_volume_json_naming_a_volume_docker_would_refuse_is_invalid() {
        let store = MemoryArchiveStore::new();
        let json = manifest_for("/", b"PG");
        pack_archive(
            &store,
            &[("volume.json", json.as_bytes()), ("backup.tar", b"PG")],
        );
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(_)));
        assert!(calls(&docker).is_empty());
    }

    #[test]
    fn a_checksum_mismatch_fails_verification_before_docker_is_touched() {
        let store = MemoryArchiveStore::new();
        let json = manifest_for("pgdata", b"PG");
        pack_archive(
            &store,
            &[
                ("volume.json", json.as_bytes()),
                ("backup.tar", b"TAMPERED"),
            ],
        );
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::VerificationFailed {
                missing: 0,
                corrupt: 1
            }
        ));
        assert!(calls(&docker).is_empty());
        assert!(confirm.asked.borrow().is_empty());
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn a_missing_backup_tar_fails_verification_before_docker_is_touched() {
        let store = MemoryArchiveStore::new();
        let json = manifest_for("pgdata", b"PG");
        pack_archive(&store, &[("volume.json", json.as_bytes())]);
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::VerificationFailed {
                missing: 1,
                corrupt: 0
            }
        ));
        assert!(calls(&docker).is_empty());
    }

    #[test]
    fn a_path_that_is_not_a_tar_bz2_is_refused() {
        let store = MemoryArchiveStore::new();
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let mut request = restore_request(None, false);
        request.source = PathBuf::from("/backups/pgdata-20260926T141500Z");
        let err = restore(&docker, &store, &progress, &confirm, &request).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(
            err.to_string(),
            "/backups/pgdata-20260926T141500Z is not a .tar.bz2 archive"
        );
        assert!(store.unpacked.borrow().is_empty());
    }

    #[test]
    fn a_failed_import_into_a_new_volume_leaves_it_and_hints_at_overwrite() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default().failing("pgdata"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap();
        assert!(matches!(report.outcome, ItemOutcome::Failed { .. }));
        assert_eq!(report.action, VolumeRestoreAction::Create);
        assert_eq!(
            report.hint.as_deref(),
            Some("volume pgdata was created and may be partially filled; retry with --overwrite")
        );
        assert_eq!(report.exit_code(), 1);
        assert!(
            docker.volumes.borrow().contains_key("pgdata"),
            "the tool never removes a volume"
        );
        assert!(no_scratch_left(&store));
    }

    #[test]
    fn a_failed_import_with_overwrite_has_no_hint() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default()
                .with_volume("pgdata", b"OLD")
                .failing("pgdata"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, true),
        )
        .unwrap();
        assert!(report.outcome.is_failure());
        assert_eq!(report.action, VolumeRestoreAction::Overwrite);
        assert_eq!(report.hint, None);
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_failed_create_has_no_hint() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default().failing("create_volume:pgdata"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap();
        assert!(report.outcome.is_failure());
        assert_eq!(report.hint, None);
        assert_eq!(report.exit_code(), 1);
        assert_eq!(docker_writes(&docker), vec!["create_volume:pgdata"]);
    }

    #[test]
    fn overwrite_for_a_volume_that_does_not_exist_creates_it_without_asking() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, true),
        )
        .unwrap();
        assert_eq!(report.action, VolumeRestoreAction::Create);
        assert!(confirm.asked.borrow().is_empty());
        assert_eq!(
            docker_writes(&docker),
            vec!["create_volume:pgdata", "import_volume:pgdata:wipe=false"]
        );
    }

    #[test]
    fn an_unreachable_daemon_is_exit_code_3_after_verification() {
        let store = intact_archive();
        let (docker, progress, confirm) = (
            FakeDocker::unavailable(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = restore(
            &docker,
            &store,
            &progress,
            &confirm,
            &restore_request(None, false),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert_eq!(calls(&docker), vec!["engine_info"]);
        assert!(no_scratch_left(&store));
    }
}
