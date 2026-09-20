//! Platform (os/arch) as reported by the docker daemon and by image metadata.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub variant: String,
}

impl Platform {
    pub fn new(os: impl Into<String>, arch: impl Into<String>) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
            variant: String::new(),
        }
    }

    /// Reads `Os`, `Architecture` and `Variant` from `docker image inspect` output.
    pub fn from_image_inspect(inspect: &Value) -> Option<Self> {
        let text = |key: &str| {
            inspect
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let platform = Self {
            os: text("Os"),
            arch: text("Architecture"),
            variant: text("Variant"),
        };
        platform.is_known().then_some(platform)
    }

    pub fn is_known(&self) -> bool {
        !self.os.is_empty() && !self.arch.is_empty()
    }

    /// Same os and arch, case-insensitive. Variant (v7, v8, …) is ignored.
    pub fn matches(&self, other: &Platform) -> bool {
        self.os.eq_ignore_ascii_case(&other.os) && self.arch.eq_ignore_ascii_case(&other.arch)
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.variant.is_empty() {
            write!(f, "{}/{}", self.os, self.arch)
        } else {
            write!(f, "{}/{}/{}", self.os, self.arch, self.variant)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_image_inspect_reads_os_arch_and_variant() {
        let inspect = json!({"Id": "x", "Os": "linux", "Architecture": "arm64", "Variant": "v8"});
        let platform = Platform::from_image_inspect(&inspect).unwrap();
        assert_eq!(
            platform,
            Platform {
                os: "linux".into(),
                arch: "arm64".into(),
                variant: "v8".into()
            }
        );
        assert_eq!(platform.to_string(), "linux/arm64/v8");
    }

    #[test]
    fn from_image_inspect_is_none_without_os_or_arch() {
        assert!(Platform::from_image_inspect(&json!({"Id": "x"})).is_none());
        assert!(Platform::from_image_inspect(&json!({"Os": "linux"})).is_none());
        assert!(
            Platform::from_image_inspect(&json!({"Os": "", "Architecture": "arm64"})).is_none()
        );
    }

    #[test]
    fn matches_ignores_variant_and_case() {
        let a = Platform {
            os: "linux".into(),
            arch: "arm64".into(),
            variant: "v8".into(),
        };
        let b = Platform::new("Linux", "ARM64");
        assert!(a.matches(&b));
        assert!(!a.matches(&Platform::new("linux", "amd64")));
        assert!(!a.matches(&Platform::new("windows", "arm64")));
    }

    #[test]
    fn display_omits_empty_variant() {
        assert_eq!(Platform::new("linux", "amd64").to_string(), "linux/amd64");
    }

    #[test]
    fn is_known_requires_os_and_arch() {
        assert!(Platform::new("linux", "amd64").is_known());
        assert!(!Platform::new("", "amd64").is_known());
        assert!(!Platform::new("linux", "").is_known());
    }
}
