//! Opt-in tests against a real docker daemon.
//!
//! `backup` has no `--only`/`--exclude` flag: a plain run exports *every*
//! named volume on the host running these tests, not just the throwaway
//! `dbk-it-*` volume each test creates for itself. On a machine with real
//! data this can be many gigabytes, and some of those volumes may belong to
//! containers that are currently running.
//!
//! Because of that, no test here ever restores a host-wide backup as-is:
//!
//! - `run_roundtrip` (used by the plain and per-file-bzip2 variants) calls
//!   `prune_to_volume` to rewrite the backup's manifest down to just the one
//!   volume the test owns - and deletes the other volumes' tar files - before
//!   any `restore` runs. Every restore is then asserted, from its `--json`
//!   output, to have touched exactly that one volume.
//! - `volume_roundtrip_single_archive` restores directly from the untouched
//!   archive (an archive can't be pruned in place), but never passes
//!   `--overwrite`. Without `--overwrite`, restore can only ever create
//!   volumes that don't already exist (our test volume) and skips every
//!   volume that does - the JSON assertions confirm exactly one item was
//!   restored, the rest were skipped, and none failed.
//!
//! `volume_roundtrip_per_file_bzip2` and `volume_roundtrip_single_archive`
//! compress every volume on the host (per-file or as one combined archive)
//! and can be slow / large on a host with many or large volumes.
//!
//! `single_volume_roundtrip` names only its own `dbk-it-*` volumes:
//! `backup-volume` exports just that one, and every `restore-volume` targets it
//! (or a `-copy` of it) by name, so it touches nothing else on the daemon.
//!
//! `compose_roundtrip_into_a_renamed_project` builds its own `dbk-it-*` compose
//! project: backup and restore both run with `--from-docker-compose`, so they
//! touch only that project's volume and image, and it never passes `--overwrite`.
//!
//! Run with: DOCKER_BACKUP_IT=1 cargo test --test docker_integration -- --ignored --test-threads=1

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::Command as Bin;
use serde_json::Value;

fn enabled() -> bool {
    if std::env::var_os("DOCKER_BACKUP_IT").is_none() {
        eprintln!("DOCKER_BACKUP_IT not set, skipping");
        return false;
    }
    true
}

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker")
        .args(args)
        .output()
        .expect("docker runs");
    assert!(
        output.status.success(),
        "docker {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Removes the test volume on drop so a failing assertion never leaks it.
struct TestVolume(String);

impl TestVolume {
    fn create(content: &str) -> Self {
        let name = format!(
            "dbk-it-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        docker(&["volume", "create", &name]);
        docker(&[
            "run",
            "--rm",
            "-v",
            &format!("{name}:/data"),
            "alpine:3",
            "sh",
            "-c",
            &format!("printf '{content}' > /data/hello.txt"),
        ]);
        Self(name)
    }

    fn read(&self) -> String {
        docker(&[
            "run",
            "--rm",
            "-v",
            &format!("{}:/data:ro", self.0),
            "alpine:3",
            "cat",
            "/data/hello.txt",
        ])
    }

    fn remove(&self) {
        let _ = Command::new("docker")
            .args(["volume", "rm", "-f", &self.0])
            .output();
    }
}

impl Drop for TestVolume {
    fn drop(&mut self) {
        self.remove();
    }
}

/// SAFETY GUARD: rewrites `manifest.json` in `backup_dir` so `volumes`
/// contains only the entry named `volume`, and deletes the tar files of every
/// dropped entry. `images`/`containers` are left as-is. After this call,
/// `restore` on `backup_dir` can only ever touch the one volume under test.
fn prune_to_volume(backup_dir: &Path, volume: &str) {
    let manifest_path = backup_dir.join("manifest.json");
    let text = fs::read_to_string(&manifest_path).expect("read manifest.json");
    let mut manifest: Value = serde_json::from_str(&text).expect("parse manifest.json");

    let volumes = manifest["volumes"]
        .as_array()
        .cloned()
        .expect("manifest has a volumes array");
    let (keep, drop): (Vec<Value>, Vec<Value>) = volumes
        .into_iter()
        .partition(|entry| entry["name"].as_str() == Some(volume));
    assert_eq!(
        keep.len(),
        1,
        "expected exactly one manifest entry named {volume}, found {}",
        keep.len()
    );

    for entry in &drop {
        let file = entry["file"]
            .as_str()
            .expect("dropped volume entry has a file");
        let path = backup_dir.join(file);
        fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("remove pruned volume file {}: {e}", path.display()));
    }

    manifest["volumes"] = Value::Array(keep);
    let pretty = serde_json::to_string_pretty(&manifest).expect("serialize pruned manifest");
    fs::write(&manifest_path, pretty).expect("write pruned manifest.json");
}

/// Runs `docker-backup <args> <source>`, asserts it exits successfully, and
/// returns the `items` array from its `--json` report.
fn restore_json(source: &Path, args: &[&str]) -> Vec<Value> {
    let output = Bin::cargo_bin("docker-backup")
        .unwrap()
        .args(args)
        .arg(source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "docker-backup {:?} {} failed: {}",
        args,
        source.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("parse restore json");
    report["items"]
        .as_array()
        .cloned()
        .expect("report has an items array")
}

/// Runs `docker-backup <args> <path>`, asserts it exits successfully, and
/// returns its `--json` report.
fn json_report(args: &[&str], path: &Path) -> Value {
    let output = Bin::cargo_bin("docker-backup")
        .unwrap()
        .args(args)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "docker-backup {:?} {} failed: {}",
        args,
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse json report")
}

/// Backs up (with `extra_backup_args`, e.g. `--per-file-bzip2`), prunes the
/// manifest down to the test's own volume, then round-trips restore's
/// skip/overwrite behavior. Every restore's JSON output is checked to have
/// touched exactly the test volume and nothing else.
fn run_roundtrip(extra_backup_args: &[&str]) {
    let volume = TestVolume::create("hello from backup");
    let dir = tempfile::tempdir().unwrap();
    let out: PathBuf = dir.path().join("b");

    let mut backup = Bin::cargo_bin("docker-backup").unwrap();
    backup
        .args(["backup", "--no-images"])
        .args(extra_backup_args)
        .arg(&out);
    backup.assert().success();

    assert!(out.exists(), "backup output missing");
    assert!(
        fs::read_to_string(out.join("manifest.json"))
            .unwrap()
            .contains(&volume.0)
    );

    // `info` on the full, host-wide folder must succeed.
    Bin::cargo_bin("docker-backup")
        .unwrap()
        .arg("info")
        .arg(&out)
        .assert()
        .success();

    // SAFETY: drop every volume but our own before any restore runs.
    prune_to_volume(&out, &volume.0);

    // `info` again proves the pruned manifest/files are still self-consistent.
    Bin::cargo_bin("docker-backup")
        .unwrap()
        .arg("info")
        .arg(&out)
        .assert()
        .success();

    volume.remove();

    let items = restore_json(&out, &["--json", "restore", "--yes"]);
    assert_eq!(
        items.len(),
        1,
        "pruned backup should restore a single item, got {items:?}"
    );
    assert_eq!(items[0]["status"], "restored");
    assert_eq!(items[0]["name"], volume.0);
    assert_eq!(volume.read(), "hello from backup");

    // Second restore without --overwrite must skip the now-existing volume.
    docker(&[
        "run",
        "--rm",
        "-v",
        &format!("{}:/data", volume.0),
        "alpine:3",
        "sh",
        "-c",
        "printf changed > /data/hello.txt",
    ]);
    let items = restore_json(&out, &["--json", "restore", "--yes"]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "skipped_existing");
    assert_eq!(items[0]["name"], volume.0);
    assert_eq!(volume.read(), "changed");

    // With --overwrite it must replace it.
    let items = restore_json(&out, &["--json", "restore", "--overwrite", "--yes"]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "restored");
    assert_eq!(items[0]["name"], volume.0);
    assert_eq!(volume.read(), "hello from backup");
}

#[test]
#[ignore]
fn volume_roundtrip_plain() {
    if !enabled() {
        return;
    }
    run_roundtrip(&[]);
}

#[test]
#[ignore]
fn volume_roundtrip_per_file_bzip2() {
    if !enabled() {
        return;
    }
    run_roundtrip(&["--per-file-bzip2"]);
}

#[test]
#[ignore]
fn volume_roundtrip_single_archive() {
    if !enabled() {
        return;
    }
    let volume = TestVolume::create("hello from backup");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("b");

    Bin::cargo_bin("docker-backup")
        .unwrap()
        .args(["backup", "--no-images", "--single-archive"])
        .arg(&out)
        .assert()
        .success();

    let archive = dir.path().join("b.tar.bz2");
    assert!(archive.exists(), "archive missing");

    // `info` on the archive (unpacked internally, never mutated) must succeed.
    Bin::cargo_bin("docker-backup")
        .unwrap()
        .arg("info")
        .arg(&archive)
        .assert()
        .success();

    volume.remove();

    // SAFETY: never pass --overwrite against a host-wide archive. Without it,
    // restore only ever creates volumes that don't already exist (our own)
    // and skips every volume that does.
    let items = restore_json(&archive, &["--json", "restore", "--yes"]);

    let mut restored = 0;
    for item in &items {
        let status = item["status"].as_str().unwrap_or_default();
        assert_ne!(status, "failed", "item failed: {item}");
        if status == "restored" {
            restored += 1;
            assert_eq!(item["name"], volume.0);
        } else {
            assert_eq!(status, "skipped_existing", "unexpected status for {item}");
        }
    }
    assert_eq!(
        restored, 1,
        "expected exactly the test volume to be restored, got {items:?}"
    );
    assert_eq!(volume.read(), "hello from backup");
}

#[test]
#[ignore]
fn doctor_reports_a_live_daemon() {
    if !enabled() {
        return;
    }
    let output = Bin::cargo_bin("docker-backup")
        .unwrap()
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["docker"]["server_version"].as_str().unwrap().len() > 2);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
#[ignore]
fn single_volume_roundtrip() {
    if !enabled() {
        return;
    }
    let volume = TestVolume::create("hello from backup-volume");
    let dir = tempfile::tempdir().unwrap();

    let report = json_report(&["--json", "backup-volume", &volume.0, "-o"], dir.path());
    let items = report["items"].as_array().expect("report has items");
    assert_eq!(items.len(), 1, "one archive per volume, got {items:?}");
    assert_eq!(items[0]["status"], "done");
    let archive = PathBuf::from(items[0]["file"].as_str().expect("archive path"));
    let file_name = archive.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        file_name.starts_with(&format!("{}-", volume.0)),
        "{file_name}"
    );
    assert!(file_name.ends_with(".tar.bz2"), "{file_name}");

    let info = json_report(&["--json", "info"], &archive);
    assert_eq!(info["type"], "volume");
    assert_eq!(info["manifest"]["volume"], volume.0);
    assert_eq!(info["verification"]["files"][0]["status"], "ok");

    // Into a volume that does not exist: no prompt, so --json needs no --yes.
    volume.remove();
    let restored = json_report(&["--json", "restore-volume"], &archive);
    assert_eq!(restored["status"], "done");
    assert_eq!(restored["action"], "create");
    assert_eq!(restored["target"], volume.0);
    assert_eq!(volume.read(), "hello from backup-volume");

    // Under another name; the guard removes it even if an assertion fails.
    let copy = TestVolume(format!("{}-copy", volume.0));
    let restored = json_report(&["--json", "restore-volume", "--as", &copy.0], &archive);
    assert_eq!(restored["volume"], volume.0);
    assert_eq!(restored["target"], copy.0);
    assert_eq!(copy.read(), "hello from backup-volume");

    // The volume exists now: without --overwrite that is a conflict.
    Bin::cargo_bin("docker-backup")
        .unwrap()
        .arg("restore-volume")
        .arg(&archive)
        .assert()
        .code(2);

    // With --overwrite it is emptied, then refilled: the stray file goes too.
    docker(&[
        "run",
        "--rm",
        "-v",
        &format!("{}:/data", volume.0),
        "alpine:3",
        "sh",
        "-c",
        "printf changed > /data/hello.txt && touch /data/stray",
    ]);
    let restored = json_report(
        &["--json", "restore-volume", "--overwrite", "--yes"],
        &archive,
    );
    assert_eq!(restored["action"], "overwrite");
    assert_eq!(volume.read(), "hello from backup-volume");
    let listing = docker(&[
        "run",
        "--rm",
        "-v",
        &format!("{}:/data:ro", volume.0),
        "alpine:3",
        "ls",
        "-A",
        "/data",
    ]);
    assert_eq!(listing, "hello.txt");
}

/// A throwaway compose project in `<parent>/<name>`: one service built from a
/// one-line Dockerfile, one named volume. Dropping it removes the project's
/// network, its `<name>_data` volume and its `<name>-app:latest` tag by name —
/// `compose down` alone may skip a volume restored without compose's labels.
struct TestProject {
    name: String,
    dir: PathBuf,
}

impl TestProject {
    fn create(parent: &Path, name: &str) -> Self {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Dockerfile"), "FROM alpine:3\n").unwrap();
        fs::write(
            dir.join("docker-compose.yml"),
            "services:\n  app:\n    build: .\n    volumes:\n      - data:/data\nvolumes:\n  data:\n",
        )
        .unwrap();
        Self {
            name: name.to_string(),
            dir,
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("docker-compose.yml")
    }

    fn compose(&self, args: &[&str]) -> String {
        let file = self.file();
        let mut all = vec!["compose", "-f", file.to_str().unwrap()];
        all.extend_from_slice(args);
        docker(&all)
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let file = self.file();
        let _ = Command::new("docker")
            .args(["compose", "-f", file.to_str().unwrap(), "down", "--volumes"])
            .output();
        let _ = Command::new("docker")
            .args(["volume", "rm", "-f", &format!("{}_data", self.name)])
            .output();
        let _ = Command::new("docker")
            .args(["image", "rm", "-f", &format!("{}-app:latest", self.name)])
            .output();
    }
}

#[test]
#[ignore]
fn compose_roundtrip_into_a_renamed_project() {
    if !enabled() {
        return;
    }
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    // The folder name is the project name, so the copy is a renamed project.
    let (name, renamed) = (format!("dbk-it-{stamp}"), format!("dbk-it-{stamp}-2"));
    let dir = tempfile::tempdir().unwrap();
    let original = TestProject::create(dir.path(), &name);
    original.compose(&["build"]);
    original.compose(&[
        "run",
        "--rm",
        "-T",
        "app",
        "sh",
        "-c",
        "printf 'hello from compose' > /data/hello.txt",
    ]);

    let out = dir.path().join("backup");
    let report = json_report(
        &[
            "--json",
            "backup",
            "--from-docker-compose",
            original.file().to_str().unwrap(),
        ],
        &out,
    );
    let items = report["items"].as_array().expect("report has items");
    let names: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![format!("{name}_data"), format!("{name}-app:latest")],
        "only the project's volume and built image"
    );
    assert!(items.iter().all(|i| i["status"] == "done"), "{items:?}");
    assert_eq!(report["compose"]["project"], name);

    let info = json_report(&["--json", "info"], &out);
    assert_eq!(info["manifest"]["compose"]["project"], name);
    assert_eq!(info["manifest"]["volumes"][0]["compose"]["key"], "data");

    let copy = TestProject::create(dir.path(), &renamed);
    let restored = json_report(
        &[
            "--json",
            "restore",
            "--yes",
            "--from-docker-compose",
            copy.file().to_str().unwrap(),
        ],
        &out,
    );
    let items = restored["items"].as_array().expect("report has items");
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["name"], format!("{name}_data"));
    assert_eq!(items[0]["target"], format!("{renamed}_data"));
    assert!(items.iter().all(|i| i["status"] == "restored"), "{items:?}");

    let read = docker(&[
        "run",
        "--rm",
        "-v",
        &format!("{renamed}_data:/data:ro"),
        "alpine:3",
        "cat",
        "/data/hello.txt",
    ]);
    assert_eq!(read, "hello from compose");
    docker(&[
        "image",
        "inspect",
        "--format",
        "{{.Id}}",
        &format!("{renamed}-app:latest"),
    ]);
}
