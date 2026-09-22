# docker-backup

Coordinates the `docker` CLI to back up and restore volumes, images and container
filesystems. The output is a plain folder with a `manifest.json`, so anything it
writes can also be restored by hand.

**[Read the manual](https://docker-backup.readthedocs.io/)** for installation,
a quickstart, and full guides to backing up and restoring, or visit
[docker-backup.jon.dev.br](https://docker-backup.jon.dev.br/).

## Install

With Homebrew, on macOS or Linux:

    brew install joepreludian/tap/docker-backup

Or into `~/.local/bin`, with no `sudo` and the download checked against the
release's published SHA-256 before anything is written:

    curl -fsSL https://docker-backup.jon.dev.br/install.sh | sh

Or download the archive for your platform from the [latest
release](https://github.com/joepreludian/docker-backup/releases/latest), verify
it, and put the binary on your PATH:

    VERSION=0.2.0
    TARGET=aarch64-apple-darwin   # or x86_64-apple-darwin,
                                  # x86_64-unknown-linux-musl, aarch64-unknown-linux-musl
    BASE=https://github.com/joepreludian/docker-backup/releases/download/v$VERSION

    curl -fLO $BASE/docker-backup-$VERSION-$TARGET.tar.gz
    curl -fLO $BASE/SHA256SUMS
    sha256sum -c --ignore-missing SHA256SUMS      # shasum -a 256 -c --ignore-missing on macOS
    tar -xzf docker-backup-$VERSION-$TARGET.tar.gz
    sudo install docker-backup-$VERSION-$TARGET/docker-backup /usr/local/bin/

Linux builds are statically linked against musl and run on any distribution.
macOS builds are signed with a Developer ID certificate and notarized by Apple.
A ticket cannot be stapled to a bare executable, so a copy downloaded through a
browser does an online Gatekeeper check the first time it runs.

From source instead:

    cargo install --path .

Requires `docker`, `tar`, and (for bzip2 options) `bzip2` on PATH.

## Quick start

    docker-backup doctor                                   # daemon, counts, required tools
    docker-backup backup ./my-backup --per-file-bzip2      # named volumes + built images
    docker-backup info ./my-backup                         # manifest + hash check
    docker-backup restore ./my-backup                      # shows a preview, asks to confirm
    docker-backup restore ./my-backup --overwrite          # wipes and refills every listed volume
    docker-backup --json doctor | jq .                     # machine-readable output
    docker-backup --json restore ./my-backup --yes         # non-interactive, --json requires --yes

## Usage

    docker-backup backup [OUTPUT_DIR] [--all-images] [--include-volatile]
                         [--containers NAME]... [--per-file-bzip2 | --single-archive]
                         [--no-images] [--no-volumes]
    docker-backup restore <BACKUP_DIR | archive.tar.bz2> [--overwrite] [--include-volatile]
                         [--container-tag TAG] [--no-images] [--no-volumes] [--no-containers]
                         [--skip-verify] [--yes | -y] [--force-import-if-arch-mismatch]
    docker-backup info    <BACKUP_DIR | archive.tar.bz2>
    docker-backup doctor

Global flags: `--json` (one JSON document on stdout, progress on stderr),
`--docker-context NAME`, `--helper-image IMAGE` (default `alpine:3`).

By default `backup` exports every named volume and every locally built image.
Anonymous ("volatile") volumes are skipped unless `--include-volatile` is given,
on both backup and restore. Containers are only exported when named with
`--containers`; repeat the flag or pass a comma-separated list
(`--containers web --containers db` or `--containers web,db`). Restoring one
imports it as an image `<name>:restored`, with the name lowercased because
docker rejects uppercase in an image reference.

`backup` refuses to write where a backup already lives: an output folder that
already holds a `manifest.json` (or a partial one), or an existing
`<output>.tar.bz2` for `--single-archive`, is an error (exit 2) instead of an
overwrite.

`restore` verifies every file's recorded SHA-256 hash before touching docker at
all, unless `--skip-verify` is given; if verification fails, nothing is
restored.

Before changing anything, `restore` prints a preview of what it will do and asks
`Proceed? [y/N]`. If the backup was made on a different os/arch than the target
daemon, it also prints an architecture-mismatch warning and asks
`Are you sure? [y/N]`. Pass `--yes` (or `-y`) to answer both prompts
automatically. Without `--yes`, running with stdin that isn't a terminal
(a script, a pipe, or `--json`) aborts with exit code 2 asking for `--yes`, so
`--json` always requires `--yes`.

Images and container filesystems built for a different os/arch than the target
daemon are skipped rather than imported, and reported as failures (exit 1).
Pass `--force-import-if-arch-mismatch` to import them anyway.

If a volume's import fails part-way, the volume can be left half-restored: it
has already been created (and, with `--overwrite`, emptied) before the data is
streamed in, so re-run the restore with `--overwrite` to refill it from the
backup.

A `.tar.bz2` backup is unpacked into a temp directory next to the archive, so
restoring or inspecting one needs roughly twice the archive's size free on that
filesystem.

**Warning:** `--overwrite` wipes and refills the contents of **every** volume
in the manifest that already exists on the host, not just one you care about.
Without `--overwrite`, restore only ever creates volumes that don't already
exist and skips the rest; with it, every existing volume named in the backup
is emptied and reloaded from the archive. Double-check what a backup contains
(`docker-backup info`) before restoring it with `--overwrite`.

## Backup layout

    <dir>/manifest.json
    <dir>/volumes/<name>.tar[.bz2]
    <dir>/images/<ref>.tar[.bz2]
    <dir>/containers/<name>.tar[.bz2]

Every file has its SHA-256 recorded in the manifest; `info` and `restore` verify them.
The manifest also records the backing daemon's os/arch and each image's platform, which
`restore` uses to detect an architecture mismatch against the target daemon.

Manual restore of a volume without this tool:

    docker volume create pgdata
    docker run --rm -i -v pgdata:/data alpine:3 tar -C /data -xf - < volumes/pgdata.tar

## Exit codes

0 ok · 1 per-item failures or verification failed · 2 usage error or restore aborted at a prompt · 3 docker unavailable

## Development

    cargo test                                                   # unit + CLI tests
    DOCKER_BACKUP_IT=1 cargo test --test docker_integration -- --ignored --test-threads=1   # real daemon

Every push and pull request runs formatting, clippy, the test suite on Linux and
macOS, and the integration tests against a real daemon. Pushing a `v*` tag builds
the four release targets, signs and notarizes the macOS binaries, and publishes a
GitHub Release. The release workflow can also be started by hand to build and
sign without publishing anything.

## License

GPLv3 or later. See [LICENSE](LICENSE) for the full text.

    docker-backup, copyright (C) 2026 Jon Trigueiro
    This program comes with ABSOLUTELY NO WARRANTY.
    This is free software, and you are welcome to redistribute it
    under the conditions of the GNU General Public License version 3.
