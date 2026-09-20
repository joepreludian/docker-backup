# docker-backup

Coordinates the `docker` CLI to back up and restore volumes, images and container
filesystems. The output is a plain folder with a `manifest.json`, so anything it
writes can also be restored by hand.

## Install

    cargo install --path .

Requires `docker`, `tar`, and (for bzip2 options) `bzip2` on PATH.

## Quick start

    docker-backup doctor                                   # daemon, counts, required tools
    docker-backup backup ./my-backup --per-file-bzip2      # named volumes + built images
    docker-backup info ./my-backup                         # manifest + hash check
    docker-backup restore ./my-backup                      # skips volumes that already exist
    docker-backup restore ./my-backup --overwrite          # wipes and refills every listed volume
    docker-backup --json doctor | jq .                     # machine-readable output

## Usage

    docker-backup backup [OUTPUT_DIR] [--all-images] [--include-volatile]
                         [--containers NAME]... [--per-file-bzip2 | --single-archive]
                         [--no-images] [--no-volumes]
    docker-backup restore <BACKUP_DIR | archive.tar.bz2> [--overwrite] [--include-volatile]
                         [--container-tag TAG] [--no-images] [--no-volumes] [--no-containers]
                         [--skip-verify]
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

Manual restore of a volume without this tool:

    docker volume create pgdata
    docker run --rm -i -v pgdata:/data alpine:3 tar -C /data -xf - < volumes/pgdata.tar

## Exit codes

0 ok · 1 per-item failures or verification failed · 2 usage error · 3 docker unavailable

## Development

    cargo test                                                   # unit + CLI tests
    DOCKER_BACKUP_IT=1 cargo test --test docker_integration -- --ignored --test-threads=1   # real daemon
