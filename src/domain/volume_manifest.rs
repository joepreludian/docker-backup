//! The `volume.json` model: what a single-volume archive holds, checkable by hand.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::{Sha256Digest, ToolMeta, is_docker_name};

pub const VOLUME_SCHEMA_VERSION: u32 = 1;
/// Not `manifest.json`, so the two archive formats are never mistaken for each other.
pub const VOLUME_MANIFEST_FILE: &str = "volume.json";
/// The volume's contents (`tar -C /data -cf - .`). A constant rather than a
/// manifest field, so a manifest can never point outside the archive.
pub const VOLUME_DATA_FILE: &str = "backup.tar";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeManifest {
    pub schema_version: u32,
    pub volume: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Size of `backup.tar`.
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of `backup.tar`.
    pub sha256: Sha256Digest,
    pub tool: ToolMeta,
}

impl VolumeManifest {
    pub fn new(
        volume: impl Into<String>,
        created_at: OffsetDateTime,
        size_bytes: u64,
        sha256: Sha256Digest,
    ) -> Self {
        Self {
            schema_version: VOLUME_SCHEMA_VERSION,
            volume: volume.into(),
            created_at,
            size_bytes,
            sha256,
            tool: ToolMeta::current(),
        }
    }

    /// Pretty JSON with a trailing newline, as written to `volume.json`.
    pub fn to_json(&self) -> AppResult<String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::ManifestInvalid(e.to_string()))?;
        Ok(json + "\n")
    }

    pub fn from_json(json: &str) -> AppResult<Self> {
        let value: Value =
            serde_json::from_str(json).map_err(|e| AppError::ManifestInvalid(e.to_string()))?;
        let version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| AppError::ManifestInvalid("missing schema_version".into()))?;
        if version != u64::from(VOLUME_SCHEMA_VERSION) {
            return Err(AppError::ManifestUnsupportedVersion(version as u32));
        }
        let manifest: Self =
            serde_json::from_value(value).map_err(|e| AppError::ManifestInvalid(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reject a volume name docker itself would refuse, since it is handed to
    /// the docker CLI, and a digest that could never match a file.
    fn validate(&self) -> AppResult<()> {
        if !is_docker_name(&self.volume) {
            return Err(AppError::ManifestInvalid(format!(
                "volume name {:?} is not a valid docker name",
                self.volume
            )));
        }
        if !is_lowercase_sha256(&self.sha256.0) {
            return Err(AppError::ManifestInvalid(format!(
                "sha256 {:?} is not 64 lowercase hex characters",
                self.sha256.0
            )));
        }
        Ok(())
    }
}

fn is_lowercase_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    fn sample() -> VolumeManifest {
        VolumeManifest::new(
            "pgdata",
            datetime!(2026-09-26 14:15:00 UTC),
            6,
            Sha256Digest::of(b"hello\n"),
        )
    }

    fn parsed_with(mutate: impl FnOnce(&mut Value)) -> AppResult<VolumeManifest> {
        let mut value: Value = serde_json::from_str(&sample().to_json().unwrap()).unwrap();
        mutate(&mut value);
        VolumeManifest::from_json(&value.to_string())
    }

    #[test]
    fn new_fills_schema_version_and_tool() {
        let manifest = sample();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.tool, ToolMeta::current());
        assert_eq!(manifest.size_bytes, 6);
    }

    #[test]
    fn round_trips_through_json() {
        let manifest = sample();
        let parsed = VolumeManifest::from_json(&manifest.to_json().unwrap()).unwrap();
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn json_uses_spec_field_names_and_ends_with_a_newline() {
        let json = sample().to_json().unwrap();
        assert!(json.ends_with("}\n"), "json was {json:?}");
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["volume"], "pgdata");
        assert_eq!(value["created_at"], "2026-09-26T14:15:00Z");
        assert_eq!(value["size_bytes"], 6);
        assert_eq!(
            value["sha256"],
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        assert_eq!(value["tool"]["name"], "docker-backup");
        assert_eq!(value["tool"]["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let err = parsed_with(|v| v["schema_version"] = json!(2)).unwrap_err();
        assert!(matches!(err, AppError::ManifestUnsupportedVersion(2)));
    }

    #[test]
    fn rejects_an_empty_volume_name() {
        let err = parsed_with(|v| v["volume"] = json!("")).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("volume name")));
    }

    #[test]
    fn rejects_volume_names_docker_would_refuse() {
        for hostile in ["/", "-v", "a b", "../etc", "/var/lib/docker"] {
            let err = parsed_with(|v| v["volume"] = json!(hostile)).unwrap_err();
            assert!(
                matches!(&err, AppError::ManifestInvalid(msg) if msg.contains(hostile)),
                "{hostile:?} gave {err:?}"
            );
        }
    }

    #[test]
    fn rejects_a_short_or_uppercase_hash() {
        let err = parsed_with(|v| v["sha256"] = json!("abc123")).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("sha256")));
        let upper = "5891B5B522D5DF086D0FF0B110FBD9D21BB4FC7163AF34D08286A2E846F6BE03";
        let err = parsed_with(|v| v["sha256"] = json!(upper)).unwrap_err();
        assert!(matches!(err, AppError::ManifestInvalid(msg) if msg.contains("sha256")));
    }

    #[test]
    fn rejects_missing_fields_and_garbage() {
        for field in [
            "schema_version",
            "volume",
            "created_at",
            "size_bytes",
            "sha256",
            "tool",
        ] {
            let err = parsed_with(|v| {
                v.as_object_mut().unwrap().remove(field);
            })
            .unwrap_err();
            assert!(
                matches!(&err, AppError::ManifestInvalid(_)),
                "without {field}: {err:?}"
            );
        }
        assert!(matches!(
            VolumeManifest::from_json("{"),
            Err(AppError::ManifestInvalid(_))
        ));
    }
}
