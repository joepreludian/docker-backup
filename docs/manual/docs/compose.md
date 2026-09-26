# Compose projects

A full [`backup`](backup.md) is the whole daemon: every named volume and every
locally built image, whichever project they belong to. When a daemon runs
several compose projects and you want just one of them, pass its compose files
with `--from-docker-compose`. Backup and restore then only touch what that
project uses.

```text
docker-backup backup  --from-docker-compose FILE[,FILE...] [OUTPUT]
                      [--no-external] [--all-images] [--no-images] [--no-volumes]
                      [--per-file-bzip2 | --single-archive]
docker-backup restore --from-docker-compose FILE[,FILE...] <SOURCE>
                      [--overwrite] [--yes | -y] [--skip-verify]
                      [--force-import-if-arch-mismatch] [--no-images] [--no-volumes]
```

It is the same `backup` and `restore` described in the rest of this manual, with
a narrower scope. The output folder, compression, verification, the preview and
the prompts all work as they always do.

## How the files are read

The tool does not parse YAML itself. It asks compose to render the project:

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml --profile '*' config --format json
```

and reads the result. Everything compose normally does to your files is
therefore already done: overrides are merged in order, `.env` and environment
variables are substituted, `extends` and `include` are followed, and every
volume gets the name compose will really give it.

Pass the files in the order you would pass them to `docker compose -f`, either
comma-separated or by repeating the flag:

```bash
docker-backup backup --from-docker-compose docker-compose.yml,docker-compose.prod.yml ./shop
docker-backup backup --from-docker-compose docker-compose.yml --from-docker-compose docker-compose.prod.yml ./shop
```

Every profile is included, so a volume used only by an optional service (say, a
`debug` profile) is not silently left out.

This needs the `docker compose` plugin. Without it, the command stops with exit
`3`. A compose file that is missing or that compose rejects stops it with exit
`2` and compose's own message, before anything is read or written. Rendering
does not need a running daemon.

## What gets backed up

| In the compose project | Backed up? |
| --- | --- |
| A named volume (`pgdata:/var/lib/postgresql/data`) | yes, under the name compose gives it, e.g. `shop_pgdata` |
| An external volume (`external: true`) | yes, marked external — unless `--no-external` |
| The image of a service with `build:` | yes, when the daemon says it was built locally |
| The image of a service with only `image:` | only with `--all-images`, since it can be pulled again |
| A bind mount (`./data:/data`) | no — those are files on your host |
| An anonymous volume (`- /data`) | no |
| Containers | no — `docker compose up` recreates them |

"Built locally" is decided the way a full backup decides it: by what the daemon
recorded about the image, not by the compose file. A service that has only
`image:` but points at an image you built yourself is still backed up.

An image built by compose without an `image:` name is called
`<project>-<service>`, and that is the tag it is saved under. When two services
build the same image, it is saved once and the manifest lists both services.

`--containers` and `--include-volatile` cannot be combined with
`--from-docker-compose`, and `--no-external` needs it.

## Back up a project

```bash
cd ~/src/shop
docker compose stop
docker-backup backup --from-docker-compose docker-compose.yml ~/Backups/docker/shop-$(date -u +%Y%m%dT%H%M%SZ)
docker compose start
```

A volume or built image the project declares but the daemon does not have — a
service that was never started, an image that was never built — is listed as
`skipped (not found)`. That is not a failure: the run still exits `0`.

If *nothing* the project declares exists on the daemon, the run stops before
writing anything, with exit `2`. That almost always means the wrong daemon:

```text
error: none of compose project shop's volumes or images exist on this docker daemon (not found: shop_appdata, shop_pgdata); check --docker-context
```

!!! warning "Stop the stack first"

    Nothing is stopped or paused. A database writing to its volume while that
    volume is being read can produce a backup that passes every check and still
    will not start. `docker compose stop` before the backup and
    `docker compose start` after it avoid that.

## Restore into a project

```bash
cd ~/src/shop
docker-backup restore --from-docker-compose docker-compose.yml ~/Backups/docker/shop-20260926T180000Z
docker compose up -d
```

The compose files are rendered first, so a mistake in them costs a second
rather than a verification pass over the whole backup. Then the backup is
verified as usual, and only the entries that belong to the project are
restored. Containers in the backup are never imported.

### When the project has another name now

Compose names volumes after the project, and the project is named after its
folder. Clone the repository into `shop2` instead of `shop` and the same
`appdata` volume becomes `shop2_appdata` — a plain restore would put the data in
`shop_appdata`, where the stack never looks.

A backup made with `--from-docker-compose` records each volume's key in the
compose file (`appdata`), so the restore looks the key up in the project as it
is *now* and uses that name. Built images get the project's current tags too.
The preview shows every rename before you confirm:

```text
Restore preview
┌─────────────────┬─────────────────────────────────────────────┐
│ Source          ┆ ~/Backups/docker/shop-20260926T180000Z      │
│ Created at      ┆ 2026-09-26T18:00:00Z                        │
│ Backup platform ┆ linux/arm64                                 │
│ Target platform ┆ linux/arm64                                 │
│ Volumes         ┆ 2 to create, 0 to overwrite, 0 skipped      │
│ Images          ┆ 1 to load                                   │
│ Containers      ┆ 0 to import                                 │
│ Compose project ┆ shop2 (backup made from shop)               │
│ Remapped        ┆ volume shop_appdata → shop2_appdata         │
│                 ┆ image shop-app:latest → shop2-app:latest    │
│                 ┆ image shop-app:latest → shop2-worker:latest │
│ Not in backup   ┆ shop2_cache                                 │
│ Overwrite       ┆ off                                         │
└─────────────────┴─────────────────────────────────────────────┘
Proceed? [y/N]
```

*Not in backup* lists the project's volumes that nothing in the backup maps to,
for example one added to the compose file after the backup was made.

External volumes and volumes with an explicit `name:` keep their names, since
the project name does not affect them.

### Volumes that already exist

The usual rule applies, under the new names: an existing volume is skipped
unless you pass `--overwrite`, which empties and refills it after a prompt. See
[Replacing existing volumes](restore.md#overwrite). With `--from-docker-compose`,
`--overwrite` reaches only the volumes the project uses — but that includes its
external volumes, even one another project shares. Leave them out of the backup
with `--no-external` if that is not what you want.

### From a full backup

`--from-docker-compose` also works on a full backup. It then restores the
entries whose volume name or image tag the project uses now. There is nothing to
remap by, so a renamed project finds nothing; if the backup holds nothing for the
project at all, the restore stops with exit `2`:

```text
error: ./my-backup has nothing for compose project shop2
```

Restoring a compose backup *without* `--from-docker-compose` is an ordinary
restore: everything in it, under the names it was backed up with.

## The project name

Compose takes the project name from the `COMPOSE_PROJECT_NAME` environment
variable (which it also reads from `.env`), then from the `name:` key in the
file, and otherwise from the folder name. `docker-backup` passes its environment
on to compose, so the variable works as usual — and overrides `name:`:

```bash
COMPOSE_PROJECT_NAME=shop-staging docker-backup restore --from-docker-compose docker-compose.yml ./shop-backup
```

## In the manifest

A compose backup's `manifest.json` carries a top-level `compose` block, which is
how you can tell one from a full backup, and each volume and image entry says
where it sits in the project:

```json
{
  "schema_version": 1,
  "compose": {
    "project": "shop",
    "files": ["docker-compose.yml", "docker-compose.prod.yml"]
  },
  "volumes": [
    { "name": "company-shared", "file": "volumes/company-shared.tar", "…": "…",
      "compose": { "key": "shared", "external": true } },
    { "name": "shop_appdata", "file": "volumes/shop_appdata.tar", "…": "…",
      "compose": { "key": "appdata", "external": false } }
  ],
  "images": [
    { "ref": "shop-app:latest", "file": "images/shop-app_latest.tar", "…": "…",
      "compose": { "services": ["app", "worker"] } }
  ]
}
```

`files` are recorded exactly as you typed them. The rendered compose
configuration is deliberately not stored: compose writes `.env` values into it,
and a backup should not become a copy of your secrets. Older versions of
`docker-backup` ignore these fields and restore such a backup as a full one.

`info` shows the project and marks external volumes:

```text
│ Compose project ┆ shop (docker-compose.yml, docker-compose.prod.yml) │
```

```text
┌────────┬───────────────────────────┬────────────────────────────┬────────┬────────┐
│ Kind   ┆ Name                      ┆ File                       ┆ Size   ┆ Status │
╞════════╪═══════════════════════════╪════════════════════════════╪════════╪════════╡
│ VOLUME ┆ company-shared (external) ┆ volumes/company-shared.tar ┆ 18 MiB ┆ ok     │
│ VOLUME ┆ shop_appdata              ┆ volumes/shop_appdata.tar   ┆ 48 MiB ┆ ok     │
│ IMAGE  ┆ shop-app:latest           ┆ images/shop-app_latest.tar ┆ 96 MiB ┆ ok     │
└────────┴───────────────────────────┴────────────────────────────┴────────┴────────┘
```

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | success — including items reported as `skipped (not found)` |
| `1` | an item failed, including an image that loaded but could not be re-tagged, or verification failed |
| `2` | usage error — a compose file compose rejects, a flag that cannot be combined with `--from-docker-compose`, nothing of the project on the daemon, nothing for the project in the backup — or declined at a prompt |
| `3` | docker unavailable, or the `docker compose` plugin or another required tool is missing |
