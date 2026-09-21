//! Pure planning: which items a backup or restore will touch, and how.

use std::path::Path;

use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::{ContainerEntry, ImageEntry, Manifest, VolumeEntry};
use crate::domain::platform::Platform;
use crate::domain::preview::{MismatchedItem, RestorePreview};
use crate::domain::refs::{ContainerRef, ImageOrigin, ImageRef, ItemKind, VolumeRef};

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub volumes: Vec<VolumeRef>,
    pub images: Vec<ImageRef>,
    pub containers: Vec<ContainerRef>,
}

#[derive(Debug, Clone)]
pub struct BackupScope {
    pub volumes: bool,
    pub include_volatile: bool,
    pub images: bool,
    pub all_images: bool,
    pub containers: Vec<String>,
}

impl Default for BackupScope {
    fn default() -> Self {
        Self {
            volumes: true,
            include_volatile: false,
            images: true,
            all_images: false,
            containers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupPlan {
    pub volumes: Vec<VolumeRef>,
    pub images: Vec<ImageRef>,
    pub containers: Vec<ContainerRef>,
}

impl BackupPlan {
    pub fn build(inventory: &Inventory, scope: &BackupScope) -> AppResult<Self> {
        let mut volumes: Vec<VolumeRef> = if scope.volumes {
            inventory
                .volumes
                .iter()
                .filter(|v| scope.include_volatile || !v.volatile)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        volumes.sort_by(|a, b| a.name.cmp(&b.name));

        let mut images: Vec<ImageRef> = if scope.images {
            inventory
                .images
                .iter()
                .filter(|i| scope.all_images || i.origin == ImageOrigin::Built)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        images.sort_by_key(ImageRef::primary_ref);

        let mut containers = Vec::with_capacity(scope.containers.len());
        for wanted in &scope.containers {
            let found = inventory
                .containers
                .iter()
                .find(|c| &c.name == wanted || &c.id == wanted)
                .ok_or_else(|| AppError::Conflict(format!("container not found: {wanted}")))?;
            containers.push(found.clone());
        }
        containers.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Self {
            volumes,
            images,
            containers,
        })
    }

    pub fn len(&self) -> usize {
        self.volumes.len() + self.images.len() + self.containers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone)]
pub struct RestorePolicy {
    pub overwrite: bool,
    pub include_volatile: bool,
    pub volumes: bool,
    pub images: bool,
    pub containers: bool,
    pub container_tag: String,
    pub force_arch_mismatch: bool,
}

impl Default for RestorePolicy {
    fn default() -> Self {
        Self {
            overwrite: false,
            include_volatile: false,
            volumes: true,
            images: true,
            containers: true,
            container_tag: "restored".to_string(),
            force_arch_mismatch: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAction {
    Restore,
    SkipExisting,
    SkipVolatile,
    SkipArchMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedVolume {
    pub entry: VolumeEntry,
    pub action: RestoreAction,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedImage {
    pub entry: ImageEntry,
    pub platform: Platform,
    pub action: RestoreAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedContainer {
    pub entry: ContainerEntry,
    pub platform: Platform,
    pub action: RestoreAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestorePlan {
    pub volumes: Vec<PlannedVolume>,
    pub images: Vec<PlannedImage>,
    pub containers: Vec<PlannedContainer>,
    pub target: Platform,
}

impl RestorePlan {
    pub fn build(
        manifest: &Manifest,
        existing_volumes: &[String],
        policy: &RestorePolicy,
        target: &Platform,
    ) -> Self {
        let volumes = if policy.volumes {
            manifest
                .volumes
                .iter()
                .map(|entry| {
                    let exists = existing_volumes.iter().any(|name| name == &entry.name);
                    let action = if entry.volatile && !policy.include_volatile {
                        RestoreAction::SkipVolatile
                    } else if exists && !policy.overwrite {
                        RestoreAction::SkipExisting
                    } else {
                        RestoreAction::Restore
                    };
                    PlannedVolume {
                        entry: entry.clone(),
                        action,
                        exists,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let images = if policy.images {
            manifest
                .images
                .iter()
                .map(|entry| {
                    let platform = manifest.image_platform(entry);
                    let action = arch_action(&platform, target, policy);
                    PlannedImage {
                        entry: entry.clone(),
                        platform,
                        action,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let containers = if policy.containers {
            manifest
                .containers
                .iter()
                .map(|entry| {
                    let platform = manifest.container_platform(entry);
                    let action = arch_action(&platform, target, policy);
                    PlannedContainer {
                        entry: entry.clone(),
                        platform,
                        action,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            volumes,
            images,
            containers,
            target: target.clone(),
        }
    }

    /// A dry-run summary of this plan: counts, and the list of arch mismatches.
    pub fn preview(
        &self,
        manifest: &Manifest,
        source: &Path,
        policy: &RestorePolicy,
    ) -> RestorePreview {
        let volumes_to_create = self
            .volumes
            .iter()
            .filter(|v| v.action == RestoreAction::Restore && !v.exists)
            .count();
        let volumes_to_overwrite = self
            .volumes
            .iter()
            .filter(|v| v.action == RestoreAction::Restore && v.exists)
            .count();
        let volumes_skipped = self
            .volumes
            .iter()
            .filter(|v| {
                matches!(
                    v.action,
                    RestoreAction::SkipExisting | RestoreAction::SkipVolatile
                )
            })
            .count();
        let images_to_load = self
            .images
            .iter()
            .filter(|i| i.action == RestoreAction::Restore)
            .count();
        let containers_to_import = self
            .containers
            .iter()
            .filter(|c| c.action == RestoreAction::Restore)
            .count();

        // An unknown platform can't prove a mismatch, so it's never listed as one
        // (kept in sync with `arch_action` below).
        let mut mismatches = Vec::new();
        for image in &self.images {
            if image.platform.is_known() && !image.platform.matches(&self.target) {
                mismatches.push(MismatchedItem {
                    kind: ItemKind::Image,
                    name: image.entry.reference.clone(),
                    platform: image.platform.clone(),
                });
            }
        }
        for container in &self.containers {
            if container.platform.is_known() && !container.platform.matches(&self.target) {
                mismatches.push(MismatchedItem {
                    kind: ItemKind::Container,
                    name: container.entry.name.clone(),
                    platform: container.platform.clone(),
                });
            }
        }

        RestorePreview {
            source: source.to_path_buf(),
            created_at: manifest.created_at,
            backup_platform: manifest.docker.platform(),
            target_platform: self.target.clone(),
            overwrite: policy.overwrite,
            force_arch_mismatch: policy.force_arch_mismatch,
            volumes_to_create,
            volumes_to_overwrite,
            volumes_skipped,
            images_to_load,
            containers_to_import,
            mismatches,
        }
    }

    pub fn len(&self) -> usize {
        self.volumes.len() + self.images.len() + self.containers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `Restore` when the item's platform matches the target, when the platform is
/// unknown (an unknown platform can't prove a mismatch — 0.1.0 manifests never
/// recorded one), or when mismatches are forced through anyway.
/// `SkipArchMismatch` otherwise.
fn arch_action(platform: &Platform, target: &Platform, policy: &RestorePolicy) -> RestoreAction {
    if !platform.is_known() || platform.matches(target) || policy.force_arch_mismatch {
        RestoreAction::Restore
    } else {
        RestoreAction::SkipArchMismatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{Compression, DockerInfo, Sha256Digest};
    use serde_json::json;
    use time::macros::datetime;

    fn inventory() -> Inventory {
        Inventory {
            volumes: vec![
                VolumeRef::new("zeta", ""),
                VolumeRef::new("0c".repeat(32), ""),
                VolumeRef::new("alpha", ""),
            ],
            images: vec![
                ImageRef {
                    id: "sha256:1".into(),
                    tags: vec!["nginx:alpine".into()],
                    origin: ImageOrigin::Pulled,
                },
                ImageRef {
                    id: "sha256:2".into(),
                    tags: vec!["app:latest".into()],
                    origin: ImageOrigin::Built,
                },
            ],
            containers: vec![
                ContainerRef {
                    id: "c1".into(),
                    name: "web".into(),
                    image: "nginx:alpine".into(),
                    running: true,
                },
                ContainerRef {
                    id: "c2".into(),
                    name: "db".into(),
                    image: "postgres:17".into(),
                    running: false,
                },
            ],
        }
    }

    #[test]
    fn default_scope_takes_named_volumes_and_built_images_only() {
        let plan = BackupPlan::build(&inventory(), &BackupScope::default()).unwrap();
        let names: Vec<&str> = plan.volumes.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        assert_eq!(plan.images.len(), 1);
        assert_eq!(plan.images[0].primary_ref(), "app:latest");
        assert!(plan.containers.is_empty());
        assert_eq!(plan.len(), 3);
    }

    #[test]
    fn flags_widen_the_scope() {
        let scope = BackupScope {
            include_volatile: true,
            all_images: true,
            containers: vec!["web".into(), "db".into()],
            ..BackupScope::default()
        };
        let plan = BackupPlan::build(&inventory(), &scope).unwrap();
        assert_eq!(plan.volumes.len(), 3);
        assert_eq!(plan.images.len(), 2);
        let containers: Vec<&str> = plan.containers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(containers, vec!["db", "web"]);
    }

    #[test]
    fn no_flags_narrow_the_scope() {
        let scope = BackupScope {
            volumes: false,
            images: false,
            ..BackupScope::default()
        };
        let plan = BackupPlan::build(&inventory(), &scope).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn unknown_container_is_a_conflict() {
        let scope = BackupScope {
            containers: vec!["nope".into()],
            ..BackupScope::default()
        };
        let err = BackupPlan::build(&inventory(), &scope).unwrap_err();
        assert!(matches!(err, AppError::Conflict(msg) if msg.contains("nope")));
    }

    #[test]
    fn container_can_be_selected_by_id() {
        let scope = BackupScope {
            containers: vec!["c2".into()],
            ..BackupScope::default()
        };
        let plan = BackupPlan::build(&inventory(), &scope).unwrap();
        assert_eq!(plan.containers[0].name, "db");
    }

    fn manifest() -> Manifest {
        let mut m = Manifest::new(
            datetime!(2026-09-19 00:00:00 UTC),
            DockerInfo {
                os: "linux".into(),
                arch: "amd64".into(),
                ..DockerInfo::default()
            },
            Compression::None,
        );
        for (name, volatile) in [
            ("pgdata", false),
            ("cache", false),
            ("0c".repeat(32).as_str(), true),
        ] {
            m.volumes.push(VolumeEntry {
                name: name.into(),
                file: format!("volumes/{name}.tar"),
                size_bytes: 1,
                sha256: Sha256Digest::of(b"x"),
                volatile,
                inspect: json!({}),
            });
        }
        m.images.push(ImageEntry {
            reference: "app:latest".into(),
            id: "sha256:2".into(),
            file: "images/app_latest.tar".into(),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"y"),
            origin: ImageOrigin::Built,
            platform: Some(Platform::new("linux", "amd64")),
            inspect: json!({}),
        });
        m.containers.push(ContainerEntry {
            name: "web".into(),
            id: "c1".into(),
            image: "nginx".into(),
            file: "containers/web.tar".into(),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"z"),
            platform: None,
            inspect: json!({}),
        });
        m
    }

    #[test]
    fn default_policy_skips_existing_and_volatile() {
        let plan = RestorePlan::build(
            &manifest(),
            &["cache".to_string()],
            &RestorePolicy::default(),
            &Platform::new("linux", "amd64"),
        );
        let actions: Vec<(&str, RestoreAction, bool)> = plan
            .volumes
            .iter()
            .map(|v| (v.entry.name.as_str(), v.action, v.exists))
            .collect();
        assert_eq!(actions[0], ("pgdata", RestoreAction::Restore, false));
        assert_eq!(actions[1], ("cache", RestoreAction::SkipExisting, true));
        assert_eq!(actions[2].1, RestoreAction::SkipVolatile);
        assert_eq!(plan.images.len(), 1);
        assert_eq!(plan.containers.len(), 1);
        assert_eq!(plan.len(), 5);
    }

    #[test]
    fn overwrite_and_include_volatile_restore_everything() {
        let policy = RestorePolicy {
            overwrite: true,
            include_volatile: true,
            ..RestorePolicy::default()
        };
        let plan = RestorePlan::build(
            &manifest(),
            &["cache".to_string()],
            &policy,
            &Platform::new("linux", "amd64"),
        );
        assert!(
            plan.volumes
                .iter()
                .all(|v| v.action == RestoreAction::Restore)
        );
        assert!(plan.volumes[1].exists);
    }

    #[test]
    fn kinds_can_be_switched_off() {
        let policy = RestorePolicy {
            volumes: false,
            images: false,
            containers: false,
            ..RestorePolicy::default()
        };
        let plan = RestorePlan::build(&manifest(), &[], &policy, &Platform::new("linux", "amd64"));
        assert_eq!(plan.len(), 0);
    }

    #[test]
    fn images_on_another_arch_are_skipped_unless_forced() {
        let m = manifest();
        let arm = Platform::new("linux", "arm64");
        let plan = RestorePlan::build(&m, &[], &RestorePolicy::default(), &arm);
        assert_eq!(plan.images[0].action, RestoreAction::SkipArchMismatch);
        assert_eq!(
            plan.containers[0].action,
            RestoreAction::SkipArchMismatch,
            "container falls back to manifest docker arch"
        );

        let forced = RestorePolicy {
            force_arch_mismatch: true,
            ..RestorePolicy::default()
        };
        let plan = RestorePlan::build(&m, &[], &forced, &arm);
        assert_eq!(plan.images[0].action, RestoreAction::Restore);
        assert_eq!(plan.containers[0].action, RestoreAction::Restore);
    }

    #[test]
    fn unknown_source_platform_is_restorable_and_not_a_mismatch() {
        // A 0.1.0-era manifest: no docker.os/arch recorded, entries have no platform.
        let mut m = Manifest::new(
            datetime!(2026-09-19 00:00:00 UTC),
            DockerInfo::default(),
            Compression::None,
        );
        m.images.push(ImageEntry {
            reference: "app:latest".into(),
            id: "sha256:2".into(),
            file: "images/app_latest.tar".into(),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"y"),
            origin: ImageOrigin::Built,
            platform: None,
            inspect: json!({}),
        });
        m.containers.push(ContainerEntry {
            name: "web".into(),
            id: "c1".into(),
            image: "nginx".into(),
            file: "containers/web.tar".into(),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"z"),
            platform: None,
            inspect: json!({}),
        });

        let target = Platform::new("linux", "arm64");
        let policy = RestorePolicy::default();
        let plan = RestorePlan::build(&m, &[], &policy, &target);
        assert_eq!(
            plan.images[0].action,
            RestoreAction::Restore,
            "an unknown platform can't prove a mismatch"
        );
        assert_eq!(plan.containers[0].action, RestoreAction::Restore);

        let preview = plan.preview(&m, Path::new("/b"), &policy);
        assert!(
            preview.mismatches.is_empty(),
            "unknown-platform items must not be listed as mismatches"
        );
    }

    #[test]
    fn same_arch_restores_everything() {
        let m = manifest();
        let plan = RestorePlan::build(
            &m,
            &[],
            &RestorePolicy::default(),
            &Platform::new("linux", "amd64"),
        );
        assert!(
            plan.images
                .iter()
                .all(|i| i.action == RestoreAction::Restore)
        );
        assert!(
            plan.containers
                .iter()
                .all(|c| c.action == RestoreAction::Restore)
        );
    }

    #[test]
    fn preview_counts_and_lists_mismatches() {
        let m = manifest();
        let arm = Platform::new("linux", "arm64");
        let policy = RestorePolicy {
            overwrite: true,
            ..RestorePolicy::default()
        };
        let plan = RestorePlan::build(&m, &["pgdata".into()], &policy, &arm);
        let preview = plan.preview(&m, Path::new("/b"), &policy);
        assert_eq!(preview.backup_platform, Platform::new("linux", "amd64"));
        assert_eq!(preview.target_platform, arm);
        assert!(preview.overwrite);
        assert_eq!(preview.volumes_to_overwrite, 1);
        assert_eq!(preview.images_to_load, 0);
        assert_eq!(preview.containers_to_import, 0);
        assert_eq!(preview.mismatches.len(), 2);
        assert_eq!(preview.mismatches[0].kind, ItemKind::Image);
        assert_eq!(preview.skipped_for_arch(), 2);

        let forced = RestorePolicy {
            force_arch_mismatch: true,
            ..policy
        };
        let plan = RestorePlan::build(&m, &[], &forced, &arm);
        let preview = plan.preview(&m, Path::new("/b"), &forced);
        assert_eq!(preview.images_to_load, 1);
        assert_eq!(
            preview.mismatches.len(),
            2,
            "mismatches are still listed when forced"
        );
        assert_eq!(preview.skipped_for_arch(), 0);
    }
}
