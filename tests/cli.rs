use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

fn bin() -> Command {
    Command::cargo_bin("docker-backup").expect("binary builds")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// SHA-256 of b"hello\n", which every volume.json written below records.
const HELLO_SHA256: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

fn has(tool: &str) -> bool {
    which::which(tool).is_ok()
}

/// Writes the layout of an extracted single-volume archive into `folder`:
/// `backup.tar` holding `data`, and a `volume.json` recording the hash of "hello\n".
fn write_volume_files(folder: &Path, data: &[u8]) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("backup.tar"), data).unwrap();
    let manifest = format!(
        r#"{{
  "schema_version": 1,
  "volume": "pgdata",
  "created_at": "2026-09-26T14:15:00Z",
  "size_bytes": 6,
  "sha256": "{HELLO_SHA256}",
  "tool": {{ "name": "docker-backup", "version": "0.3.0" }}
}}
"#
    );
    std::fs::write(folder.join("volume.json"), manifest).unwrap();
}

fn volume_folder(data: &[u8]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_volume_files(dir.path(), data);
    dir
}

/// A real `<stem>.tar.bz2` holding one `<stem>/` folder, packed with the system
/// tar the way backup-volume packs it. `None` when tar or bzip2 is missing.
fn volume_archive(data: &[u8]) -> Option<(tempfile::TempDir, PathBuf)> {
    if !has("tar") || !has("bzip2") {
        eprintln!("tar/bzip2 not installed, skipping");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let stem = "pgdata-20260926T141500Z";
    write_volume_files(&dir.path().join(stem), data);
    let archive = dir.path().join(format!("{stem}.tar.bz2"));
    let status = std::process::Command::new("tar")
        .arg("-cjf")
        .arg(&archive)
        .arg("-C")
        .arg(dir.path())
        .arg(stem)
        .status()
        .unwrap();
    assert!(status.success(), "tar -cjf failed");
    Some((dir, archive))
}

/// Nothing named `.docker-backup-*` (a scratch dir) is left in `dir`.
fn no_scratch_left(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".docker-backup-")
    })
}

#[test]
fn help_lists_all_commands() {
    bin().arg("--help").assert().success().stdout(
        predicate::str::contains("backup")
            .and(predicate::str::contains("restore"))
            .and(predicate::str::contains("backup-volume"))
            .and(predicate::str::contains("restore-volume"))
            .and(predicate::str::contains("info"))
            .and(predicate::str::contains("doctor")),
    );
}

#[test]
fn restore_help_lists_confirmation_flags() {
    bin().args(["restore", "--help"]).assert().success().stdout(
        predicate::str::contains("--yes")
            .and(predicate::str::contains("--force-import-if-arch-mismatch")),
    );
}

#[test]
fn conflicting_compression_flags_are_a_usage_error() {
    bin()
        .args(["backup", "--per-file-bzip2", "--single-archive"])
        .assert()
        .code(2);
}

#[test]
fn info_reports_corruption_with_exit_code_1() {
    bin()
        .args(["info"])
        .arg(fixture("backup_corrupt"))
        .assert()
        .code(1)
        .stdout(predicate::str::contains("corrupt").and(predicate::str::contains("good")));
}

#[test]
fn info_json_is_a_single_document_on_stdout() {
    let output = bin()
        .args(["--json", "info"])
        .arg(fixture("backup_corrupt"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is one json document");
    assert_eq!(value["verification"]["files"][1]["status"], "corrupt");
    assert_eq!(value["manifest"]["volumes"].as_array().unwrap().len(), 2);
}

#[test]
fn info_on_missing_folder_fails_cleanly() {
    bin()
        .args(["info", "/definitely/not/a/backup"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("manifest.json"));
}

#[test]
fn json_error_goes_to_stdout() {
    let output = bin()
        .args(["--json", "info", "/definitely/not/a/backup"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "manifest_invalid");
}

#[test]
fn doctor_json_always_produces_a_document() {
    let output = bin().args(["--json", "doctor"]).output().unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is one json document");
    assert!(
        value["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "docker")
    );
}

#[test]
fn info_reads_a_single_volume_folder() {
    let dir = volume_folder(b"hello\n");
    bin().arg("info").arg(dir.path()).assert().success().stdout(
        predicate::str::contains("single volume")
            .and(predicate::str::contains("backup.tar"))
            .and(predicate::str::contains(HELLO_SHA256)),
    );
}

#[test]
fn info_json_tags_a_single_volume_archive() {
    let dir = volume_folder(b"hello\n");
    let output = bin()
        .args(["--json", "info"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is one json document");
    assert_eq!(value["type"], "volume");
    assert_eq!(value["manifest"]["volume"], "pgdata");
    assert_eq!(value["verification"]["files"][0]["status"], "ok");
}

#[test]
fn info_reports_a_corrupt_single_volume_archive_with_exit_code_1() {
    let dir = volume_folder(b"tampered\n");
    bin()
        .arg("info")
        .arg(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("corrupt"));
}

#[test]
fn info_unpacks_a_single_volume_archive_and_cleans_up() {
    let Some((dir, archive)) = volume_archive(b"hello\n") else {
        return;
    };
    bin()
        .arg("info")
        .arg(&archive)
        .assert()
        .success()
        .stdout(predicate::str::contains("single volume"));
    assert!(no_scratch_left(dir.path()));
}

#[test]
fn backup_volume_without_a_name_is_a_usage_error() {
    bin()
        .arg("backup-volume")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("<NAME>"));
}

#[test]
fn backup_volume_writes_nothing_when_docker_is_unreachable() {
    // An unknown context makes docker unreachable without touching any daemon.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    bin()
        .args([
            "--docker-context",
            "definitely-not-a-context-xyz",
            "backup-volume",
            "pgdata",
            "-o",
        ])
        .arg(&out)
        .assert()
        .code(3);
    assert!(!out.exists());
}

#[test]
fn restore_volume_refuses_a_path_that_is_not_an_archive() {
    bin()
        .args(["restore-volume", "/definitely/not/an/archive"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a .tar.bz2 archive"));
}

#[test]
fn restore_volume_refuses_an_as_name_docker_would_refuse() {
    bin()
        .args([
            "restore-volume",
            "/definitely/not/here.tar.bz2",
            "--as",
            "a/b",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not a valid volume name"));
}

#[test]
fn restore_volume_verifies_the_archive_before_reaching_docker() {
    // An unknown context makes docker unreachable without touching any daemon:
    // exit 3 proves verification passed, exit 1 that it failed.
    let Some((dir, archive)) = volume_archive(b"hello\n") else {
        return;
    };
    bin()
        .args([
            "--docker-context",
            "definitely-not-a-context-xyz",
            "restore-volume",
        ])
        .arg(&archive)
        .assert()
        .code(3);
    assert!(no_scratch_left(dir.path()));

    let Some((dir, archive)) = volume_archive(b"tampered\n") else {
        return;
    };
    bin()
        .args([
            "--docker-context",
            "definitely-not-a-context-xyz",
            "restore-volume",
        ])
        .arg(&archive)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("verification failed"));
    assert!(no_scratch_left(dir.path()));
}
