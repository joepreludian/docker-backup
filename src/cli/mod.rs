//! Command-line surface. Only parsing and conversion to service requests lives here.

pub mod run;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use time::OffsetDateTime;
use time::macros::format_description;

use crate::application::backup::BackupRequest;
use crate::application::restore::RestoreRequest;
use crate::domain::manifest::Compression;
use crate::domain::plan::{BackupScope, RestorePolicy};

pub use run::run;

#[derive(Debug, Parser)]
#[command(
    name = "docker-backup",
    version,
    about = "Back up and restore docker volumes, images and containers through the docker CLI"
)]
pub struct Cli {
    /// Print a single JSON document on stdout instead of tables (progress still goes to stderr).
    #[arg(long, global = true)]
    pub json: bool,

    /// Docker context to use (defaults to the current one).
    #[arg(long, global = true, value_name = "NAME")]
    pub docker_context: Option<String>,

    /// Image used by the helper container that tars volumes.
    #[arg(long, global = true, default_value = "alpine:3", value_name = "IMAGE")]
    pub helper_image: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Export volumes, images and optionally containers into a backup folder.
    Backup(BackupArgs),
    /// Load a backup folder or archive back into the daemon.
    Restore(RestoreArgs),
    /// Show a backup's manifest and verify its files.
    Info(InfoArgs),
    /// Report on the docker daemon and the external tools this program needs.
    Doctor,
}

#[derive(Debug, Args)]
pub struct BackupArgs {
    /// Output folder (default: ./docker-backup-<UTC timestamp>).
    pub output: Option<PathBuf>,
    /// Also export images that can be pulled again (default: locally built only).
    #[arg(long)]
    pub all_images: bool,
    /// Also export anonymous (volatile) volumes.
    #[arg(long)]
    pub include_volatile: bool,
    /// Containers whose filesystem should be exported.
    #[arg(long, num_args = 1.., value_name = "NAME")]
    pub containers: Vec<String>,
    /// Compress each item individually with bzip2.
    #[arg(long, conflicts_with = "single_archive")]
    pub per_file_bzip2: bool,
    /// Pack the whole backup into one <output>.tar.bz2.
    #[arg(long)]
    pub single_archive: bool,
    /// Skip images.
    #[arg(long)]
    pub no_images: bool,
    /// Skip volumes.
    #[arg(long)]
    pub no_volumes: bool,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Backup folder or .tar.bz2 archive.
    pub source: PathBuf,
    /// Replace the contents of volumes that already exist (default: skip them).
    #[arg(long)]
    pub overwrite: bool,
    /// Also restore anonymous (volatile) volumes.
    #[arg(long)]
    pub include_volatile: bool,
    /// Tag given to images imported from container exports (<name>:<tag>).
    #[arg(long, default_value = "restored", value_name = "TAG")]
    pub container_tag: String,
    #[arg(long)]
    pub no_images: bool,
    #[arg(long)]
    pub no_volumes: bool,
    #[arg(long)]
    pub no_containers: bool,
    /// Do not verify file hashes before restoring.
    #[arg(long)]
    pub skip_verify: bool,
}

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Backup folder or .tar.bz2 archive.
    pub source: PathBuf,
}

pub fn default_output_name(now: OffsetDateTime) -> String {
    let stamp = now
        .format(format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .expect("static format");
    format!("docker-backup-{stamp}")
}

impl BackupArgs {
    pub fn to_request(&self, now: OffsetDateTime) -> BackupRequest {
        BackupRequest {
            output: self
                .output
                .clone()
                .unwrap_or_else(|| PathBuf::from(default_output_name(now))),
            scope: BackupScope {
                volumes: !self.no_volumes,
                include_volatile: self.include_volatile,
                images: !self.no_images,
                all_images: self.all_images,
                containers: self.containers.clone(),
            },
            compression: if self.per_file_bzip2 {
                Compression::Bzip2PerFile
            } else {
                Compression::None
            },
            single_archive: self.single_archive,
        }
    }
}

impl RestoreArgs {
    pub fn to_request(&self) -> RestoreRequest {
        RestoreRequest {
            source: self.source.clone(),
            policy: RestorePolicy {
                overwrite: self.overwrite,
                include_volatile: self.include_volatile,
                volumes: !self.no_volumes,
                images: !self.no_images,
                containers: !self.no_containers,
                container_tag: self.container_tag.clone(),
            },
            verify: !self.skip_verify,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use time::macros::datetime;

    #[test]
    fn default_output_name_is_timestamped() {
        assert_eq!(
            default_output_name(datetime!(2026-09-19 14:03:11 UTC)),
            "docker-backup-20260919T140311Z"
        );
    }

    #[test]
    fn backup_args_map_to_request() {
        let cli = Cli::try_parse_from([
            "docker-backup",
            "backup",
            "--all-images",
            "--include-volatile",
            "--containers",
            "web",
            "db",
            "--per-file-bzip2",
            "/tmp/out",
        ])
        .unwrap();
        let Command::Backup(args) = cli.command else {
            panic!("expected backup")
        };
        let request = args.to_request(datetime!(2026-09-19 14:03:11 UTC));
        assert_eq!(request.output, PathBuf::from("/tmp/out"));
        assert!(request.scope.all_images);
        assert!(request.scope.include_volatile);
        assert_eq!(request.scope.containers, vec!["web", "db"]);
        assert_eq!(request.compression, Compression::Bzip2PerFile);
        assert!(!request.single_archive);
    }

    #[test]
    fn backup_defaults() {
        let cli = Cli::try_parse_from(["docker-backup", "backup"]).unwrap();
        let Command::Backup(args) = cli.command else {
            panic!("expected backup")
        };
        let request = args.to_request(datetime!(2026-09-19 14:03:11 UTC));
        assert_eq!(
            request.output,
            PathBuf::from("docker-backup-20260919T140311Z")
        );
        assert!(request.scope.volumes && request.scope.images);
        assert_eq!(request.compression, Compression::None);
    }

    #[test]
    fn bzip2_and_single_archive_conflict() {
        assert!(
            Cli::try_parse_from([
                "docker-backup",
                "backup",
                "--per-file-bzip2",
                "--single-archive"
            ])
            .is_err()
        );
    }

    #[test]
    fn restore_args_map_to_request() {
        let cli = Cli::try_parse_from([
            "docker-backup",
            "--json",
            "restore",
            "--overwrite",
            "--no-images",
            "--container-tag",
            "x",
            "--skip-verify",
            "/b",
        ])
        .unwrap();
        assert!(cli.json);
        let Command::Restore(args) = cli.command else {
            panic!("expected restore")
        };
        let request = args.to_request();
        assert!(request.policy.overwrite);
        assert!(!request.policy.images);
        assert!(request.policy.volumes);
        assert_eq!(request.policy.container_tag, "x");
        assert!(!request.verify);
        assert_eq!(request.source, PathBuf::from("/b"));
    }

    #[test]
    fn global_flags_are_accepted_after_the_subcommand() {
        let cli = Cli::try_parse_from([
            "docker-backup",
            "doctor",
            "--json",
            "--docker-context",
            "colima",
        ])
        .unwrap();
        assert!(cli.json);
        assert_eq!(cli.docker_context.as_deref(), Some("colima"));
        assert_eq!(cli.helper_image, "alpine:3");
    }
}
