//! Pure planning: which items a backup or restore will touch, and how.

use std::collections::BTreeMap;
use std::path::Path;

use crate::domain::compose::{ComposeInfo, ComposeProject, ImageCompose, VolumeCompose};
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
    /// Limits the backup to one compose project (`--from-docker-compose`).
    pub compose: Option<ComposeScope>,
}

/// The compose project a backup is limited to.
#[derive(Debug, Clone)]
pub struct ComposeScope {
    pub project: ComposeProject,
    /// The files as typed, recorded in the manifest.
    pub files: Vec<String>,
    pub include_external: bool,
}

impl Default for BackupScope {
    fn default() -> Self {
        Self {
            volumes: true,
            include_volatile: false,
            images: true,
            all_images: false,
            containers: Vec::new(),
            compose: None,
        }
    }
}

/// What a compose-scoped plan knows beyond the items themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposePlan {
    pub info: ComposeInfo,
    /// By volume name.
    pub volumes: BTreeMap<String, VolumeCompose>,
    /// By image id.
    pub images: BTreeMap<String, ImageCompose>,
    /// Declared by the project but absent on the daemon: volume names and image refs.
    pub not_found: Vec<(ItemKind, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupPlan {
    pub volumes: Vec<VolumeRef>,
    pub images: Vec<ImageRef>,
    pub containers: Vec<ContainerRef>,
    /// Set for a compose-scoped backup; in it each image's `tags` is just the
    /// project's tag, so `primary_ref()` is what gets saved.
    pub compose: Option<ComposePlan>,
}

impl BackupPlan {
    pub fn build(inventory: &Inventory, scope: &BackupScope) -> AppResult<Self> {
        if let Some(compose) = &scope.compose {
            return Self::build_for_compose(inventory, scope, compose);
        }
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
            compose: None,
        })
    }

    /// The project's named volumes and the images its services run, as far as
    /// the daemon has them; what it lacks is listed in `not_found`.
    fn build_for_compose(
        inventory: &Inventory,
        scope: &BackupScope,
        compose: &ComposeScope,
    ) -> AppResult<Self> {
        let project = &compose.project;
        let mut meta = ComposePlan {
            info: ComposeInfo {
                project: project.name.clone(),
                files: compose.files.clone(),
            },
            ..ComposePlan::default()
        };

        let mut volumes = Vec::new();
        if scope.volumes {
            for volume in &project.volumes {
                if volume.external && !compose.include_external {
                    continue;
                }
                match inventory.volumes.iter().find(|v| v.name == volume.name) {
                    Some(found) => {
                        volumes.push(found.clone());
                        meta.volumes.insert(
                            volume.name.clone(),
                            VolumeCompose {
                                key: volume.key.clone(),
                                external: volume.external,
                            },
                        );
                    }
                    None => meta.not_found.push((ItemKind::Volume, volume.name.clone())),
                }
            }
            volumes.sort_by(|a, b| a.name.cmp(&b.name));
        }

        let mut images: Vec<ImageRef> = Vec::new();
        if scope.images {
            // Services are sorted by name, so the first service naming an image
            // picks the tag it is saved under.
            for service in &project.services {
                let Some(found) = inventory
                    .images
                    .iter()
                    .find(|i| i.tags.contains(&service.image))
                else {
                    // An absent image-only ref would never have been saved: it is pulled.
                    let missing = (ItemKind::Image, service.image.clone());
                    if (service.builds || scope.all_images) && !meta.not_found.contains(&missing) {
                        meta.not_found.push(missing);
                    }
                    continue;
                };
                if found.origin == ImageOrigin::Pulled && !scope.all_images {
                    continue;
                }
                if let Some(entry) = meta.images.get_mut(&found.id) {
                    entry.services.push(service.name.clone());
                    continue;
                }
                images.push(ImageRef {
                    id: found.id.clone(),
                    tags: vec![service.image.clone()],
                    origin: found.origin,
                });
                meta.images.insert(
                    found.id.clone(),
                    ImageCompose {
                        services: vec![service.name.clone()],
                    },
                );
            }
            images.sort_by_key(ImageRef::primary_ref);
        }

        if volumes.is_empty() && images.is_empty() {
            let message = if meta.not_found.is_empty() {
                format!(
                    "compose project {} has nothing to back up: no named volumes and no \
                     locally built images (--all-images adds pulled ones)",
                    project.name
                )
            } else {
                let names: Vec<&str> = meta
                    .not_found
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect();
                format!(
                    "none of compose project {}'s volumes or images exist on this docker \
                     daemon (not found: {}); check --docker-context",
                    project.name,
                    names.join(", ")
                )
            };
            return Err(AppError::Conflict(message));
        }

        Ok(Self {
            volumes,
            images,
            containers: Vec::new(),
            compose: Some(meta),
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

    fn shop_project() -> ComposeProject {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/compose/shop.json")).unwrap();
        ComposeProject::from_config(&config).unwrap()
    }

    fn image(id: &str, tags: &[&str], origin: ImageOrigin) -> ImageRef {
        ImageRef {
            id: id.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            origin,
        }
    }

    /// Daemon with most shop volumes, another project's volume, the image app and
    /// worker share (also tagged `mine:dev`, listed first), a pulled api image and
    /// a pulled postgres.
    fn shop_inventory() -> Inventory {
        Inventory {
            volumes: vec![
                VolumeRef::new("shop_appdata", ""),
                VolumeRef::new("shop-pg-custom", ""),
                VolumeRef::new("company-shared", ""),
                VolumeRef::new("other_data", ""),
            ],
            images: vec![
                image(
                    "sha256:app",
                    &["mine:dev", "shop-app:latest", "shop-worker:latest"],
                    ImageOrigin::Built,
                ),
                image(
                    "sha256:api",
                    &["registry.example.com/shop/api:1.2"],
                    ImageOrigin::Pulled,
                ),
                image("sha256:pg", &["postgres:17"], ImageOrigin::Pulled),
                image("sha256:other", &["other:latest"], ImageOrigin::Built),
            ],
            containers: Vec::new(),
        }
    }

    fn compose_scope(include_external: bool) -> BackupScope {
        BackupScope {
            compose: Some(ComposeScope {
                project: shop_project(),
                files: vec!["docker-compose.yml".into()],
                include_external,
            }),
            ..BackupScope::default()
        }
    }

    #[test]
    fn compose_scope_takes_only_the_projects_volumes_and_built_images() {
        let plan = BackupPlan::build(&shop_inventory(), &compose_scope(true)).unwrap();
        let volumes: Vec<&str> = plan.volumes.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            volumes,
            vec!["company-shared", "shop-pg-custom", "shop_appdata"]
        );
        assert_eq!(
            plan.images.len(),
            1,
            "one id shared by app and worker; api and pg are pulled"
        );
        assert_eq!(plan.images[0].id, "sha256:app");
        assert_eq!(
            plan.images[0].primary_ref(),
            "shop-app:latest",
            "saved under the project tag"
        );
        assert!(plan.containers.is_empty());

        let compose = plan.compose.unwrap();
        assert_eq!(compose.info.project, "shop");
        assert_eq!(compose.info.files, vec!["docker-compose.yml"]);
        assert_eq!(
            compose.volumes["shop_appdata"],
            VolumeCompose {
                key: "appdata".into(),
                external: false
            }
        );
        assert_eq!(
            compose.volumes["company-shared"],
            VolumeCompose {
                key: "shared".into(),
                external: true
            }
        );
        assert_eq!(compose.images["sha256:app"].services, vec!["app", "worker"]);
    }

    #[test]
    fn compose_not_found_lists_absent_volumes_and_built_images_only() {
        let plan = BackupPlan::build(&shop_inventory(), &compose_scope(true)).unwrap();
        // cachedata and debugdata are absent; every build service's image exists;
        // absent image-only refs (redis:7, nginx:latest) are not noise.
        assert_eq!(
            plan.compose.unwrap().not_found,
            vec![
                (ItemKind::Volume, "shop_cachedata".to_string()),
                (ItemKind::Volume, "shop_debugdata".to_string()),
            ]
        );
    }

    #[test]
    fn compose_absent_built_image_is_reported() {
        let mut inventory = shop_inventory();
        inventory.images.retain(|i| i.id != "sha256:app");
        let plan = BackupPlan::build(&inventory, &compose_scope(true)).unwrap();
        let not_found = plan.compose.unwrap().not_found;
        assert!(not_found.contains(&(ItemKind::Image, "shop-app:latest".to_string())));
        assert!(not_found.contains(&(ItemKind::Image, "shop-worker:latest".to_string())));
        assert!(!not_found.iter().any(|(_, n)| n == "redis:7"));
    }

    #[test]
    fn compose_all_images_adds_pulled_ones_and_reports_absent_image_only_refs() {
        let scope = BackupScope {
            all_images: true,
            ..compose_scope(true)
        };
        let plan = BackupPlan::build(&shop_inventory(), &scope).unwrap();
        let refs: Vec<String> = plan.images.iter().map(ImageRef::primary_ref).collect();
        assert_eq!(
            refs,
            vec![
                "postgres:17",
                "registry.example.com/shop/api:1.2",
                "shop-app:latest"
            ]
        );
        let not_found = plan.compose.unwrap().not_found;
        assert!(not_found.contains(&(ItemKind::Image, "redis:7".to_string())));
        assert!(not_found.contains(&(ItemKind::Image, "nginx:latest".to_string())));
    }

    #[test]
    fn compose_no_external_leaves_external_volumes_out() {
        let plan = BackupPlan::build(&shop_inventory(), &compose_scope(false)).unwrap();
        assert!(!plan.volumes.iter().any(|v| v.name == "company-shared"));
        assert!(
            !plan
                .compose
                .unwrap()
                .not_found
                .iter()
                .any(|(_, n)| n == "company-shared")
        );
    }

    #[test]
    fn compose_project_absent_from_the_daemon_is_a_conflict() {
        let inventory = Inventory {
            volumes: vec![VolumeRef::new("other_data", "")],
            images: vec![image("sha256:other", &["other:latest"], ImageOrigin::Built)],
            containers: Vec::new(),
        };
        let err = BackupPlan::build(&inventory, &compose_scope(true)).unwrap_err();
        assert!(
            matches!(&err, AppError::Conflict(m)
                if m.contains("none of compose project shop")
                    && m.contains("shop_appdata")
                    && m.contains("--docker-context")),
            "{err}"
        );
    }

    #[test]
    fn compose_project_with_nothing_qualifying_is_a_conflict() {
        let project = ComposeProject::from_config(&json!({
            "name": "web", "services": { "web": { "image": "nginx" } }
        }))
        .unwrap();
        let inventory = Inventory {
            images: vec![image("sha256:n", &["nginx:latest"], ImageOrigin::Pulled)],
            ..Inventory::default()
        };
        let scope = BackupScope {
            compose: Some(ComposeScope {
                project,
                files: vec![],
                include_external: true,
            }),
            ..BackupScope::default()
        };
        let err = BackupPlan::build(&inventory, &scope).unwrap_err();
        assert!(
            matches!(&err, AppError::Conflict(m)
                if m.contains("nothing to back up") && m.contains("--all-images")),
            "{err}"
        );
    }

    #[test]
    fn compose_scope_honours_no_volumes() {
        let scope = BackupScope {
            volumes: false,
            ..compose_scope(true)
        };
        let plan = BackupPlan::build(&shop_inventory(), &scope).unwrap();
        assert!(plan.volumes.is_empty());
        assert!(
            !plan
                .compose
                .unwrap()
                .not_found
                .iter()
                .any(|(k, _)| *k == ItemKind::Volume)
        );
    }

    #[test]
    fn full_scope_has_no_compose_plan() {
        let plan = BackupPlan::build(&inventory(), &BackupScope::default()).unwrap();
        assert!(plan.compose.is_none());
    }

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
                compose: None,
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
            compose: None,
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
            compose: None,
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
