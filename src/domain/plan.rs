//! Pure planning: which items a backup or restore will touch, and how.

use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::{ContainerEntry, ImageEntry, Manifest, VolumeEntry};
use crate::domain::refs::{ContainerRef, ImageOrigin, ImageRef, VolumeRef};

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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAction {
    Restore,
    SkipExisting,
    SkipVolatile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedVolume {
    pub entry: VolumeEntry,
    pub action: RestoreAction,
    pub exists: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestorePlan {
    pub volumes: Vec<PlannedVolume>,
    pub images: Vec<ImageEntry>,
    pub containers: Vec<ContainerEntry>,
}

impl RestorePlan {
    pub fn build(manifest: &Manifest, existing_volumes: &[String], policy: &RestorePolicy) -> Self {
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
            manifest.images.clone()
        } else {
            Vec::new()
        };
        let containers = if policy.containers {
            manifest.containers.clone()
        } else {
            Vec::new()
        };
        Self {
            volumes,
            images,
            containers,
        }
    }

    pub fn len(&self) -> usize {
        self.volumes.len() + self.images.len() + self.containers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
            DockerInfo::default(),
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
            inspect: json!({}),
        });
        m.containers.push(ContainerEntry {
            name: "web".into(),
            id: "c1".into(),
            image: "nginx".into(),
            file: "containers/web.tar".into(),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"z"),
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
        let plan = RestorePlan::build(&manifest(), &["cache".to_string()], &policy);
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
        let plan = RestorePlan::build(&manifest(), &[], &policy);
        assert_eq!(plan.len(), 0);
    }
}
