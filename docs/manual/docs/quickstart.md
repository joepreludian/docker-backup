# Quickstart

The whole tool is four commands. This page runs through all of them once, in the
order you would actually use them.

```mermaid
flowchart LR
    D["doctor<br/><small>is this going to work?</small>"]
    B["backup<br/><small>write a folder</small>"]
    I["info<br/><small>read it back</small>"]
    R["restore<br/><small>load it into a daemon</small>"]
    D --> B --> I --> R
    R -. "same folder, another machine" .-> I
```

## 1. Check the daemon

```bash
docker-backup doctor
```

It prints the daemon it found and whether `docker`, `tar` and `bzip2` are
available, then ends with `healthy` or a description of what is wrong. Exit
code `3` means the daemon is unreachable or a required tool is missing — fix
that before going further.

## 2. Take a backup

```bash
docker-backup backup ./my-backup --per-file-bzip2
```

That captures **every named volume** and **every locally built image**. Images
you merely pulled are skipped, because a registry can supply them again;
anonymous volumes are skipped, because they are usually scratch space. Both
defaults can be changed — see [How to back up](backup.md).

The output folder must not already contain a backup. `docker-backup` refuses to
mix two of them together and exits `2` rather than overwriting.

To include a container's whole filesystem, name it:

```bash
docker-backup backup ./my-backup --containers web,db
```

## 3. Inspect what you captured

```bash
docker-backup info ./my-backup
```

`info` prints the manifest — when the backup was made, which daemon and platform
produced it, and every file with its size — and verifies each recorded SHA-256.
Run it before you trust a backup, and again before you rely on one that has been
sitting on a disk or moved between machines.

## 4. Restore it

```bash
docker-backup restore ./my-backup
```

Nothing is changed yet. `restore` first verifies every hash, then shows what it
intends to do and waits:

```text
Restore preview
┌─────────────────┬────────────────────────────────────────┐
│ Source          ┆ ./my-backup                            │
│ Created at      ┆ 2026-09-20T09:12:44Z                   │
│ Backup platform ┆ linux/arm64                            │
│ Target platform ┆ linux/arm64                            │
│ Volumes         ┆ 3 to create, 0 to overwrite, 1 skipped │
│ Images          ┆ 2 to load                              │
│ Containers      ┆ 0 to import                            │
│ Overwrite       ┆ off                                    │
└─────────────────┴────────────────────────────────────────┘
Proceed? [y/N]
```

Answer anything other than `y` or `yes` and it exits `2` having changed nothing.

Read that `1 skipped` carefully. By default a volume that already exists on the
host is left completely alone — `restore` only ever *creates* missing volumes.
Replacing the contents of volumes that already exist requires `--overwrite`,
which is a much bigger hammer than it looks; [read about it](restore.md#overwrite)
before you reach for it.

## Scripting it

Prompts need a terminal. Without one — in a script, behind a pipe, or with
`--json` — `restore` aborts with exit code `2` and tells you to pass `--yes`:

```bash
docker-backup --json restore ./my-backup --yes | jq '.summary'
```

`--yes` answers both the preview prompt and the architecture-mismatch prompt, so
use it only where you already know what the backup contains.

## Moving between machines

The common reason to use this tool is getting a stack off one machine and onto
another. The single-archive mode makes that one file to copy:

```bash
# On the old machine
docker-backup backup ./stack --single-archive     # produces ./stack.tar.bz2
scp stack.tar.bz2 newhost:

# On the new machine
docker-backup info ./stack.tar.bz2
docker-backup restore ./stack.tar.bz2
```

`info` and `restore` both accept the archive directly and unpack it into a
temporary directory alongside it, so make sure there is roughly twice the
archive's size free on that filesystem.

!!! warning "Different machine, possibly different architecture"

    If the two machines do not share an os/arch — an Intel server and an Apple
    Silicon laptop, say — images built for the other platform will not run.
    `restore` notices, lists them, and asks a second time before continuing.
    See [the architecture check](restore.md#the-architecture-check).

## Next

- [How to back up](backup.md) — selection rules, compression, and layout.
- [How to restore](restore.md) — the full restore model, including `--overwrite`
  and the architecture check.
