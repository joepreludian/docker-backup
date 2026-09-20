//! Pure rules for turning docker names into safe, unique file names.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::domain::refs::ImageRef;

pub const ARCHIVE_SUFFIX: &str = ".tar.bz2";

/// Keep `[A-Za-z0-9._-]`, replace everything else with `_`. Never empty.
pub fn sanitize(input: &str) -> String {
    let out: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() { "_".to_string() } else { out }
}

/// First 12 hex chars of an image id, without the `sha256:` prefix.
pub fn short_id(id: &str) -> &str {
    let raw = id.strip_prefix("sha256:").unwrap_or(id);
    &raw[..raw.len().min(12)]
}

pub fn image_base_name(image: &ImageRef) -> String {
    match image.tags.first() {
        Some(tag) => sanitize(&tag.replace(':', "_")),
        None => sanitize(short_id(&image.id)),
    }
}

/// Hands out file names that are unique within one backup section.
#[derive(Debug, Default)]
pub struct FileNamer {
    used: HashSet<String>,
}

impl FileNamer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn unique(&mut self, base: &str, extension: &str) -> String {
        let mut candidate = format!("{base}.{extension}");
        let mut n = 2;
        while self.used.contains(&candidate) {
            candidate = format!("{base}-{n}.{extension}");
            n += 1;
        }
        self.used.insert(candidate.clone());
        candidate
    }
}

pub fn is_archive(path: &Path) -> bool {
    path.to_string_lossy().ends_with(ARCHIVE_SUFFIX)
}

pub fn archive_path(output: &Path) -> PathBuf {
    if is_archive(output) {
        output.to_path_buf()
    } else {
        PathBuf::from(format!("{}{ARCHIVE_SUFFIX}", output.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::refs::ImageOrigin;
    use std::path::Path;

    fn image(tags: &[&str], id: &str) -> ImageRef {
        ImageRef {
            id: id.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            origin: ImageOrigin::Built,
        }
    }

    #[test]
    fn sanitize_replaces_everything_outside_the_safe_set() {
        assert_eq!(sanitize("ghcr.io/org/app"), "ghcr.io_org_app");
        assert_eq!(sanitize("a b:c@d"), "a_b_c_d");
        assert_eq!(sanitize(""), "_");
    }

    #[test]
    fn short_id_strips_prefix_and_truncates() {
        assert_eq!(short_id("sha256:28bd5fe8b56d1bd048e5"), "28bd5fe8b56d");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn tagged_image_uses_first_tag_with_colon_replaced() {
        assert_eq!(
            image_base_name(&image(&["ghcr.io/org/app:1.2", "other:x"], "sha256:aaaa")),
            "ghcr.io_org_app_1.2"
        );
    }

    #[test]
    fn untagged_image_uses_short_id() {
        assert_eq!(
            image_base_name(&image(&[], "sha256:28bd5fe8b56d1bd048e5babf")),
            "28bd5fe8b56d"
        );
    }

    #[test]
    fn namer_appends_numeric_suffix_on_collision() {
        let mut namer = FileNamer::new();
        assert_eq!(namer.unique("a", "tar"), "a.tar");
        assert_eq!(namer.unique("a", "tar"), "a-2.tar");
        assert_eq!(namer.unique("a", "tar"), "a-3.tar");
        assert_eq!(namer.unique("b", "tar.bz2"), "b.tar.bz2");
    }

    #[test]
    fn archive_path_appends_suffix_once() {
        assert_eq!(
            archive_path(Path::new("/x/out")),
            Path::new("/x/out.tar.bz2")
        );
        assert_eq!(
            archive_path(Path::new("/x/out.tar.bz2")),
            Path::new("/x/out.tar.bz2")
        );
        assert!(is_archive(Path::new("b.tar.bz2")));
        assert!(!is_archive(Path::new("b")));
    }
}
