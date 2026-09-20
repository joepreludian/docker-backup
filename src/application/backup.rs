//! Backup use case: plan, export each item through the store, write the manifest.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::application::inventory::collect_inventory;
use crate::application::ports::{
    ArchiveStore, Clock, DockerPort, Operation, ProgressSink, StoredFile,
};
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::{
    CONTAINERS_DIR, Compression, ContainerEntry, IMAGES_DIR, ImageEntry, MANIFEST_FILE, Manifest,
    PARTIAL_MANIFEST_FILE, VOLUMES_DIR, VolumeEntry,
};
use crate::domain::naming::{FileNamer, archive_path, image_base_name, sanitize};
use crate::domain::plan::{BackupPlan, BackupScope};
use crate::domain::refs::ItemKind;
use crate::domain::report::{BackupReport, ItemOutcome, ItemResult};

#[derive(Debug, Clone)]
pub struct BackupRequest {
    pub output: PathBuf,
    pub scope: BackupScope,
    pub compression: Compression,
    pub single_archive: bool,
}

pub struct BackupService<'a> {
    pub docker: &'a dyn DockerPort,
    pub store: &'a dyn ArchiveStore,
    pub progress: &'a dyn ProgressSink,
    pub clock: &'a dyn Clock,
}

impl BackupService<'_> {
    pub fn run(&self, request: &BackupRequest) -> AppResult<BackupReport> {
        let docker_info = self
            .docker
            .engine_info()
            .map_err(|e| AppError::DockerUnavailable(e.to_string()))?;
        let inventory = collect_inventory(self.docker)?;
        let plan = BackupPlan::build(&inventory, &request.scope)?;

        let output = request.output.clone();
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let folder_name = output
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "backup".into());
        let temp_root = if request.single_archive {
            Some(self.store.make_temp_dir(&parent)?)
        } else {
            None
        };
        let work_dir = match &temp_root {
            Some(temp) => temp.join(&folder_name),
            None => output.clone(),
        };
        for dir in [VOLUMES_DIR, IMAGES_DIR, CONTAINERS_DIR] {
            self.store.create_dir_all(&work_dir.join(dir))?;
        }
        if !plan.volumes.is_empty() {
            self.docker.ensure_helper_image()?;
        }

        let mut manifest = Manifest::new(
            self.clock.now_utc(),
            docker_info.clone(),
            request.compression,
        );
        let extension = request.compression.extension();
        let mut items = Vec::with_capacity(plan.len());
        let mut index = 0;
        self.progress.start(Operation::Backup, plan.len());

        let mut namer = FileNamer::new();
        for volume in &plan.volumes {
            index += 1;
            let file = format!(
                "{VOLUMES_DIR}/{}",
                namer.unique(&sanitize(&volume.name), extension)
            );
            let result = self.export(
                &work_dir,
                &file,
                request.compression,
                ItemKind::Volume,
                &volume.name,
                index,
                || self.docker.inspect_volume(&volume.name),
                &mut |sink| self.docker.export_volume(&volume.name, sink),
            );
            if let Ok((stored, inspect)) = &result {
                manifest.volumes.push(VolumeEntry {
                    name: volume.name.clone(),
                    file: file.clone(),
                    size_bytes: stored.size_bytes,
                    sha256: stored.sha256.clone(),
                    volatile: volume.volatile,
                    inspect: inspect.clone(),
                });
            }
            items.push(self.finish_item(
                &work_dir,
                &manifest,
                ItemKind::Volume,
                &volume.name,
                file,
                result,
            )?);
        }

        let mut namer = FileNamer::new();
        for image in &plan.images {
            index += 1;
            let reference = image.primary_ref();
            let file = format!(
                "{IMAGES_DIR}/{}",
                namer.unique(&image_base_name(image), extension)
            );
            let result = self.export(
                &work_dir,
                &file,
                request.compression,
                ItemKind::Image,
                &reference,
                index,
                || self.docker.inspect_image(&image.id),
                &mut |sink| self.docker.save_image(&reference, sink),
            );
            if let Ok((stored, inspect)) = &result {
                manifest.images.push(ImageEntry {
                    reference: reference.clone(),
                    id: image.id.clone(),
                    file: file.clone(),
                    size_bytes: stored.size_bytes,
                    sha256: stored.sha256.clone(),
                    origin: image.origin,
                    inspect: inspect.clone(),
                });
            }
            items.push(self.finish_item(
                &work_dir,
                &manifest,
                ItemKind::Image,
                &reference,
                file,
                result,
            )?);
        }

        let mut namer = FileNamer::new();
        for container in &plan.containers {
            index += 1;
            let file = format!(
                "{CONTAINERS_DIR}/{}",
                namer.unique(&sanitize(&container.name), extension)
            );
            let result = self.export(
                &work_dir,
                &file,
                request.compression,
                ItemKind::Container,
                &container.name,
                index,
                || self.docker.inspect_container(&container.name),
                &mut |sink| self.docker.export_container(&container.name, sink),
            );
            if let Ok((stored, inspect)) = &result {
                manifest.containers.push(ContainerEntry {
                    name: container.name.clone(),
                    id: container.id.clone(),
                    image: container.image.clone(),
                    file: file.clone(),
                    size_bytes: stored.size_bytes,
                    sha256: stored.sha256.clone(),
                    inspect: inspect.clone(),
                });
            }
            items.push(self.finish_item(
                &work_dir,
                &manifest,
                ItemKind::Container,
                &container.name,
                file,
                result,
            )?);
        }

        self.store
            .write_text(&work_dir.join(MANIFEST_FILE), &manifest.to_json()?)?;
        let partial = work_dir.join(PARTIAL_MANIFEST_FILE);
        if self.store.exists(&partial) {
            self.store.remove_file(&partial)?;
        }

        let final_output = match temp_root {
            Some(temp) => {
                let archive = archive_path(&output);
                if let Err(error) = self.store.pack_folder(&work_dir, &archive) {
                    let _ = self.store.remove_dir_all(&temp);
                    return Err(error);
                }
                self.store.remove_dir_all(&temp)?;
                archive
            }
            None => output,
        };
        self.progress.finish();

        Ok(BackupReport {
            output: final_output,
            single_archive: request.single_archive,
            compression: request.compression,
            docker: docker_info,
            items,
        })
    }

    /// Inspect, then stream the export into the store. Any error becomes the item's failure.
    #[allow(clippy::too_many_arguments)]
    fn export(
        &self,
        work_dir: &Path,
        file: &str,
        compression: Compression,
        kind: ItemKind,
        name: &str,
        index: usize,
        inspect: impl FnOnce() -> AppResult<Value>,
        producer: &mut dyn FnMut(&mut dyn Write) -> AppResult<()>,
    ) -> AppResult<(StoredFile, Value)> {
        self.progress.item_started(kind, name, index);
        let inspect = inspect()?;
        let stored = self
            .store
            .write_item(&work_dir.join(file), compression, producer)?;
        Ok((stored, inspect))
    }

    /// Persist the partial manifest, report progress, and turn the result into an `ItemResult`.
    fn finish_item(
        &self,
        work_dir: &Path,
        manifest: &Manifest,
        kind: ItemKind,
        name: &str,
        file: String,
        result: AppResult<(StoredFile, Value)>,
    ) -> AppResult<ItemResult> {
        let outcome = match result {
            Ok((stored, _)) => ItemOutcome::Done {
                size_bytes: stored.size_bytes,
            },
            Err(error) => ItemOutcome::Failed {
                error: error.to_string(),
            },
        };
        self.store
            .write_text(&work_dir.join(PARTIAL_MANIFEST_FILE), &manifest.to_json()?)?;
        self.progress.item_finished(kind, name, &outcome);
        let file = if outcome.is_failure() {
            None
        } else {
            Some(file)
        };
        Ok(ItemResult {
            kind,
            name: name.to_string(),
            file,
            outcome,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::fakes::{
        FakeDocker, FixedClock, MemoryArchiveStore, RecordingProgress,
    };
    use crate::domain::manifest::{MANIFEST_FILE, Manifest, PARTIAL_MANIFEST_FILE, Sha256Digest};
    use crate::domain::refs::ImageOrigin;
    use std::path::{Path, PathBuf};
    use time::macros::datetime;

    fn docker() -> FakeDocker {
        FakeDocker::default()
            .with_volume("pgdata", b"PGDATA")
            .with_volatile_volume(&"0c".repeat(32), b"ANON")
            .with_image("app:latest", ImageOrigin::Built, b"APPIMAGE")
            .with_image("nginx:alpine", ImageOrigin::Pulled, b"NGINX")
            .with_container("web", "nginx:alpine", b"WEBFS")
    }

    fn request(scope: BackupScope) -> BackupRequest {
        BackupRequest {
            output: PathBuf::from("/backups/out"),
            scope,
            compression: Compression::None,
            single_archive: false,
        }
    }

    fn run(
        docker: &FakeDocker,
        store: &MemoryArchiveStore,
        progress: &RecordingProgress,
        request: &BackupRequest,
    ) -> AppResult<BackupReport> {
        let clock = FixedClock(datetime!(2026-09-19 14:03:11 UTC));
        BackupService {
            docker,
            store,
            progress,
            clock: &clock,
        }
        .run(request)
    }

    fn manifest(store: &MemoryArchiveStore, root: &str) -> Manifest {
        Manifest::from_json(
            &store
                .read_text(&Path::new(root).join(MANIFEST_FILE))
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn default_scope_backs_up_named_volumes_and_built_images() {
        let (docker, store, progress) = (
            docker(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let report = run(&docker, &store, &progress, &request(BackupScope::default())).unwrap();

        let summary: Vec<(ItemKind, &str, bool)> = report
            .items
            .iter()
            .map(|i| (i.kind, i.name.as_str(), i.outcome.is_failure()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (ItemKind::Volume, "pgdata", false),
                (ItemKind::Image, "app:latest", false)
            ]
        );
        assert_eq!(report.output, Path::new("/backups/out"));
        assert_eq!(report.exit_code(), 0);

        assert_eq!(
            store.file("/backups/out/volumes/pgdata.tar").unwrap(),
            b"PGDATA"
        );
        assert_eq!(
            store.file("/backups/out/images/app_latest.tar").unwrap(),
            b"APPIMAGE"
        );

        let manifest = manifest(&store, "/backups/out");
        assert_eq!(manifest.created_at, datetime!(2026-09-19 14:03:11 UTC));
        assert_eq!(manifest.docker.server_version, "29.5.2");
        assert_eq!(manifest.volumes.len(), 1);
        assert_eq!(manifest.volumes[0].file, "volumes/pgdata.tar");
        assert_eq!(manifest.volumes[0].sha256, Sha256Digest::of(b"PGDATA"));
        assert_eq!(manifest.volumes[0].size_bytes, 6);
        assert_eq!(manifest.volumes[0].inspect["Name"], "pgdata");
        assert_eq!(manifest.images[0].reference, "app:latest");
        assert_eq!(manifest.images[0].origin, ImageOrigin::Built);
        assert!(manifest.containers.is_empty());
        assert!(!store.exists(&Path::new("/backups/out").join(PARTIAL_MANIFEST_FILE)));
        assert!(
            docker
                .calls
                .borrow()
                .contains(&"ensure_helper_image".to_string())
        );
    }

    #[test]
    fn widened_scope_includes_volatile_all_images_and_containers() {
        let (docker, store, progress) = (
            docker(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let scope = BackupScope {
            include_volatile: true,
            all_images: true,
            containers: vec!["web".into()],
            ..BackupScope::default()
        };
        let report = run(&docker, &store, &progress, &request(scope)).unwrap();
        assert_eq!(report.items.len(), 5);
        let manifest = manifest(&store, "/backups/out");
        assert_eq!(manifest.volumes.len(), 2);
        assert!(manifest.volumes.iter().any(|v| v.volatile));
        assert_eq!(manifest.images.len(), 2);
        assert_eq!(manifest.containers[0].name, "web");
        assert_eq!(manifest.containers[0].image, "nginx:alpine");
        assert_eq!(
            store.file("/backups/out/containers/web.tar").unwrap(),
            b"WEBFS"
        );
    }

    #[test]
    fn a_failing_item_is_recorded_and_the_backup_continues() {
        let (docker, store, progress) = (
            docker().failing("pgdata"),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let report = run(&docker, &store, &progress, &request(BackupScope::default())).unwrap();
        assert!(matches!(
            report.items[0].outcome,
            ItemOutcome::Failed { .. }
        ));
        assert!(!report.items[1].outcome.is_failure());
        assert_eq!(report.failed_count(), 1);
        assert_eq!(report.exit_code(), 1);
        let manifest = manifest(&store, "/backups/out");
        assert!(manifest.volumes.is_empty());
        assert_eq!(manifest.images.len(), 1);
        assert!(!store.exists(Path::new("/backups/out/volumes/pgdata.tar")));
    }

    #[test]
    fn docker_unavailable_aborts_with_exit_code_3() {
        let (docker, store, progress) = (
            FakeDocker::unavailable(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let err = run(&docker, &store, &progress, &request(BackupScope::default())).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(store.files.borrow().is_empty());
    }

    #[test]
    fn progress_events_are_emitted_in_order() {
        let (docker, store, progress) = (
            docker(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        run(&docker, &store, &progress, &request(BackupScope::default())).unwrap();
        let events = progress.events.borrow().clone();
        assert_eq!(events[0], "start:BACKUP:2");
        assert_eq!(events[1], "started:VOLUME:pgdata:1");
        assert_eq!(events[2], "finished:VOLUME:pgdata:done (6 B)");
        assert_eq!(events[3], "started:IMAGE:app:latest:2");
        assert_eq!(events[4], "finished:IMAGE:app:latest:done (8 B)");
        assert_eq!(events[5], "finish");
    }

    #[test]
    fn per_file_bzip2_changes_extension_and_manifest_compression() {
        let (docker, store, progress) = (
            docker(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let mut req = request(BackupScope::default());
        req.compression = Compression::Bzip2PerFile;
        run(&docker, &store, &progress, &req).unwrap();
        assert!(store.exists(Path::new("/backups/out/volumes/pgdata.tar.bz2")));
        let manifest = manifest(&store, "/backups/out");
        assert_eq!(manifest.compression, Compression::Bzip2PerFile);
        assert_eq!(manifest.volumes[0].file, "volumes/pgdata.tar.bz2");
    }

    #[test]
    fn single_archive_packs_the_folder_and_removes_the_work_dir() {
        let (docker, store, progress) = (
            docker(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let mut req = request(BackupScope::default());
        req.single_archive = true;
        let report = run(&docker, &store, &progress, &req).unwrap();
        assert_eq!(report.output, Path::new("/backups/out.tar.bz2"));
        assert!(report.single_archive);
        assert!(store.exists(Path::new("/backups/out.tar.bz2")));
        let packed = store.packed.borrow().clone();
        assert_eq!(packed.len(), 1);
        assert_eq!(packed[0].0.file_name().unwrap(), "out");
        assert!(packed[0].0.starts_with("/backups/.docker-backup-tmp-1"));
        assert!(
            store
                .paths_under(Path::new("/backups/.docker-backup-tmp-1"))
                .is_empty()
        );
        assert!(!store.exists(Path::new("/backups/out")));
    }

    #[test]
    fn pack_failure_removes_the_temp_dir_and_returns_the_error() {
        let (docker, store, progress) = (
            docker(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        store.fail_pack.set(true);
        let mut req = request(BackupScope::default());
        req.single_archive = true;
        let err = run(&docker, &store, &progress, &req).unwrap_err();
        assert!(matches!(err, AppError::ToolFailed { .. }));
        assert!(
            store
                .paths_under(Path::new("/backups/.docker-backup-tmp-1"))
                .is_empty()
        );
        assert!(!store.exists(Path::new("/backups/out.tar.bz2")));
    }

    #[test]
    fn unknown_container_is_reported_as_conflict() {
        let (docker, store, progress) = (
            docker(),
            MemoryArchiveStore::new(),
            RecordingProgress::default(),
        );
        let scope = BackupScope {
            containers: vec!["ghost".into()],
            ..BackupScope::default()
        };
        let err = run(&docker, &store, &progress, &request(scope)).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }
}
