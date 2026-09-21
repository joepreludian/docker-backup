use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;

fn bin() -> Command {
    Command::cargo_bin("docker-backup").expect("binary builds")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn help_lists_all_commands() {
    bin().arg("--help").assert().success().stdout(
        predicate::str::contains("backup")
            .and(predicate::str::contains("restore"))
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
