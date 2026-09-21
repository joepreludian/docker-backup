//! Restore use case: verify, plan against the live daemon, stream each item back.

use std::path::{Path, PathBuf};

use crate::application::ports::{ArchiveStore, ConfirmPort, DockerPort, Operation, ProgressSink};
use crate::application::verify::{locate_backup, read_manifest, verify_files};
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::Compression;
use crate::domain::plan::{RestoreAction, RestorePlan, RestorePolicy};
use crate::domain::refs::ItemKind;
use crate::domain::report::{ItemOutcome, ItemResult, RestoreReport};

#[derive(Debug, Clone)]
pub struct RestoreRequest {
    pub source: PathBuf,
    pub policy: RestorePolicy,
    pub verify: bool,
}

pub struct RestoreService<'a> {
    pub docker: &'a dyn DockerPort,
    pub store: &'a dyn ArchiveStore,
    pub progress: &'a dyn ProgressSink,
    pub confirm: &'a dyn ConfirmPort,
}

impl RestoreService<'_> {
    pub fn run(&self, request: &RestoreRequest) -> AppResult<RestoreReport> {
        let located = locate_backup(self.store, &request.source)?;
        let result = self.run_in(&located.root, request);
        located.cleanup(self.store);
        result
    }

    fn run_in(&self, root: &Path, request: &RestoreRequest) -> AppResult<RestoreReport> {
        let manifest = read_manifest(self.store, root)?;
        if request.verify {
            let verification = verify_files(self.store, root, &manifest)?;
            if !verification.is_ok() {
                return Err(AppError::VerificationFailed {
                    missing: verification.missing_count(),
                    corrupt: verification.corrupt_count(),
                });
            }
        }

        let info = self.docker.engine_info()?;
        info.require_platform()?;
        let target = info.platform();

        let existing: Vec<String> = self
            .docker
            .list_volumes()?
            .into_iter()
            .map(|v| v.name)
            .collect();
        let plan = RestorePlan::build(&manifest, &existing, &request.policy, &target);
        let preview = plan.preview(&manifest, &request.source, &request.policy);

        self.confirm.confirm_restore(&preview)?;
        if !preview.mismatches.is_empty() {
            self.confirm.confirm_arch_mismatch(&preview)?;
        }

        if plan
            .volumes
            .iter()
            .any(|v| v.action == RestoreAction::Restore)
        {
            self.docker.ensure_helper_image()?;
        }

        let compression = manifest.compression;
        let mut items = Vec::with_capacity(plan.len());
        let mut index = 0;
        self.progress.start(Operation::Restore, plan.len());

        for planned in &plan.volumes {
            index += 1;
            let name = planned.entry.name.as_str();
            self.progress.item_started(ItemKind::Volume, name, index);
            let outcome = match planned.action {
                RestoreAction::SkipExisting => ItemOutcome::SkippedExisting,
                RestoreAction::SkipVolatile => ItemOutcome::SkippedVolatile,
                RestoreAction::Restore => self.restore_volume(
                    root,
                    &planned.entry.file,
                    compression,
                    name,
                    planned.exists,
                    request.policy.overwrite,
                ),
                // Volumes carry no platform, so `RestorePlan::build` never assigns them this action.
                RestoreAction::SkipArchMismatch => unreachable!("volumes have no architecture"),
            };
            self.progress
                .item_finished(ItemKind::Volume, name, &outcome);
            items.push(ItemResult {
                kind: ItemKind::Volume,
                name: name.to_string(),
                file: Some(planned.entry.file.clone()),
                outcome,
            });
        }

        for image in &plan.images {
            index += 1;
            self.progress
                .item_started(ItemKind::Image, &image.entry.reference, index);
            let outcome = match image.action {
                RestoreAction::SkipArchMismatch => ItemOutcome::SkippedArchMismatch {
                    platform: image.platform.to_string(),
                    target: target.to_string(),
                },
                _ => to_outcome(
                    self.store
                        .open_item(&root.join(&image.entry.file), compression)
                        .and_then(|mut reader| self.docker.load_image(&mut reader)),
                ),
            };
            self.progress
                .item_finished(ItemKind::Image, &image.entry.reference, &outcome);
            items.push(ItemResult {
                kind: ItemKind::Image,
                name: image.entry.reference.clone(),
                file: Some(image.entry.file.clone()),
                outcome,
            });
        }

        for container in &plan.containers {
            index += 1;
            self.progress
                .item_started(ItemKind::Container, &container.entry.name, index);
            // Docker rejects uppercase in an image name, container names allow it.
            let tag = format!(
                "{}:{}",
                container.entry.name.to_ascii_lowercase(),
                request.policy.container_tag
            );
            let outcome = match container.action {
                RestoreAction::SkipArchMismatch => ItemOutcome::SkippedArchMismatch {
                    platform: container.platform.to_string(),
                    target: target.to_string(),
                },
                _ => to_outcome(
                    self.store
                        .open_item(&root.join(&container.entry.file), compression)
                        .and_then(|mut reader| self.docker.import_container_fs(&mut reader, &tag)),
                ),
            };
            self.progress
                .item_finished(ItemKind::Container, &container.entry.name, &outcome);
            items.push(ItemResult {
                kind: ItemKind::Container,
                name: container.entry.name.clone(),
                file: Some(container.entry.file.clone()),
                outcome,
            });
        }

        self.progress.finish();
        Ok(RestoreReport {
            source: request.source.clone(),
            preview: Some(preview),
            items,
        })
    }

    fn restore_volume(
        &self,
        root: &Path,
        file: &str,
        compression: Compression,
        name: &str,
        exists: bool,
        overwrite: bool,
    ) -> ItemOutcome {
        let result = (|| {
            if !exists {
                self.docker.create_volume(name)?;
            }
            let mut reader = self.store.open_item(&root.join(file), compression)?;
            self.docker
                .import_volume(name, &mut reader, exists && overwrite)
        })();
        to_outcome(result)
    }
}

fn to_outcome(result: AppResult<()>) -> ItemOutcome {
    match result {
        Ok(()) => ItemOutcome::Restored,
        Err(error) => ItemOutcome::Failed {
            error: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::fakes::{
        FakeConfirm, FakeDocker, MemoryArchiveStore, RecordingProgress,
    };
    use crate::domain::manifest::{
        Compression, ContainerEntry, DockerInfo, ImageEntry, MANIFEST_FILE, Manifest, Sha256Digest,
        VolumeEntry,
    };
    use crate::domain::platform::Platform;
    use crate::domain::refs::ImageOrigin;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use time::macros::datetime;

    /// Writes a backup folder at /b with pgdata + cache volumes, one volatile volume, one image, one container.
    fn seed(store: &MemoryArchiveStore) -> Manifest {
        let mut m = Manifest::new(
            datetime!(2026-09-19 00:00:00 UTC),
            DockerInfo {
                os: "linux".into(),
                arch: "arm64".into(),
                ..Default::default()
            },
            Compression::None,
        );
        let anon = "0c".repeat(32);
        for (name, content, volatile) in [
            ("pgdata", b"PG".as_slice(), false),
            ("cache", b"CACHE".as_slice(), false),
            (anon.as_str(), b"ANON".as_slice(), true),
        ] {
            store.put(format!("/b/volumes/{name}.tar"), content);
            m.volumes.push(VolumeEntry {
                name: name.into(),
                file: format!("volumes/{name}.tar"),
                size_bytes: content.len() as u64,
                sha256: Sha256Digest::of(content),
                volatile,
                inspect: json!({}),
            });
        }
        store.put("/b/images/app_latest.tar", b"APPIMAGE");
        m.images.push(ImageEntry {
            reference: "app:latest".into(),
            id: "sha256:1".into(),
            file: "images/app_latest.tar".into(),
            size_bytes: 8,
            sha256: Sha256Digest::of(b"APPIMAGE"),
            origin: ImageOrigin::Built,
            platform: Some(Platform::new("linux", "arm64")),
            inspect: json!({}),
        });
        store.put("/b/containers/web.tar", b"WEBFS");
        m.containers.push(ContainerEntry {
            name: "web".into(),
            id: "c1".into(),
            image: "nginx".into(),
            file: "containers/web.tar".into(),
            size_bytes: 5,
            sha256: Sha256Digest::of(b"WEBFS"),
            platform: None,
            inspect: json!({}),
        });
        store
            .write_text(&Path::new("/b").join(MANIFEST_FILE), &m.to_json().unwrap())
            .unwrap();
        m
    }

    fn request(policy: RestorePolicy) -> RestoreRequest {
        RestoreRequest {
            source: PathBuf::from("/b"),
            policy,
            verify: true,
        }
    }

    fn run(
        docker: &FakeDocker,
        store: &MemoryArchiveStore,
        progress: &RecordingProgress,
        confirm: &FakeConfirm,
        request: &RestoreRequest,
    ) -> AppResult<RestoreReport> {
        RestoreService {
            docker,
            store,
            progress,
            confirm,
        }
        .run(request)
    }

    #[test]
    fn restores_volumes_images_and_containers_into_an_empty_daemon() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap();

        let outcomes: Vec<(ItemKind, &str, &ItemOutcome)> = report
            .items
            .iter()
            .map(|i| (i.kind, i.name.as_str(), &i.outcome))
            .collect();
        assert_eq!(
            outcomes[0],
            (ItemKind::Volume, "pgdata", &ItemOutcome::Restored)
        );
        assert_eq!(
            outcomes[1],
            (ItemKind::Volume, "cache", &ItemOutcome::Restored)
        );
        assert_eq!(outcomes[2].2, &ItemOutcome::SkippedVolatile);
        assert_eq!(
            outcomes[3],
            (ItemKind::Image, "app:latest", &ItemOutcome::Restored)
        );
        assert_eq!(
            outcomes[4],
            (ItemKind::Container, "web", &ItemOutcome::Restored)
        );
        assert_eq!(report.exit_code(), 0);

        assert_eq!(docker.volumes.borrow()["pgdata"], b"PG");
        assert_eq!(docker.loaded_images.borrow()[0], b"APPIMAGE");
        assert_eq!(
            docker.imported_containers.borrow()[0],
            ("web:restored".to_string(), b"WEBFS".to_vec())
        );
        let calls = docker.calls.borrow().clone();
        assert!(calls.contains(&"create_volume:pgdata".to_string()));
        assert!(calls.contains(&"import_volume:pgdata:wipe=false".to_string()));
        assert!(calls.contains(&"ensure_helper_image".to_string()));
        assert_eq!(progress.events.borrow()[0], "start:RESTORE:5");
        assert_eq!(progress.events.borrow().last().unwrap(), "finish");
    }

    #[test]
    fn existing_volume_is_skipped_unless_overwrite() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let (docker, progress, confirm) = (
            FakeDocker::default().with_volume("cache", b"OLD"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap();
        assert_eq!(report.items[1].outcome, ItemOutcome::SkippedExisting);
        assert_eq!(docker.volumes.borrow()["cache"], b"OLD");

        let (docker, progress, confirm) = (
            FakeDocker::default().with_volume("cache", b"OLD"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let policy = RestorePolicy {
            overwrite: true,
            ..RestorePolicy::default()
        };
        let report = run(&docker, &store, &progress, &confirm, &request(policy)).unwrap();
        assert_eq!(report.items[1].outcome, ItemOutcome::Restored);
        assert_eq!(docker.volumes.borrow()["cache"], b"CACHE");
        let calls = docker.calls.borrow().clone();
        assert!(calls.contains(&"import_volume:cache:wipe=true".to_string()));
        assert!(!calls.contains(&"create_volume:cache".to_string()));
    }

    #[test]
    fn include_volatile_restores_anonymous_volumes() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let policy = RestorePolicy {
            include_volatile: true,
            ..RestorePolicy::default()
        };
        let report = run(&docker, &store, &progress, &confirm, &request(policy)).unwrap();
        assert_eq!(report.items[2].outcome, ItemOutcome::Restored);
        assert_eq!(docker.volumes.borrow()[&"0c".repeat(32)], b"ANON");
    }

    #[test]
    fn corrupt_file_aborts_before_touching_docker() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        store.put("/b/volumes/pgdata.tar", b"TAMPERED");
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::VerificationFailed {
                missing: 0,
                corrupt: 1
            }
        ));
        assert!(docker.calls.borrow().is_empty());
        // Verification runs before any confirmation prompt.
        assert!(confirm.asked.borrow().is_empty());
    }

    #[test]
    fn skip_verify_proceeds_despite_corruption() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        store.put("/b/volumes/pgdata.tar", b"TAMPERED");
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let mut req = request(RestorePolicy::default());
        req.verify = false;
        let report = run(&docker, &store, &progress, &confirm, &req).unwrap();
        assert_eq!(report.items[0].outcome, ItemOutcome::Restored);
        assert_eq!(docker.volumes.borrow()["pgdata"], b"TAMPERED");
    }

    #[test]
    fn failed_item_is_reported_and_restore_continues() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let (docker, progress, confirm) = (
            FakeDocker::default().failing("pgdata"),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let report = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap();
        assert!(report.items[0].outcome.is_failure());
        assert_eq!(report.items[1].outcome, ItemOutcome::Restored);
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn container_tag_is_configurable_and_kinds_can_be_disabled() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let policy = RestorePolicy {
            volumes: false,
            images: false,
            container_tag: "from-backup".into(),
            ..RestorePolicy::default()
        };
        let report = run(&docker, &store, &progress, &confirm, &request(policy)).unwrap();
        assert_eq!(report.items.len(), 1);
        assert_eq!(docker.imported_containers.borrow()[0].0, "web:from-backup");
    }

    #[test]
    fn container_tag_is_lowercased_for_docker() {
        let store = MemoryArchiveStore::new();
        let mut manifest = seed(&store);
        manifest.containers[0].name = "WebApp".into();
        store
            .write_text(
                &Path::new("/b").join(MANIFEST_FILE),
                &manifest.to_json().unwrap(),
            )
            .unwrap();
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let policy = RestorePolicy {
            volumes: false,
            images: false,
            ..RestorePolicy::default()
        };
        let report = run(&docker, &store, &progress, &confirm, &request(policy)).unwrap();
        assert_eq!(report.items[0].name, "WebApp");
        assert_eq!(docker.imported_containers.borrow()[0].0, "webapp:restored");
    }

    #[test]
    fn archive_source_is_unpacked_and_cleaned_up() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        store
            .pack_folder(Path::new("/b"), Path::new("/b.tar.bz2"))
            .unwrap();
        let (docker, progress, confirm) = (
            FakeDocker::default(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let mut req = request(RestorePolicy::default());
        req.source = PathBuf::from("/b.tar.bz2");
        let report = run(&docker, &store, &progress, &confirm, &req).unwrap();
        assert_eq!(report.items[0].outcome, ItemOutcome::Restored);
        assert_eq!(store.unpacked.borrow().len(), 1);
        assert!(
            store
                .paths_under(Path::new("/.docker-backup-tmp-1"))
                .is_empty()
        );
    }

    #[test]
    fn docker_unavailable_is_exit_code_3() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let (docker, progress, confirm) = (
            FakeDocker::unavailable(),
            RecordingProgress::default(),
            FakeConfirm::accepting(),
        );
        let err = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.to_string(), "docker is not available: fake daemon down");
    }

    #[test]
    fn declining_the_first_prompt_aborts_before_touching_docker() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let docker = FakeDocker::default();
        let progress = RecordingProgress::default();
        let confirm = FakeConfirm::declining_restore();
        let error = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert!(matches!(error, AppError::Aborted(_)));
        assert!(
            docker
                .calls
                .borrow()
                .iter()
                .all(|c| !c.starts_with("import")
                    && !c.starts_with("load")
                    && !c.starts_with("create"))
        );
        assert_eq!(*confirm.asked.borrow(), vec!["restore".to_string()]);
    }

    #[test]
    fn same_arch_asks_once_and_previews_counts() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let docker = FakeDocker::default();
        let progress = RecordingProgress::default();
        let confirm = FakeConfirm::accepting();
        let report = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap();
        assert_eq!(*confirm.asked.borrow(), vec!["restore".to_string()]);
        let preview = report.preview.as_ref().unwrap();
        assert!(preview.mismatches.is_empty());
        assert_eq!(preview.images_to_load, 1);
        assert_eq!(preview.volumes_to_create, 2);
        assert_eq!(preview.volumes_skipped, 1);
    }

    #[test]
    fn arch_mismatch_asks_twice_and_skips_images_and_containers() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let mut docker = FakeDocker::default();
        docker.info.arch = "amd64".into();
        let progress = RecordingProgress::default();
        let confirm = FakeConfirm::accepting();
        let report = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap();
        assert_eq!(
            *confirm.asked.borrow(),
            vec!["restore".to_string(), "arch".to_string()]
        );
        let image = report
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Image)
            .unwrap();
        assert_eq!(
            image.outcome,
            ItemOutcome::SkippedArchMismatch {
                platform: "linux/arm64".into(),
                target: "linux/amd64".into()
            }
        );
        let container = report
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Container)
            .unwrap();
        assert!(matches!(
            container.outcome,
            ItemOutcome::SkippedArchMismatch { .. }
        ));
        assert!(docker.loaded_images.borrow().is_empty());
        assert!(docker.imported_containers.borrow().is_empty());
        assert_eq!(report.exit_code(), 1);
        // volumes still restored
        assert_eq!(docker.volumes.borrow().get("pgdata").unwrap(), b"PG");
    }

    #[test]
    fn declining_the_arch_prompt_aborts_before_touching_docker() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let mut docker = FakeDocker::default();
        docker.info.arch = "amd64".into();
        let progress = RecordingProgress::default();
        let confirm = FakeConfirm::declining_arch();
        let error = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert!(matches!(error, AppError::Aborted(_)));
        assert!(
            docker
                .calls
                .borrow()
                .iter()
                .all(|c| !c.starts_with("import")
                    && !c.starts_with("load")
                    && !c.starts_with("create"))
        );
        assert_eq!(
            *confirm.asked.borrow(),
            vec!["restore".to_string(), "arch".to_string()]
        );
    }

    #[test]
    fn force_flag_imports_despite_mismatch_but_still_asks_twice() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let mut docker = FakeDocker::default();
        docker.info.arch = "amd64".into();
        let progress = RecordingProgress::default();
        let confirm = FakeConfirm::accepting();
        let policy = RestorePolicy {
            force_arch_mismatch: true,
            ..RestorePolicy::default()
        };
        let report = run(&docker, &store, &progress, &confirm, &request(policy)).unwrap();
        assert_eq!(
            *confirm.asked.borrow(),
            vec!["restore".to_string(), "arch".to_string()]
        );
        let image = report
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Image)
            .unwrap();
        assert_eq!(image.outcome, ItemOutcome::Restored);
        assert_eq!(docker.loaded_images.borrow().len(), 1);
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn legacy_manifest_without_item_platform_uses_backup_daemon_platform() {
        let store = MemoryArchiveStore::new();
        let mut manifest = seed(&store);
        manifest.images[0].platform = None;
        store
            .write_text(
                &Path::new("/b").join(MANIFEST_FILE),
                &manifest.to_json().unwrap(),
            )
            .unwrap();
        let mut docker = FakeDocker::default();
        docker.info.arch = "amd64".into();
        let progress = RecordingProgress::default();
        let confirm = FakeConfirm::accepting();
        let report = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap();
        let image = report
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Image)
            .unwrap();
        assert_eq!(
            image.outcome,
            ItemOutcome::SkippedArchMismatch {
                platform: "linux/arm64".into(),
                target: "linux/amd64".into()
            }
        );
    }

    #[test]
    fn daemon_without_platform_is_refused_before_prompting() {
        let store = MemoryArchiveStore::new();
        seed(&store);
        let mut docker = FakeDocker::default();
        docker.info.os = "".into();
        let progress = RecordingProgress::default();
        let confirm = FakeConfirm::accepting();
        let error = run(
            &docker,
            &store,
            &progress,
            &confirm,
            &request(RestorePolicy::default()),
        )
        .unwrap_err();
        assert_eq!(error.exit_code(), 3);
        assert!(confirm.asked.borrow().is_empty());
    }
}
