# Installation

## Prerequisites

`docker-backup` drives other programs rather than reimplementing them, so a few
must be on your `PATH`:

| Tool | Required | Used for |
| --- | --- | --- |
| `docker` | yes | every daemon operation, and the helper container that reads volumes |
| `tar` | yes | packing and unpacking volume and container archives |
| `bzip2` | no | only for `--per-file-bzip2` and `--single-archive` |

You also need a reachable daemon. Docker Desktop, Colima, Rancher Desktop, and a
plain Linux `dockerd` all work; the tool talks to whichever context is current,
or to the one named with `--docker-context`.

## Homebrew

The quickest route on macOS, and on Linux with Homebrew installed:

```bash
brew install joepreludian/tap/docker-backup
```

That taps [joepreludian/homebrew-tap](https://github.com/joepreludian/homebrew-tap)
and installs the same signed, notarized binary attached to the release — Homebrew
does not rebuild it from source. Later versions arrive with `brew upgrade`.

The formula deliberately does **not** declare a dependency on a `docker`
package. Homebrew's `docker` formula would collide with the Docker Desktop,
Colima or Rancher Desktop installation you probably already have, so the docker
CLI stays yours to manage. `docker-backup doctor` reports it if it is missing.

## Install script

No Homebrew, and no root:

```bash
curl -fsSL https://docker-backup.jon.dev.br/install.sh | sh
```

It detects your operating system and architecture, downloads the matching
release archive, **checks it against the SHA-256 published alongside the
release**, and only then installs the binary into `~/.local/bin`. Nothing is
written before the checksum matches, and the script never asks for `sudo`.

Two environment variables change what it does:

| Variable | Effect |
| --- | --- |
| `DOCKER_BACKUP_VERSION` | install this version instead of the latest release |
| `DOCKER_BACKUP_BIN_DIR` | install here instead of `~/.local/bin` |

```bash
curl -fsSL https://docker-backup.jon.dev.br/install.sh | DOCKER_BACKUP_VERSION=0.2.0 sh
```

If `~/.local/bin` is not on your `PATH`, the script says so and prints the exact
line to add for your shell. It also warns you when a different `docker-backup`
earlier on your `PATH` would shadow the one it has just installed.

!!! tip "Read it before you run it"

    Piping a script from the internet into a shell deserves a look first. This
    one is a single file —
    [install.sh](https://github.com/joepreludian/docker-backup/blob/main/install.sh)
    — and `curl -fsSL https://docker-backup.jon.dev.br/install.sh | less` shows
    you exactly what would run.

## Download a release

If you would rather not use either of the above, releases are published for
four targets. Pick yours, verify it, and put the binary on your `PATH`.

=== "Linux (x86_64)"

    ```bash
    VERSION=0.2.0
    TARGET=x86_64-unknown-linux-musl
    BASE=https://github.com/joepreludian/docker-backup/releases/download/v$VERSION

    curl -fLO $BASE/docker-backup-$VERSION-$TARGET.tar.gz
    curl -fLO $BASE/SHA256SUMS
    sha256sum -c --ignore-missing SHA256SUMS
    tar -xzf docker-backup-$VERSION-$TARGET.tar.gz
    sudo install docker-backup-$VERSION-$TARGET/docker-backup /usr/local/bin/
    ```

=== "Linux (ARM64)"

    ```bash
    VERSION=0.2.0
    TARGET=aarch64-unknown-linux-musl
    BASE=https://github.com/joepreludian/docker-backup/releases/download/v$VERSION

    curl -fLO $BASE/docker-backup-$VERSION-$TARGET.tar.gz
    curl -fLO $BASE/SHA256SUMS
    sha256sum -c --ignore-missing SHA256SUMS
    tar -xzf docker-backup-$VERSION-$TARGET.tar.gz
    sudo install docker-backup-$VERSION-$TARGET/docker-backup /usr/local/bin/
    ```

=== "macOS (Apple Silicon)"

    ```bash
    VERSION=0.2.0
    TARGET=aarch64-apple-darwin
    BASE=https://github.com/joepreludian/docker-backup/releases/download/v$VERSION

    curl -fLO $BASE/docker-backup-$VERSION-$TARGET.tar.gz
    curl -fLO $BASE/SHA256SUMS
    shasum -a 256 -c --ignore-missing SHA256SUMS
    tar -xzf docker-backup-$VERSION-$TARGET.tar.gz
    sudo install docker-backup-$VERSION-$TARGET/docker-backup /usr/local/bin/
    ```

=== "macOS (Intel)"

    ```bash
    VERSION=0.2.0
    TARGET=x86_64-apple-darwin
    BASE=https://github.com/joepreludian/docker-backup/releases/download/v$VERSION

    curl -fLO $BASE/docker-backup-$VERSION-$TARGET.tar.gz
    curl -fLO $BASE/SHA256SUMS
    shasum -a 256 -c --ignore-missing SHA256SUMS
    tar -xzf docker-backup-$VERSION-$TARGET.tar.gz
    sudo install docker-backup-$VERSION-$TARGET/docker-backup /usr/local/bin/
    ```

`--ignore-missing` matters: `SHA256SUMS` covers every published archive, and
without it the check fails on the three you did not download.

### Notes per platform

**Linux** builds are statically linked against musl. They carry no libc
dependency and run on any distribution, including Alpine and distroless-style
images.

**macOS** builds are signed with a Developer ID certificate and notarised by
Apple. A notarisation ticket cannot be stapled to a bare executable — only to
app bundles, disk images and installer packages — so if you download the archive
through a browser, the first run performs an online Gatekeeper check and needs
network access for a moment. Fetching it with `curl` as shown above avoids the
quarantine attribute altogether.

## Build from source

```bash
git clone https://github.com/joepreludian/docker-backup.git
cd docker-backup
cargo install --path .
```

The crate uses the 2024 edition, so it needs a reasonably current stable
toolchain. `rustup update stable` is enough if the build complains about the
edition.

## Confirm it works

```bash
docker-backup --version
docker-backup doctor
```

`doctor` is the honest answer to "is this going to work here". It reports the
daemon it found, its platform, what it can see, and whether each external tool
is present:

```text
┌────────────────┬───────────────────────────────────────────────────┐
│ Docker         ┆ Value                                             │
╞════════════════╪═══════════════════════════════════════════════════╡
│ Status         ┆ available                                         │
│ Context        ┆ colima                                            │
│ Host           ┆ unix:///Users/example/.colima/default/docker.sock  │
│ Server version ┆ 29.5.2                                            │
│ Client version ┆ 29.8.1                                            │
│ Platform       ┆ linux/arm64                                       │
│ Images         ┆ 10 created, 13 pulled, 23 total                   │
│ Volumes        ┆ 20 named, 8 volatile, 28 total                    │
│ Containers     ┆ 6 running, 9 total                                │
└────────────────┴───────────────────────────────────────────────────┘
┌────────┬──────────┬────────┬──────────────────────────┬──────────────────────────────────────────┐
│ Tool   ┆ Required ┆ Status ┆ Path                     ┆ Version                                  │
╞════════╪══════════╪════════╪══════════════════════════╪══════════════════════════════════════════╡
│ docker ┆ yes      ┆ found  ┆ /opt/homebrew/bin/docker ┆ Docker version 29.8.1, build 4a63305d74  │
│ tar    ┆ yes      ┆ found  ┆ /usr/bin/tar             ┆ bsdtar 3.5.3 - libarchive 3.7.4          │
│ bzip2  ┆ no       ┆ found  ┆ /usr/bin/bzip2           ┆ bzip2, Version 1.0.8, 13-Jul-2019.       │
└────────┴──────────┴────────┴──────────────────────────┴──────────────────────────────────────────┘
healthy
```

The **Platform** row is worth noting now rather than later: it is the value a
restore compares against, and the reason an `amd64` backup behaves differently
on this machine than on the one that produced it. See
[the architecture check](restore.md#the-architecture-check).

!!! tip "Machine-readable output"

    Every command accepts `--json` and will then print one JSON document on
    stdout, with progress kept on stderr:

    ```bash
    docker-backup --json doctor | jq '.docker.platform'
    ```

## Choosing a daemon

If you have more than one context, `docker context ls` lists them and
`--docker-context NAME` selects one for a single run:

```bash
docker-backup --docker-context colima doctor
```

Without the flag, the current context is used.

## Exit codes

Useful when scripting any of this:

| Code | Meaning |
| --- | --- |
| `0` | success |
| `1` | some items failed, or verification failed |
| `2` | usage error, output conflict, or a restore declined at a prompt |
| `3` | docker unavailable, or a required tool is missing |
