//! `DockerPort` implemented by shelling out to the `docker` CLI.

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::thread;

use serde_json::Value;

use crate::application::ports::DockerPort;
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::DockerInfo;
use crate::domain::refs::{ContainerRef, ImageRef, VolumeRef};

/// Empties `/data` inside the helper container, dotfiles included.
pub const WIPE_SCRIPT: &str = "rm -rf /data/* /data/.[!.]* /data/..?* 2>/dev/null";
const JSON_FORMAT: &str = "{{json .}}";

pub struct DockerCli {
    binary: String,
    context: Option<String>,
    helper_image: String,
}

impl DockerCli {
    pub fn new(context: Option<String>, helper_image: String) -> Self {
        Self::with_binary("docker", context, helper_image)
    }

    pub fn with_binary(
        binary: impl Into<String>,
        context: Option<String>,
        helper_image: String,
    ) -> Self {
        Self {
            binary: binary.into(),
            context,
            helper_image,
        }
    }

    pub fn base_args(&self) -> Vec<String> {
        match &self.context {
            Some(context) => vec!["--context".to_string(), context.clone()],
            None => Vec::new(),
        }
    }

    pub fn volume_export_args(&self, name: &str) -> Vec<String> {
        [
            "run",
            "--rm",
            "-i",
            "-v",
            &format!("{name}:/data:ro"),
            &self.helper_image,
            "tar",
            "-C",
            "/data",
            "-cf",
            "-",
            ".",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    pub fn volume_import_args(&self, name: &str, wipe: bool) -> Vec<String> {
        let script = if wipe {
            format!("{WIPE_SCRIPT}; tar -C /data -xf -")
        } else {
            "tar -C /data -xf -".to_string()
        };
        [
            "run",
            "--rm",
            "-i",
            "-v",
            &format!("{name}:/data"),
            &self.helper_image,
            "sh",
            "-c",
            &script,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.binary);
        command.args(self.base_args()).args(args);
        command
    }

    fn describe(&self, args: &[String]) -> String {
        format!("{} {}", self.binary, args.join(" "))
    }

    /// Run to completion and return stdout; non-zero exit becomes `DockerCommandFailed`.
    fn capture(&self, args: &[&str]) -> AppResult<String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let output = self
            .command(&args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| AppError::DockerUnavailable(format!("cannot run {}: {e}", self.binary)))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(AppError::DockerCommandFailed {
                command: self.describe(&args),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    fn capture_json(&self, args: &[&str]) -> AppResult<Value> {
        let text = self.capture(args)?;
        serde_json::from_str(&text).map_err(|e| AppError::DockerCommandFailed {
            command: self.describe(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
            stderr: format!("invalid json: {e}"),
        })
    }

    /// Stream the command's stdout into `sink`.
    fn stream_out(&self, args: Vec<String>, sink: &mut dyn Write) -> AppResult<()> {
        let mut child = self
            .command(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AppError::DockerUnavailable(format!("cannot run {}: {e}", self.binary)))?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stderr_reader = thread::spawn(move || read_all(stderr));
        let copied = io::copy(&mut stdout, sink);
        // Close our end of the pipe before waiting: a child still writing into a
        // full pipe nobody reads would never exit.
        drop(stdout);
        if copied.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        let stderr_text = stderr_reader.join().expect("stderr thread panicked");
        // Our own write failure explains the (now expected) non-zero exit.
        copied?;
        if !status.success() {
            return Err(AppError::DockerCommandFailed {
                command: self.describe(&args),
                stderr: stderr_text,
            });
        }
        Ok(())
    }

    /// Stream `source` into the command's stdin.
    fn stream_in(&self, args: Vec<String>, source: &mut dyn Read) -> AppResult<()> {
        let mut child = self
            .command(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AppError::DockerUnavailable(format!("cannot run {}: {e}", self.binary)))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stderr = child.stderr.take().expect("piped stderr");
        let stderr_reader = thread::spawn(move || read_all(stderr));
        let copied = io::copy(source, &mut stdin);
        // Close the pipe before waiting; kill the child when we cannot feed it any more.
        drop(stdin);
        if copied.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        let stderr_text = stderr_reader.join().expect("stderr thread panicked");
        let child_failed = !status.success();
        let failure = || AppError::DockerCommandFailed {
            command: self.describe(&args),
            stderr: stderr_text.clone(),
        };
        // A child that failed on its own says why; otherwise our copy error is the cause.
        if child_failed && !stderr_text.is_empty() {
            return Err(failure());
        }
        copied?;
        if child_failed {
            return Err(failure());
        }
        Ok(())
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }
}

fn read_all(mut reader: impl Read) -> String {
    let mut text = String::new();
    let _ = reader.read_to_string(&mut text);
    text.trim().to_string()
}

pub fn parse_version(json: &str) -> AppResult<DockerInfo> {
    let value: Value = serde_json::from_str(json).map_err(|e| {
        AppError::DockerUnavailable(format!("invalid `docker version` output: {e}"))
    })?;
    let server = value
        .get("Server")
        .filter(|s| !s.is_null())
        .ok_or_else(|| AppError::DockerUnavailable("docker daemon did not answer".into()))?;
    let text = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Ok(DockerInfo {
        server_version: text(server, "Version"),
        client_version: value
            .get("Client")
            .map(|c| text(c, "Version"))
            .unwrap_or_default(),
        host: String::new(),
        context: String::new(),
        os: text(server, "Os"),
        arch: text(server, "Arch"),
    })
}

pub fn parse_context_host(json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(json).ok()?;
    let object = match &value {
        Value::Array(items) => items.first()?,
        other => other,
    };
    object
        .pointer("/Endpoints/docker/Host")?
        .as_str()
        .map(String::from)
}

fn json_lines(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
}

pub fn parse_volume_lines(text: &str) -> Vec<VolumeRef> {
    json_lines(text)
        .filter_map(|v| {
            let name = v.get("Name")?.as_str()?.to_string();
            let labels = v.get("Labels").and_then(Value::as_str).unwrap_or_default();
            Some(VolumeRef::new(name, labels))
        })
        .collect()
}

pub fn parse_container_lines(text: &str) -> Vec<ContainerRef> {
    json_lines(text)
        .map(|v| {
            let text = |key: &str| {
                v.get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let name = text("Names")
                .split(',')
                .next()
                .unwrap_or_default()
                .to_string();
            ContainerRef {
                id: text("ID"),
                name,
                image: text("Image"),
                running: text("State") == "running",
            }
        })
        .collect()
}

pub fn unique_ids(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| seen.insert(l.to_string()))
        .map(String::from)
        .collect()
}

impl DockerPort for DockerCli {
    fn engine_info(&self) -> AppResult<DockerInfo> {
        let raw = match self.capture(&["version", "--format", JSON_FORMAT]) {
            Ok(raw) => raw,
            Err(AppError::DockerCommandFailed { stderr, .. }) => {
                return Err(AppError::DockerUnavailable(stderr));
            }
            Err(other) => return Err(other),
        };
        let mut info = parse_version(&raw)?;
        info.context = match &self.context {
            Some(context) => context.clone(),
            None => self
                .capture(&["context", "show"])
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        };
        info.host = self
            .capture(&["context", "inspect", "--format", JSON_FORMAT, &info.context])
            .ok()
            .and_then(|json| parse_context_host(&json))
            .unwrap_or_default();
        Ok(info)
    }

    fn list_volumes(&self) -> AppResult<Vec<VolumeRef>> {
        Ok(parse_volume_lines(&self.capture(&[
            "volume",
            "ls",
            "--format",
            JSON_FORMAT,
        ])?))
    }

    fn inspect_volume(&self, name: &str) -> AppResult<Value> {
        self.capture_json(&["volume", "inspect", "--format", JSON_FORMAT, name])
    }

    fn list_images(&self) -> AppResult<Vec<ImageRef>> {
        let ids = unique_ids(&self.capture(&["image", "ls", "-q", "--no-trunc"])?);
        let mut images = Vec::with_capacity(ids.len());
        for id in ids {
            let inspect = self.inspect_image(&id)?;
            if let Some(image) = ImageRef::from_inspect(&inspect) {
                images.push(image);
            }
        }
        Ok(images)
    }

    fn inspect_image(&self, id: &str) -> AppResult<Value> {
        self.capture_json(&["image", "inspect", "--format", JSON_FORMAT, id])
    }

    fn list_containers(&self) -> AppResult<Vec<ContainerRef>> {
        Ok(parse_container_lines(&self.capture(&[
            "container",
            "ls",
            "-a",
            "--format",
            JSON_FORMAT,
        ])?))
    }

    fn inspect_container(&self, name: &str) -> AppResult<Value> {
        self.capture_json(&["container", "inspect", "--format", JSON_FORMAT, name])
    }

    fn ensure_helper_image(&self) -> AppResult<()> {
        if self
            .capture(&[
                "image",
                "inspect",
                "--format",
                "{{.Id}}",
                &self.helper_image,
            ])
            .is_ok()
        {
            return Ok(());
        }
        self.capture(&["pull", "--quiet", &self.helper_image])
            .map(|_| ())
    }

    fn export_volume(&self, name: &str, sink: &mut dyn Write) -> AppResult<()> {
        self.stream_out(self.volume_export_args(name), sink)
    }

    fn save_image(&self, reference: &str, sink: &mut dyn Write) -> AppResult<()> {
        self.stream_out(Self::strings(&["save", reference]), sink)
    }

    fn export_container(&self, name: &str, sink: &mut dyn Write) -> AppResult<()> {
        self.stream_out(Self::strings(&["export", name]), sink)
    }

    fn create_volume(&self, name: &str) -> AppResult<()> {
        self.capture(&["volume", "create", name]).map(|_| ())
    }

    fn import_volume(&self, name: &str, source: &mut dyn Read, wipe: bool) -> AppResult<()> {
        self.stream_in(self.volume_import_args(name, wipe), source)
    }

    fn load_image(&self, source: &mut dyn Read) -> AppResult<()> {
        self.stream_in(Self::strings(&["load"]), source)
    }

    fn import_container_fs(&self, source: &mut dyn Read, tag: &str) -> AppResult<()> {
        self.stream_in(Self::strings(&["import", "-", tag]), source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli() -> DockerCli {
        DockerCli::new(None, "alpine:3".into())
    }

    #[test]
    fn base_args_include_context_when_set() {
        assert!(cli().base_args().is_empty());
        assert_eq!(
            DockerCli::new(Some("colima".into()), "alpine:3".into()).base_args(),
            vec!["--context", "colima"]
        );
    }

    #[test]
    fn volume_export_args_use_read_only_helper() {
        assert_eq!(
            cli().volume_export_args("pgdata"),
            vec![
                "run",
                "--rm",
                "-i",
                "-v",
                "pgdata:/data:ro",
                "alpine:3",
                "tar",
                "-C",
                "/data",
                "-cf",
                "-",
                "."
            ]
        );
    }

    #[test]
    fn volume_import_args_wipe_only_when_asked() {
        let plain = cli().volume_import_args("pgdata", false);
        assert_eq!(
            plain[..6],
            ["run", "--rm", "-i", "-v", "pgdata:/data", "alpine:3"]
        );
        assert_eq!(plain[6..], ["sh", "-c", "tar -C /data -xf -"]);
        let wipe = cli().volume_import_args("pgdata", true);
        assert_eq!(wipe[8], format!("{WIPE_SCRIPT}; tar -C /data -xf -"));
    }

    #[test]
    fn parses_version_json() {
        let info = parse_version(include_str!("../../tests/fixtures/docker/version.json")).unwrap();
        assert_eq!(info.server_version, "29.5.2");
        assert_eq!(info.client_version, "29.8.1");
        assert_eq!(info.os, "linux");
        assert_eq!(info.arch, "arm64");
    }

    #[test]
    fn version_without_server_is_unavailable() {
        let err = parse_version(r#"{"Client":{"Version":"29.8.1"}}"#).unwrap_err();
        assert!(matches!(err, AppError::DockerUnavailable(_)));
    }

    #[test]
    fn parses_context_host_from_object_or_array() {
        let json = include_str!("../../tests/fixtures/docker/context.json");
        assert_eq!(
            parse_context_host(json).as_deref(),
            Some("unix:///Users/example/.colima/default/docker.sock")
        );
        assert_eq!(
            parse_context_host(&format!("[{json}]")).as_deref(),
            Some("unix:///Users/example/.colima/default/docker.sock")
        );
        assert_eq!(parse_context_host("{}"), None);
    }

    #[test]
    fn parses_volume_lines_with_volatile_flag() {
        let volumes =
            parse_volume_lines(include_str!("../../tests/fixtures/docker/volume_ls.jsonl"));
        assert_eq!(volumes.len(), 3);
        assert!(volumes[0].volatile);
        assert_eq!(volumes[1].name, "pgdata");
        assert!(!volumes[1].volatile);
        assert!(!volumes[2].volatile);
    }

    #[test]
    fn parses_container_lines() {
        let containers = parse_container_lines(include_str!(
            "../../tests/fixtures/docker/container_ls.jsonl"
        ));
        assert_eq!(containers[0].name, "site-serve-1");
        assert_eq!(containers[0].id, "102fd657755e");
        assert!(!containers[0].running);
        assert_eq!(containers[1].image, "example-app:latest");
        assert!(containers[1].running);
    }

    #[test]
    fn unique_ids_preserve_order_and_dedupe() {
        assert_eq!(
            unique_ids("sha256:a\nsha256:b\nsha256:a\n\n"),
            vec!["sha256:a", "sha256:b"]
        );
    }

    /// A throwaway executable script standing in for the `docker` binary.
    #[cfg(unix)]
    fn fake_docker(body: &str) -> tempfile::TempPath {
        use std::os::unix::fs::PermissionsExt;
        let mut file = tempfile::Builder::new()
            .prefix("fake-docker-")
            .suffix(".sh")
            .tempfile()
            .expect("temp script");
        writeln!(file, "#!/bin/sh\n{body}").expect("write script");
        file.flush().expect("flush script");
        let path = file.into_temp_path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[cfg(unix)]
    struct FailingSink {
        seen: usize,
    }

    #[cfg(unix)]
    impl Write for FailingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.seen += 1;
            if self.seen >= 2 {
                return Err(io::Error::other("disk full"));
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_sink_aborts_the_stream_instead_of_hanging() {
        use std::sync::mpsc;
        use std::time::Duration;

        let script = fake_docker("head -c 10000000 /dev/zero");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let cli = DockerCli::with_binary(
                script.to_string_lossy().into_owned(),
                None,
                "alpine:3".into(),
            );
            let mut sink = FailingSink { seen: 0 };
            let failed = cli.save_image("x", &mut sink).is_err();
            let _ = sender.send(failed);
            drop(script);
        });
        match receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(failed) => assert!(failed, "a sink error must surface as an error"),
            Err(_) => panic!("save_image hung after the sink failed"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_zero_exit_reports_the_command_stderr() {
        let script = fake_docker("echo 'no such image: x' >&2; exit 1");
        let cli = DockerCli::with_binary(
            script.to_string_lossy().into_owned(),
            None,
            "alpine:3".into(),
        );
        let err = cli.save_image("x", &mut Vec::new()).unwrap_err();
        match err {
            AppError::DockerCommandFailed { stderr, .. } => {
                assert!(stderr.contains("no such image: x"), "stderr was {stderr:?}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn missing_binary_is_docker_unavailable() {
        let cli = DockerCli::with_binary("definitely-not-docker-xyz", None, "alpine:3".into());
        assert!(matches!(
            cli.engine_info(),
            Err(AppError::DockerUnavailable(_))
        ));
    }
}
