# Single volumes

A full [`backup`](backup.md) is the whole daemon: every named volume and every
locally built image, in one folder. When you want one volume — "just give me
`pgdata`" — use `backup-volume` and `restore-volume` instead. Each volume
becomes one small, self-describing archive that you can copy, check and restore
on its own, under its own name or another.

```text
docker-backup backup-volume <NAME>... [-o, --output-dir DIR]
docker-backup restore-volume <FILE> [--as NAME] [--overwrite] [-y, --yes]
docker-backup info <FILE | DIR>
```

The global flags `--json`, `--docker-context` and `--helper-image` work with
these commands as they do everywhere else.

## Back up a volume

```bash
docker-backup backup-volume pgdata
```

This writes `./pgdata-20260926T141500Z.tar.bz2`: the volume's name, a dash, and
the UTC time of the run. Name several volumes to get one archive each; every
archive from one run carries the same timestamp.

```bash
docker-backup backup-volume pgdata redis-data -o ~/Backups/docker
```

`-o` (`--output-dir`) picks the directory for the archives. It defaults to the
current directory and is created if it does not exist.

Before anything is written, `backup-volume` checks three things: the daemon is
reachable, every named volume exists, and none of the archives it is about to
write is already there. If any check fails, the run stops and writes nothing:

```text
error: volume not found: pgdta
error: ./pgdata-20260926T141500Z.tar.bz2 already exists; choose another output directory
```

It also refuses two names that differ only in case, such as `Data` and `data`.
On a case-insensitive filesystem, macOS's default, their archives would be one
file, so back them up to separate directories.

Once the run is under way, a volume whose export fails is reported as failed
and leaves no archive behind. The other volumes are still backed up, and the run
exits `1`.

Each volume is first written uncompressed into a scratch directory inside the
output directory, then compressed into its archive. So that filesystem briefly
needs room for the volume's contents on top of the archive.

!!! warning "Stop the stack first"

    Nothing is stopped or paused. A database writing to its volume while that
    volume is being read can produce an archive that passes every check and
    still will not start. Stop whatever uses the volume first:

    ```bash
    docker compose stop
    docker-backup backup-volume pgdata
    docker compose start
    ```

## What an archive holds

An archive holds one folder, named like the archive itself, so extracting it by
hand gives you a folder rather than loose files:

```text
pgdata-20260926T141500Z/
├── backup.tar      # the volume's contents: tar -C /data -cf - .
└── volume.json
```

`backup.tar` is stored uncompressed inside the archive; the `.tar.bz2` around it
does the compressing. `volume.json` describes it:

```json
{
  "schema_version": 1,
  "volume": "pgdata",
  "created_at": "2026-09-26T14:15:00Z",
  "size_bytes": 12893184,
  "sha256": "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03",
  "tool": { "name": "docker-backup", "version": "0.3.0" }
}
```

`size_bytes` and `sha256` describe `backup.tar`. The file is called
`volume.json`, not `manifest.json`, so a single-volume archive is never mistaken
for a full backup. `restore` refuses one and points you at `restore-volume`, and
`restore-volume` does the same the other way round.

## Check an archive

```bash
docker-backup info ./pgdata-20260926T141500Z.tar.bz2
```

`info` reads `volume.json` and hashes `backup.tar` against it:

```text
┌────────────┬──────────────────────────────────────────────────────────────────┐
│ Field      ┆ Value                                                            │
╞════════════╪══════════════════════════════════════════════════════════════════╡
│ Source     ┆ ./pgdata-20260926T141500Z.tar.bz2                                │
│ Type       ┆ single volume                                                    │
│ Volume     ┆ pgdata                                                           │
│ Created at ┆ 2026-09-26T14:15:00Z                                             │
│ Tool       ┆ docker-backup 0.3.0                                              │
│ Size       ┆ 12 MiB                                                           │
│ SHA-256    ┆ 5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03 │
└────────────┴──────────────────────────────────────────────────────────────────┘
┌────────┬────────┬────────────┬────────┬────────┐
│ Kind   ┆ Name   ┆ File       ┆ Size   ┆ Status │
╞════════╪════════╪════════════╪════════╪════════╡
│ VOLUME ┆ pgdata ┆ backup.tar ┆ 12 MiB ┆ ok     │
└────────┴────────┴────────────┴────────┴────────┘
```

A missing or corrupt `backup.tar` shows up in the table as `missing` or
`corrupt`, and `info` exits `1`. `info` also reads the folder that `tar -xjf`
makes of an archive:

```bash
docker-backup info ./pgdata-20260926T141500Z
```

With `--json`, the report carries `"type": "volume"`, which tells it apart from
a full backup's report.

## Restore a volume

```bash
docker-backup restore-volume ./pgdata-20260926T141500Z.tar.bz2
```

`restore-volume` takes exactly one archive. It always verifies `backup.tar`
against `volume.json` first — there is no `--skip-verify`. A missing or corrupt
file stops it with exit `1` before the daemon is touched:

```text
error: backup verification failed: 0 missing, 1 corrupt
```

What happens next depends on the target volume:

| Target volume | Without `--overwrite` | With `--overwrite` |
| --- | --- | --- |
| does not exist | created and filled | created and filled |
| already exists | refused, exit `2` | **emptied and refilled**, after a prompt |

Restoring into a volume that does not exist asks nothing, so it works as-is in
scripts and with `--json`.

### Under another name

`--as` restores into a different volume and leaves the original alone:

```bash
docker-backup restore-volume ./pgdata-20260926T141500Z.tar.bz2 --as pgdata-copy
```

This is the safe way to look at old data next to live data: restore it under a
new name, mount that somewhere, and compare.

### Replacing an existing volume

Without `--overwrite`, an existing target is refused:

```text
error: volume pgdata already exists; pass --overwrite to replace its contents
```

With `--overwrite`, the tool asks before it empties anything:

```text
Volume pgdata will be emptied and refilled from ./pgdata-20260926T141500Z.tar.bz2 (backed up 2026-09-26 14:15 UTC). Continue? [y/N]
```

Only `y` or `yes` continues. Without a terminal — in a script, behind a pipe, or
with `--json` — pass `--yes` (`-y`) to answer the prompt up front. Otherwise the
command aborts with exit `2` and changes nothing.

Unlike `restore --overwrite`, this only ever touches the one volume you name.

### When a restore fails part-way

A new volume is created before its data is streamed in. If the stream fails,
the volume stays in place, possibly half-filled, because the tool never removes
a volume. The report says so:

```text
✗ pgdata → pgdata: docker command failed: …
hint: volume pgdata was created and may be partially filled; retry with --overwrite
```

Run the same command again with `--overwrite` to empty and refill it.

The archive is unpacked into a scratch directory beside itself, which needs
room for the volume's contents.

## Restore by hand

None of this needs `docker-backup`:

```bash
tar -xjf pgdata-20260926T141500Z.tar.bz2
cd pgdata-20260926T141500Z
shasum -a 256 backup.tar            # must match "sha256" in volume.json
docker volume create pgdata
docker run --rm -i -v pgdata:/data alpine:3 tar -C /data -xf - < backup.tar
```

On Linux, `sha256sum backup.tar` does the same job as `shasum -a 256`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | every volume backed up; the volume restored; the archive is intact |
| `1` | a volume failed to back up or restore, `backup.tar` is missing or corrupt, or `volume.json` is invalid |
| `2` | usage error — an unknown volume, an archive that already exists, a target that exists without `--overwrite`, a file that is not a `.tar.bz2` — or declined at the prompt |
| `3` | docker unavailable, or a required tool is missing |
