//! The parts of a rendered compose project this tool cares about: its named
//! volumes and the image each service runs. Pure — the JSON comes from
//! `docker compose config --format json`, which has already merged the files,
//! read `.env` and resolved every name.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::error::{AppError, AppResult};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
