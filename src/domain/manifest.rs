//! The `manifest.json` model: everything a sysadmin needs to restore by hand.

use std::fmt;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::domain::error::{AppError, AppResult};
use crate::domain::platform::Platform;
use crate::domain::refs::{ImageOrigin, ItemKind};

pub const SCHEMA_VERSION: u32 = 1;
pub const HASH_ALGORITHM: &str = "sha256";
pub const MANIFEST_FILE: &str = "manifest.json";
pub const PARTIAL_MANIFEST_FILE: &str = "manifest.partial.json";
pub const VOLUMES_DIR: &str = "volumes";
pub const IMAGES_DIR: &str = "images";
pub const CONTAINERS_DIR: &str = "containers";

/// Lowercase hex SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sha256Digest(pub String);

impl Sha256Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }

    pub fn from_raw(raw: &[u8]) -> Self {
        Self(hex::encode(raw))
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Compression {
    None,
    Bzip2PerFile,
}

impl Compression {
    pub fn extension(self) -> &'static str {
        match self {
            Compression::None => "tar",
            Compression::Bzip2PerFile => "tar.bz2",
        }
    }

    /// Human-readable label matching this variant's serde (kebab-case) name.
    pub fn label(self) -> &'static str {
        match self {
            Compression::None => "none",
            Compression::Bzip2PerFile => "bzip2-per-file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolMeta {
    pub name: String,
    pub version: String,
}

impl ToolMeta {
    pub fn current() -> Self {
        Self {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerInfo {
    pub server_version: String,
    pub client_version: String,
    pub host: String,
    pub context: String,
    pub os: String,
    pub arch: String,
}

impl DockerInfo {
    pub fn platform(&self) -> Platform {
        Platform::new(self.os.clone(), self.arch.clone())
    }

    /// Backup and restore refuse to run against a daemon whose platform is unknown.
    pub fn require_platform(&self) -> AppResult<()> {
        if self.platform().is_known() {
            Ok(())
        } else {
            Err(AppError::DockerUnavailable(
                "docker daemon did not report its platform (os/arch)".into(),
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub path: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeEntry {
    pub name: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
    pub volatile: bool,
    #[serde(default)]
    pub inspect: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageEntry {
    #[serde(rename = "ref")]
    pub reference: String,
    pub id: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
    pub origin: ImageOrigin,
    /// Platform the image was built for; absent in manifests written before 0.2.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    #[serde(default)]
    pub inspect: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerEntry {
    pub name: String,
    pub id: String,
    pub image: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
    /// Platform the container's image was built for; absent in manifests written before 0.2.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    #[serde(default)]
    pub inspect: Value,
}

/// One stored file as referenced by the manifest, kind-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFile {
    pub kind: ItemKind,
    pub name: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub tool: ToolMeta,
    pub docker: DockerInfo,
    pub compression: Compression,
    pub hash_algorithm: String,
    #[serde(default)]
    pub volumes: Vec<VolumeEntry>,
    #[serde(default)]
    pub images: Vec<ImageEntry>,
    #[serde(default)]
    pub containers: Vec<ContainerEntry>,
}

impl Manifest {
    pub fn new(created_at: OffsetDateTime, docker: DockerInfo, compression: Compression) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            created_at,
            tool: ToolMeta::current(),
            docker,
            compression,
            hash_algorithm: HASH_ALGORITHM.to_string(),
            volumes: Vec::new(),
            images: Vec::new(),
            containers: Vec::new(),
        }
    }

    pub fn to_json(&self) -> AppResult<String> {
        serde_json::to_string_pretty(self).map_err(|e| AppError::ManifestInvalid(e.to_string()))
    }

    pub fn from_json(json: &str) -> AppResult<Self> {
        let value: Value =
            serde_json::from_str(json).map_err(|e| AppError::ManifestInvalid(e.to_string()))?;
        let version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| AppError::ManifestInvalid("missing schema_version".into()))?;
        if version != u64::from(SCHEMA_VERSION) {
            return Err(AppError::ManifestUnsupportedVersion(version as u32));
        }
        let manifest: Manifest =
            serde_json::from_value(value).map_err(|e| AppError::ManifestInvalid(e.to_string()))?;
        if manifest.hash_algorithm != HASH_ALGORITHM {
            return Err(AppError::ManifestInvalid(format!(
                "unsupported hash algorithm {}",
                manifest.hash_algorithm
            )));
        }
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reject anything a hostile manifest could use to write outside the backup
    /// folder or to name a docker object we would then hand to the CLI.
    pub fn validate(&self) -> AppResult<()> {
        for volume in &self.volumes {
            check_docker_name("volume", &volume.name)?;
            check_file("volume", &volume.name, &volume.file)?;
        }
        for image in &self.images {
            check_file("image", &image.reference, &image.file)?;
        }
        for container in &self.containers {
            check_docker_name("container", &container.name)?;
            check_file("container", &container.name, &container.file)?;
        }
        Ok(())
    }

    pub fn item_count(&self) -> usize {
        self.volumes.len() + self.images.len() + self.containers.len()
    }

    pub fn total_bytes(&self) -> u64 {
        self.files().iter().map(|f| f.size_bytes).sum()
    }

    pub fn files(&self) -> Vec<ManifestFile> {
        let volumes = self.volumes.iter().map(|v| ManifestFile {
            kind: ItemKind::Volume,
            name: v.name.clone(),
            file: v.file.clone(),
            size_bytes: v.size_bytes,
            sha256: v.sha256.clone(),
        });
        let images = self.images.iter().map(|i| ManifestFile {
            kind: ItemKind::Image,
            name: i.reference.clone(),
            file: i.file.clone(),
            size_bytes: i.size_bytes,
            sha256: i.sha256.clone(),
        });
        let containers = self.containers.iter().map(|c| ManifestFile {
            kind: ItemKind::Container,
            name: c.name.clone(),
            file: c.file.clone(),
            size_bytes: c.size_bytes,
            sha256: c.sha256.clone(),
        });
        volumes.chain(images).chain(containers).collect()
    }

    /// The image's own platform, or the backup daemon's platform for old manifests.
    pub fn image_platform(&self, entry: &ImageEntry) -> Platform {
        entry
            .platform
            .clone()
            .unwrap_or_else(|| self.docker.platform())
    }

    /// The container's own platform, or the backup daemon's platform for old manifests.
    pub fn container_platform(&self, entry: &ContainerEntry) -> Platform {
        entry
            .platform
            .clone()
            .unwrap_or_else(|| self.docker.platform())
    }
}

/// The only directories a manifest may reference, in the order `files()` uses them.
const ITEM_DIRS: [&str; 3] = [VOLUMES_DIR, IMAGES_DIR, CONTAINERS_DIR];

/// Docker's own rule for volume and container names: `^[A-Za-z0-9][A-Za-z0-9_.-]*$`.
fn is_docker_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let starts_well = bytes.next().is_some_and(|b| b.is_ascii_alphanumeric());
    starts_well && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// `Ok` only for `volumes/…`, `images/…` or `containers/…` relative paths with
/// no `..`, no root and no prefix component.
fn check_relative_item_path(file: &str) -> Result<(), &'static str> {
    let components: Vec<Component<'_>> = Path::new(file).components().collect();
    let Some(Component::Normal(first)) = components.first() else {
        return Err("is not a relative path");
    };
    if !ITEM_DIRS.contains(&&*first.to_string_lossy()) {
        return Err("is not inside volumes/, images/ or containers/");
    }
    if components.len() < 2 {
        return Err("does not name a file");
    }
    if !components.iter().all(|c| matches!(c, Component::Normal(_))) {
        return Err("escapes the backup folder");
    }
    Ok(())
}

fn check_docker_name(kind: &str, name: &str) -> AppResult<()> {
    if is_docker_name(name) {
        Ok(())
    } else {
        Err(AppError::ManifestInvalid(format!(
            "{kind} name {name:?} is not a valid docker name"
        )))
    }
}

fn check_file(kind: &str, name: &str, file: &str) -> AppResult<()> {
    check_relative_item_path(file)
        .map_err(|why| AppError::ManifestInvalid(format!("{kind} {name:?}: file {file:?} {why}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    fn sample() -> Manifest {
        let mut manifest = Manifest::new(
            datetime!(2026-09-19 14:03:11 UTC),
            DockerInfo {
                server_version: "29.5.2".into(),
                client_version: "29.8.1".into(),
                host: "unix:///var/run/docker.sock".into(),
                context: "default".into(),
                os: "linux".into(),
                arch: "arm64".into(),
            },
            Compression::None,
        );
        manifest.volumes.push(VolumeEntry {
            name: "pgdata".into(),
            file: "volumes/pgdata.tar".into(),
            size_bytes: 3,
            sha256: Sha256Digest::of(b"abc"),
            volatile: false,
            inspect: json!({"Name": "pgdata"}),
        });
        manifest.images.push(ImageEntry {
            reference: "app:latest".into(),
            id: "sha256:abc".into(),
            file: "images/app_latest.tar".into(),
            size_bytes: 5,
            sha256: Sha256Digest::of(b"image"),
            origin: ImageOrigin::Built,
            platform: None,
            inspect: json!({}),
        });
        manifest.containers.push(ContainerEntry {
            name: "web".into(),
            id: "1234".into(),
            image: "nginx:alpine".into(),
            file: "containers/web.tar".into(),
            size_bytes: 7,
            sha256: Sha256Digest::of(b"container"),
            platform: None,
            inspect: json!({}),
        });
        manifest
    }

    #[test]
    fn sha256_of_known_input() {
        assert_eq!(
            Sha256Digest::of(b"hello\n").to_string(),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn compression_extensions() {
        assert_eq!(Compression::None.extension(), "tar");
        assert_eq!(Compression::Bzip2PerFile.extension(), "tar.bz2");
    }

    #[test]
    fn compression_labels() {
        assert_eq!(Compression::None.label(), "none");
        assert_eq!(Compression::Bzip2PerFile.label(), "bzip2-per-file");
    }

    #[test]
    fn round_trips_through_json() {
        let manifest = sample();
        let json = manifest.to_json().unwrap();
        let parsed = Manifest::from_json(&json).unwrap();
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn json_uses_spec_field_names() {
        let json = sample().to_json().unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["created_at"], "2026-09-19T14:03:11Z");
        assert_eq!(value["hash_algorithm"], "sha256");
        assert_eq!(value["compression"], "none");
        assert_eq!(value["images"][0]["ref"], "app:latest");
        assert_eq!(value["images"][0]["origin"], "built");
        assert_eq!(value["tool"]["name"], "docker-backup");
    }

    #[test]
    fn bzip2_compression_serializes_as_kebab_case() {
        let mut manifest = sample();
        manifest.compression = Compression::Bzip2PerFile;
        let value: serde_json::Value = serde_json::from_str(&manifest.to_json().unwrap()).unwrap();
        assert_eq!(value["compression"], "bzip2-per-file");
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let mut value: serde_json::Value =
            serde_json::from_str(&sample().to_json().unwrap()).unwrap();
        value["schema_version"] = json!(2);
        let err = Manifest::from_json(&value.to_string()).unwrap_err();
        assert!(matches!(err, AppError::ManifestUnsupportedVersion(2)));
    }

    #[test]
    fn rejects_unknown_hash_algorithm() {
        let mut value: serde_json::Value =
            serde_json::from_str(&sample().to_json().unwrap()).unwrap();
        value["hash_algorithm"] = json!("md5");
        let err = Manifest::from_json(&value.to_string()).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(_)));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            Manifest::from_json("{"),
            Err(AppError::ManifestInvalid(_))
        ));
        assert!(matches!(
            Manifest::from_json("{}"),
            Err(AppError::ManifestInvalid(_))
        ));
    }

    fn parsed_with(mutate: impl FnOnce(&mut Value)) -> AppResult<Manifest> {
        let mut value: Value = serde_json::from_str(&sample().to_json().unwrap()).unwrap();
        mutate(&mut value);
        Manifest::from_json(&value.to_string())
    }

    #[test]
    fn rejects_absolute_file_paths() {
        let err = parsed_with(|v| v["volumes"][0]["file"] = json!("/etc/passwd")).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("/etc/passwd")));
    }

    #[test]
    fn rejects_parent_directory_traversal() {
        let err = parsed_with(|v| v["volumes"][0]["file"] = json!("volumes/../../etc/passwd"))
            .unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("pgdata")));
    }

    #[test]
    fn rejects_files_outside_the_backup_directories() {
        let err = parsed_with(|v| v["images"][0]["file"] = json!("elsewhere/app.tar")).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("elsewhere/app.tar")));
    }

    #[test]
    fn rejects_hostile_volume_names() {
        let err = parsed_with(|v| v["volumes"][0]["name"] = json!("/")).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("volume")));
    }

    #[test]
    fn rejects_container_names_with_spaces() {
        let err = parsed_with(|v| v["containers"][0]["name"] = json!("web app")).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("web app")));
    }

    #[test]
    fn docker_info_platform_and_validation() {
        let info = DockerInfo {
            os: "linux".into(),
            arch: "arm64".into(),
            ..DockerInfo::default()
        };
        assert_eq!(info.platform(), Platform::new("linux", "arm64"));
        assert!(info.require_platform().is_ok());

        let unknown = DockerInfo::default();
        let error = unknown.require_platform().unwrap_err();
        assert_eq!(error.exit_code(), 3);
        assert!(error.to_string().contains("platform"));
    }

    #[test]
    fn entry_platform_falls_back_to_daemon_platform() {
        let docker = DockerInfo {
            os: "linux".into(),
            arch: "amd64".into(),
            ..DockerInfo::default()
        };
        let mut m = Manifest::new(
            datetime!(2026-09-20 00:00:00 UTC),
            docker,
            Compression::None,
        );
        m.images.push(ImageEntry {
            reference: "a:1".into(),
            id: "sha256:1".into(),
            file: "images/a_1.tar".into(),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"x"),
            origin: ImageOrigin::Built,
            platform: None,
            inspect: json!({}),
        });
        m.images.push(ImageEntry {
            reference: "b:1".into(),
            id: "sha256:2".into(),
            file: "images/b_1.tar".into(),
            size_bytes: 1,
            sha256: Sha256Digest::of(b"y"),
            origin: ImageOrigin::Built,
            platform: Some(Platform::new("linux", "arm64")),
            inspect: json!({}),
        });
        assert_eq!(
            m.image_platform(&m.images[0]),
            Platform::new("linux", "amd64")
        );
        assert_eq!(
            m.image_platform(&m.images[1]),
            Platform::new("linux", "arm64")
        );

        let json = m.to_json().unwrap();
        assert!(
            !json.contains("\"platform\": null"),
            "None must be omitted: {json}"
        );
        let back = Manifest::from_json(&json).unwrap();
        assert_eq!(
            back.images[1].platform,
            Some(Platform::new("linux", "arm64"))
        );
    }

    #[test]
    fn manifests_without_platform_fields_still_load() {
        // Existing fixture predates the platform field.
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/backup_corrupt/manifest.json"
        ))
        .unwrap();
        let m = Manifest::from_json(&text).unwrap();
        assert!(m.images.iter().all(|i| i.platform.is_none()));
    }

    #[test]
    fn files_lists_every_entry_in_order() {
        let files = sample().files();
        let names: Vec<(ItemKind, &str)> =
            files.iter().map(|f| (f.kind, f.name.as_str())).collect();
        assert_eq!(
            names,
            vec![
                (ItemKind::Volume, "pgdata"),
                (ItemKind::Image, "app:latest"),
                (ItemKind::Container, "web")
            ]
        );
        assert_eq!(sample().item_count(), 3);
        assert_eq!(sample().total_bytes(), 15);
    }
}
