//! Identifiers for the three kinds of docker objects this tool handles,
//! plus the pure rules that classify them.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Volume,
    Image,
    Container,
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            ItemKind::Volume => "VOLUME",
            ItemKind::Image => "IMAGE",
            ItemKind::Container => "CONTAINER",
        };
        f.write_str(label)
    }
}

pub const ANONYMOUS_VOLUME_LABEL: &str = "com.docker.volume.anonymous";

/// A volume is "volatile" (anonymous) when docker labelled it so, or when its
/// name is the 64-hex-char id docker generates for anonymous volumes.
pub fn is_anonymous_volume(name: &str, labels: &str) -> bool {
    let looks_generated =
        name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    labels.contains(ANONYMOUS_VOLUME_LABEL) || looks_generated
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRef {
    pub name: String,
    pub volatile: bool,
}

impl VolumeRef {
    pub fn new(name: impl Into<String>, labels: &str) -> Self {
        let name = name.into();
        let volatile = is_anonymous_volume(&name, labels);
        Self { name, volatile }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageOrigin {
    Built,
    Pulled,
}

/// Pulled images carry a non-empty `Identity.Pull` array in `docker image inspect`.
/// Everything else (built locally, imported, or predating the field) counts as built.
pub fn image_origin_from_inspect(inspect: &Value) -> ImageOrigin {
    let pulled = inspect
        .pointer("/Identity/Pull")
        .and_then(Value::as_array)
        .is_some_and(|entries| !entries.is_empty());
    if pulled {
        ImageOrigin::Pulled
    } else {
        ImageOrigin::Built
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub id: String,
    pub tags: Vec<String>,
    pub origin: ImageOrigin,
}

impl ImageRef {
    pub fn from_inspect(inspect: &Value) -> Option<Self> {
        let id = inspect.get("Id")?.as_str()?.to_string();
        let tags = inspect
            .get("RepoTags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .filter(|tag| *tag != "<none>:<none>")
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            id,
            tags,
            origin: image_origin_from_inspect(inspect),
        })
    }

    /// The first tag, or the image id when the image is untagged.
    pub fn primary_ref(&self) -> String {
        self.tags
            .first()
            .cloned()
            .unwrap_or_else(|| self.id.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerRef {
    pub id: String,
    pub name: String,
    pub image: String,
    pub running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn fixture(name: &str) -> Value {
        let json = match name {
            "pulled" => include_str!("../../tests/fixtures/inspect/image_pulled.json"),
            "built" => include_str!("../../tests/fixtures/inspect/image_built.json"),
            "none" => include_str!("../../tests/fixtures/inspect/image_no_identity.json"),
            other => panic!("unknown fixture {other}"),
        };
        serde_json::from_str(json).expect("fixture is valid json")
    }

    #[test]
    fn named_volume_is_not_volatile() {
        assert!(!is_anonymous_volume("pgdata", ""));
    }

    #[test]
    fn sixty_four_lowercase_hex_name_is_volatile() {
        assert!(is_anonymous_volume(&"0c".repeat(32), ""));
    }

    #[test]
    fn uppercase_hex_name_is_not_volatile() {
        assert!(!is_anonymous_volume(&"AB".repeat(32), ""));
    }

    #[test]
    fn anonymous_label_marks_volume_volatile() {
        assert!(is_anonymous_volume(
            "weird-name",
            "com.docker.volume.anonymous="
        ));
    }

    #[test]
    fn volume_ref_new_computes_volatile() {
        assert!(VolumeRef::new("0c".repeat(32), "").volatile);
        assert!(!VolumeRef::new("pgdata", "").volatile);
    }

    #[test]
    fn image_with_identity_pull_is_pulled() {
        assert_eq!(
            image_origin_from_inspect(&fixture("pulled")),
            ImageOrigin::Pulled
        );
    }

    #[test]
    fn image_with_identity_build_is_built() {
        assert_eq!(
            image_origin_from_inspect(&fixture("built")),
            ImageOrigin::Built
        );
    }

    #[test]
    fn image_without_identity_is_built() {
        assert_eq!(
            image_origin_from_inspect(&fixture("none")),
            ImageOrigin::Built
        );
    }

    #[test]
    fn image_ref_from_inspect_reads_id_tags_and_origin() {
        let image = ImageRef::from_inspect(&fixture("pulled")).expect("parses");
        assert_eq!(
            image.id,
            "sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b"
        );
        assert_eq!(
            image.tags,
            vec!["alpine:3".to_string(), "alpine:latest".to_string()]
        );
        assert_eq!(image.origin, ImageOrigin::Pulled);
        assert_eq!(image.primary_ref(), "alpine:3");
    }

    #[test]
    fn untagged_image_primary_ref_is_its_id() {
        let image = ImageRef::from_inspect(&fixture("none")).expect("parses");
        assert!(image.tags.is_empty());
        assert_eq!(image.primary_ref(), image.id);
    }

    #[test]
    fn none_tag_is_dropped() {
        let value = serde_json::json!({"Id": "sha256:abc", "RepoTags": ["<none>:<none>"]});
        let image = ImageRef::from_inspect(&value).expect("parses");
        assert!(image.tags.is_empty());
    }

    #[test]
    fn item_kind_displays_upper_case() {
        assert_eq!(ItemKind::Volume.to_string(), "VOLUME");
        assert_eq!(ItemKind::Image.to_string(), "IMAGE");
        assert_eq!(ItemKind::Container.to_string(), "CONTAINER");
    }
}
