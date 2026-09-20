//! Outcome of checking the files a manifest references.

use serde::{Deserialize, Serialize};

use crate::domain::manifest::Sha256Digest;
use crate::domain::refs::ItemKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum FileStatus {
    Ok,
    Missing,
    Corrupt {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
}

impl FileStatus {
    pub fn classify(expected: &Sha256Digest, actual: Option<&Sha256Digest>) -> Self {
        match actual {
            None => FileStatus::Missing,
            Some(actual) if actual == expected => FileStatus::Ok,
            Some(actual) => FileStatus::Corrupt {
                expected: expected.clone(),
                actual: actual.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCheck {
    pub kind: ItemKind,
    pub name: String,
    pub file: String,
    pub size_bytes: u64,
    #[serde(flatten)]
    pub status: FileStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReport {
    pub files: Vec<FileCheck>,
}

impl VerificationReport {
    pub fn is_ok(&self) -> bool {
        self.files.iter().all(|f| f.status == FileStatus::Ok)
    }

    pub fn ok_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.status == FileStatus::Ok)
            .count()
    }

    pub fn missing_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.status == FileStatus::Missing)
            .count()
    }

    pub fn corrupt_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| matches!(f.status, FileStatus::Corrupt { .. }))
            .count()
    }

    pub fn summary(&self) -> String {
        format!(
            "{} ok, {} missing, {} corrupt",
            self.ok_count(),
            self.missing_count(),
            self.corrupt_count()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: FileStatus) -> FileCheck {
        FileCheck {
            kind: ItemKind::Volume,
            name: "v".into(),
            file: "volumes/v.tar".into(),
            size_bytes: 1,
            status,
        }
    }

    #[test]
    fn classify_matches() {
        let expected = Sha256Digest::of(b"a");
        assert_eq!(
            FileStatus::classify(&expected, Some(&Sha256Digest::of(b"a"))),
            FileStatus::Ok
        );
        assert_eq!(FileStatus::classify(&expected, None), FileStatus::Missing);
        assert_eq!(
            FileStatus::classify(&expected, Some(&Sha256Digest::of(b"b"))),
            FileStatus::Corrupt {
                expected: Sha256Digest::of(b"a"),
                actual: Sha256Digest::of(b"b")
            }
        );
    }

    #[test]
    fn report_counts_and_summary() {
        let report = VerificationReport {
            files: vec![
                check(FileStatus::Ok),
                check(FileStatus::Missing),
                check(FileStatus::Corrupt {
                    expected: Sha256Digest::of(b"a"),
                    actual: Sha256Digest::of(b"b"),
                }),
            ],
        };
        assert!(!report.is_ok());
        assert_eq!(report.ok_count(), 1);
        assert_eq!(report.missing_count(), 1);
        assert_eq!(report.corrupt_count(), 1);
        assert_eq!(report.summary(), "1 ok, 1 missing, 1 corrupt");
        assert!(VerificationReport::default().is_ok());
    }

    #[test]
    fn status_serializes_with_status_tag() {
        let json = serde_json::to_value(check(FileStatus::Missing)).unwrap();
        assert_eq!(json["status"], "missing");
        assert_eq!(json["kind"], "volume");
    }
}
