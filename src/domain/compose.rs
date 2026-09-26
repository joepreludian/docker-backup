//! The parts of a rendered compose project this tool cares about: its named
//! volumes and the image each service runs. Pure — the JSON comes from
//! `docker compose config --format json`, which has already merged the files,
//! read `.env` and resolved every name.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::Manifest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeVolume {
    /// The key under the top-level `volumes:` map, e.g. `appdata`.
    pub key: String,
    /// The docker volume name compose resolves it to, e.g. `shop_appdata`.
    pub name: String,
    pub external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeService {
    pub name: String,
    /// Normalised the way docker lists it in `RepoTags`.
    pub image: String,
    pub builds: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeProject {
    pub name: String,
    /// Sorted by key.
    pub volumes: Vec<ComposeVolume>,
    /// Sorted by name.
    pub services: Vec<ComposeService>,
}

impl ComposeProject {
    pub fn from_config(config: &Value) -> AppResult<Self> {
        let name = config
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                AppError::Conflict("docker compose config returned no project name".into())
            })?
            .to_string();

        let mut volumes: Vec<ComposeVolume> = config
            .get("volumes")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(key, spec)| ComposeVolume {
                        key: key.clone(),
                        name: spec
                            .get("name")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_else(|| format!("{name}_{key}")),
                        external: spec
                            .get("external")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default();
        volumes.sort_by(|a, b| a.key.cmp(&b.key));

        let mut services: Vec<ComposeService> = config
            .get("services")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(service, spec)| {
                        let builds = spec.get("build").is_some_and(|b| !b.is_null());
                        // Compose names a build-only service's image `<project>-<service>`.
                        let image = match spec.get("image").and_then(Value::as_str) {
                            Some(image) => image.to_string(),
                            None if builds => format!("{name}-{service}"),
                            None => return None,
                        };
                        Some(ComposeService {
                            name: service.clone(),
                            image: normalize_image_ref(&image),
                            builds,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        services.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Self {
            name,
            volumes,
            services,
        })
    }

    pub fn volume_by_key(&self, key: &str) -> Option<&ComposeVolume> {
        self.volumes.iter().find(|v| v.key == key)
    }

    pub fn volume_by_name(&self, name: &str) -> Option<&ComposeVolume> {
        self.volumes.iter().find(|v| v.name == name)
    }

    pub fn service(&self, name: &str) -> Option<&ComposeService> {
        self.services.iter().find(|s| s.name == name)
    }

    pub fn image_refs(&self) -> BTreeSet<String> {
        self.services.iter().map(|s| s.image.clone()).collect()
    }
}

/// Turns a compose `image:` into the form docker lists in `RepoTags`:
/// Docker Hub prefixes dropped, `:latest` added when there is no tag.
/// Digest references are returned unchanged.
pub fn normalize_image_ref(reference: &str) -> String {
    if reference.contains('@') {
        return reference.to_string();
    }
    let mut name = reference;
    for prefix in [
        "docker.io/library/",
        "index.docker.io/library/",
        "docker.io/",
        "index.docker.io/",
        "library/",
    ] {
        if let Some(rest) = name.strip_prefix(prefix) {
            name = rest;
            break;
        }
    }
    // A registry port (`host:5000/…`) has a colon too, so only the last segment counts.
    let last_segment = name.rsplit('/').next().unwrap_or(name);
    if last_segment.contains(':') {
        name.to_string()
    } else {
        format!("{name}:latest")
    }
}

/// Top-level `compose` block of a manifest: the backup came from this project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeInfo {
    pub project: String,
    /// As typed on the command line.
    pub files: Vec<String>,
}

/// A volume entry's place in the compose project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeCompose {
    pub key: String,
    pub external: bool,
}

/// The compose services that run an image entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCompose {
    pub services: Vec<String>,
}

/// Which backup entries belong to the current compose project, and under which
/// names they go back: volumes by their compose key (so a renamed project still
/// gets its data), images re-tagged for every service that runs them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposeRestoreMap {
    pub project: String,
    /// The project the backup was made from; `None` for a full backup.
    pub backup_project: Option<String>,
    /// Entry volume name -> target volume name.
    pub volumes: BTreeMap<String, String>,
    /// Entry image ref -> tags to add after loading it.
    pub images: BTreeMap<String, Vec<String>>,
    /// Project volumes no entry maps to.
    pub not_in_backup: Vec<String>,
}

impl ComposeRestoreMap {
    pub fn build(manifest: &Manifest, project: &ComposeProject, source: &Path) -> AppResult<Self> {
        let mut volumes = BTreeMap::new();
        for entry in &manifest.volumes {
            let target = match &entry.compose {
                Some(meta) => project.volume_by_key(&meta.key),
                None => project.volume_by_name(&entry.name),
            };
            if let Some(target) = target {
                volumes.insert(entry.name.clone(), target.name.clone());
            }
        }

        let project_refs = project.image_refs();
        let mut images = BTreeMap::new();
        for entry in &manifest.images {
            let targets: BTreeSet<String> = match &entry.compose {
                // A service that now pulls another image must not get ours tagged as it.
                Some(meta) => meta
                    .services
                    .iter()
                    .filter_map(|name| project.service(name))
                    .filter(|service| service.builds || service.image == entry.reference)
                    .map(|service| service.image.clone())
                    .collect(),
                None => repo_tags(&entry.inspect)
                    .chain(std::iter::once(entry.reference.clone()))
                    .filter(|tag| project_refs.contains(tag))
                    .collect(),
            };
            if targets.is_empty() {
                continue;
            }
            let extra = targets
                .into_iter()
                .filter(|tag| *tag != entry.reference)
                .collect();
            images.insert(entry.reference.clone(), extra);
        }

        if volumes.is_empty() && images.is_empty() {
            return Err(AppError::Conflict(format!(
                "{} has nothing for compose project {}",
                source.display(),
                project.name
            )));
        }

        let targeted: BTreeSet<&String> = volumes.values().collect();
        let not_in_backup = project
            .volumes
            .iter()
            .map(|volume| &volume.name)
            .filter(|name| !targeted.contains(name))
            .cloned()
            .collect();

        Ok(Self {
            project: project.name.clone(),
            backup_project: manifest.compose.as_ref().map(|c| c.project.clone()),
            volumes,
            images,
            not_in_backup,
        })
    }
}

/// The tags an image carried when it was backed up, from its `docker image inspect`.
fn repo_tags(inspect: &Value) -> impl Iterator<Item = String> + '_ {
    inspect
        .get("RepoTags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        Compression, DockerInfo, ImageEntry, Manifest, Sha256Digest, VolumeEntry,
    };
    use crate::domain::refs::ImageOrigin;
    use serde_json::json;
    use std::path::Path;
    use time::macros::datetime;

    fn shop() -> ComposeProject {
        let config: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/compose/shop.json")).unwrap();
        ComposeProject::from_config(&config).unwrap()
    }

    #[test]
    fn reads_the_project_name_and_named_volumes() {
        let project = shop();
        assert_eq!(project.name, "shop");
        let volumes: Vec<(&str, &str, bool)> = project
            .volumes
            .iter()
            .map(|v| (v.key.as_str(), v.name.as_str(), v.external))
            .collect();
        assert_eq!(
            volumes,
            vec![
                ("appdata", "shop_appdata", false),
                ("cachedata", "shop_cachedata", false),
                ("debugdata", "shop_debugdata", false),
                ("pgdata", "shop-pg-custom", false),
                ("shared", "company-shared", true),
            ]
        );
    }

    #[test]
    fn gives_build_only_services_compose_default_image_name() {
        let project = shop();
        let services: Vec<(&str, &str, bool)> = project
            .services
            .iter()
            .map(|s| (s.name.as_str(), s.image.as_str(), s.builds))
            .collect();
        assert_eq!(
            services,
            vec![
                ("api", "registry.example.com/shop/api:1.2", true),
                ("app", "shop-app:latest", true),
                ("cache", "redis:7", false),
                ("db", "postgres:17", false),
                ("tools", "nginx:latest", false),
                ("worker", "shop-worker:latest", true),
            ]
        );
    }

    #[test]
    fn lookups_and_image_refs() {
        let project = shop();
        assert_eq!(
            project.volume_by_key("pgdata").unwrap().name,
            "shop-pg-custom"
        );
        assert_eq!(
            project.volume_by_name("company-shared").unwrap().key,
            "shared"
        );
        assert!(project.volume_by_key("nope").is_none());
        assert_eq!(project.service("app").unwrap().image, "shop-app:latest");
        assert!(project.image_refs().contains("shop-worker:latest"));
        assert_eq!(project.image_refs().len(), 6);
    }

    #[test]
    fn missing_sections_mean_none_and_a_service_without_image_or_build_is_skipped() {
        let project = ComposeProject::from_config(&json!({
            "name": "bare",
            "services": { "odd": { "command": ["true"] } }
        }))
        .unwrap();
        assert!(project.volumes.is_empty());
        assert!(project.services.is_empty());
    }

    #[test]
    fn volume_without_explicit_name_falls_back_to_project_key() {
        let project = ComposeProject::from_config(&json!({
            "name": "p", "volumes": { "data": {} }
        }))
        .unwrap();
        assert_eq!(project.volumes[0].name, "p_data");
    }

    #[test]
    fn missing_project_name_is_a_conflict() {
        let err = ComposeProject::from_config(&json!({"services": {}})).unwrap_err();
        assert!(matches!(err, AppError::Conflict(m) if m.contains("no project name")));
    }

    fn volume(name: &str, key: Option<&str>) -> VolumeEntry {
        VolumeEntry {
            name: name.into(),
            file: format!("volumes/{name}.tar"),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"x"),
            volatile: false,
            inspect: json!({}),
            compose: key.map(|k| VolumeCompose {
                key: k.into(),
                external: false,
            }),
        }
    }

    fn image(reference: &str, services: Option<&[&str]>, repo_tags: &[&str]) -> ImageEntry {
        ImageEntry {
            reference: reference.into(),
            id: format!("sha256:{reference}"),
            file: "images/x.tar".into(),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"x"),
            origin: ImageOrigin::Built,
            platform: None,
            inspect: json!({ "RepoTags": repo_tags }),
            compose: services.map(|s| ImageCompose {
                services: s.iter().map(|x| x.to_string()).collect(),
            }),
        }
    }

    fn empty_manifest() -> Manifest {
        Manifest::new(
            datetime!(2026-09-26 00:00:00 UTC),
            DockerInfo::default(),
            Compression::None,
        )
    }

    /// A backup made from project `shop` with `--from-docker-compose`.
    fn shop_backup() -> Manifest {
        let mut m = empty_manifest();
        m.compose = Some(ComposeInfo {
            project: "shop".into(),
            files: vec!["docker-compose.yml".into()],
        });
        m.volumes = vec![
            volume("company-shared", Some("shared")),
            volume("shop_appdata", Some("appdata")),
            volume("shop_gone", Some("gone")),
        ];
        m.images = vec![image("shop-app:latest", Some(&["app", "worker"]), &[])];
        m
    }

    /// `shop.json` rendered as if the folder were renamed to `shop2` (build-only
    /// services then default to `shop2-<service>` by themselves).
    fn shop2() -> ComposeProject {
        let text = include_str!("../../tests/fixtures/compose/shop.json")
            .replace("\"name\": \"shop\"", "\"name\": \"shop2\"")
            .replace("shop_", "shop2_");
        ComposeProject::from_config(&serde_json::from_str(&text).unwrap()).unwrap()
    }

    #[test]
    fn same_project_restores_under_the_same_names() {
        let map = ComposeRestoreMap::build(&shop_backup(), &shop(), Path::new("/b")).unwrap();
        assert_eq!(map.project, "shop");
        assert_eq!(map.backup_project.as_deref(), Some("shop"));
        assert_eq!(map.volumes["shop_appdata"], "shop_appdata");
        assert_eq!(map.volumes["company-shared"], "company-shared");
        assert!(
            !map.volumes.contains_key("shop_gone"),
            "key no longer in the project"
        );
        assert_eq!(
            map.images["shop-app:latest"],
            vec!["shop-worker:latest"],
            "worker shares the image"
        );
        assert_eq!(
            map.not_in_backup,
            vec!["shop_cachedata", "shop_debugdata", "shop-pg-custom"]
        );
    }

    #[test]
    fn renamed_project_remaps_volumes_by_key_and_retags_images() {
        let map = ComposeRestoreMap::build(&shop_backup(), &shop2(), Path::new("/b")).unwrap();
        assert_eq!(map.project, "shop2");
        assert_eq!(map.volumes["shop_appdata"], "shop2_appdata");
        assert_eq!(
            map.volumes["company-shared"], "company-shared",
            "external keeps its explicit name"
        );
        assert_eq!(
            map.images["shop-app:latest"],
            vec!["shop2-app:latest", "shop2-worker:latest"]
        );
    }

    #[test]
    fn a_service_that_now_pulls_its_image_is_not_retagged() {
        let mut project = shop();
        let app = project
            .services
            .iter_mut()
            .find(|s| s.name == "app")
            .unwrap();
        app.image = "nginx:latest".into();
        app.builds = false;
        let map = ComposeRestoreMap::build(&shop_backup(), &project, Path::new("/b")).unwrap();
        assert_eq!(map.images["shop-app:latest"], vec!["shop-worker:latest"]);
    }

    #[test]
    fn full_backup_entries_match_by_name_and_repo_tags() {
        let mut m = empty_manifest();
        m.volumes = vec![volume("shop_appdata", None), volume("other_data", None)];
        m.images = vec![
            image("mine:dev", None, &["mine:dev", "shop-app:latest"]),
            image("other:latest", None, &["other:latest"]),
        ];
        let map = ComposeRestoreMap::build(&m, &shop(), Path::new("/b")).unwrap();
        assert_eq!(map.backup_project, None);
        assert_eq!(map.volumes.len(), 1);
        assert_eq!(map.volumes["shop_appdata"], "shop_appdata");
        assert_eq!(map.images.len(), 1);
        assert_eq!(map.images["mine:dev"], vec!["shop-app:latest"]);
    }

    #[test]
    fn a_backup_with_nothing_for_the_project_is_a_conflict() {
        let mut m = empty_manifest();
        m.volumes = vec![volume("other_data", None)];
        let err = ComposeRestoreMap::build(&m, &shop(), Path::new("/b")).unwrap_err();
        assert!(
            matches!(&err, AppError::Conflict(msg) if msg == "/b has nothing for compose project shop"),
            "{err}"
        );
    }

    #[test]
    fn normalizes_image_refs_to_docker_repo_tags() {
        for (input, expected) in [
            ("nginx", "nginx:latest"),
            ("nginx:1.27", "nginx:1.27"),
            ("docker.io/library/nginx", "nginx:latest"),
            ("index.docker.io/library/nginx:1", "nginx:1"),
            ("docker.io/bitnami/redis:7", "bitnami/redis:7"),
            ("library/nginx", "nginx:latest"),
            (
                "registry.example.com:5000/shop/worker",
                "registry.example.com:5000/shop/worker:latest",
            ),
            ("shop-app", "shop-app:latest"),
            ("nginx@sha256:abc", "nginx@sha256:abc"),
        ] {
            assert_eq!(normalize_image_ref(input), expected, "{input}");
        }
    }
}
