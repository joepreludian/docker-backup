#!/bin/sh
# Installs docker-backup into ~/.local/bin. No sudo, no package manager.
#
#     curl -fsSL https://docker-backup.jon.dev.br/install.sh | sh
#
# Environment:
#     DOCKER_BACKUP_VERSION   install this version instead of the latest release
#     DOCKER_BACKUP_BIN_DIR   install here instead of ~/.local/bin

set -eu

REPO="joepreludian/docker-backup"
BIN_DIR="${DOCKER_BACKUP_BIN_DIR:-$HOME/.local/bin}"

die() {
    echo "install.sh: $*" >&2
    exit 1
}

require() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required but is not on PATH"
}

# Maps uname output onto one of the four targets the release workflow builds.
detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin) os_part="apple-darwin" ;;
        Linux) os_part="unknown-linux-musl" ;;
        *) die "unsupported operating system: $os (macOS and Linux are built)" ;;
    esac

    case "$arch" in
        x86_64 | amd64) arch_part="x86_64" ;;
        arm64 | aarch64) arch_part="aarch64" ;;
        *) die "unsupported architecture: $arch (x86_64 and aarch64 are built)" ;;
    esac

    echo "${arch_part}-${os_part}"
}

# GitHub redirects /releases/latest to the tag page, so the newest version can
# be read without an API token, a rate limit, or a JSON parser.
latest_version() {
    url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest")" ||
        die "could not reach GitHub to look up the latest release"

    version="${url##*/tag/v}"
    case "$version" in
        "" | */*) die "could not read a version out of $url" ;;
    esac
    echo "$version"
}

# The published SHA256SUMS lists names with a ./ prefix, and macOS shasum has no
# --ignore-missing, so neither `sha256sum -c` nor `shasum -c` can check a single
# file straight out of it. Pull out the one line that matters and compare the
# digests as text instead.
verify() {
    path="$1"
    sums="$2"
    name="$(basename "$path")"

    expected="$(awk -v want="$name" '
        { file = $2; sub(/^\.\//, "", file); if (file == want) { print $1; exit } }
    ' "$sums")"
    [ -n "$expected" ] || die "$name is not listed in SHA256SUMS"

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$path" | cut -d ' ' -f 1)"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$path" | cut -d ' ' -f 1)"
    else
        die "neither sha256sum nor shasum is available; refusing to install unverified"
    fi

    [ "$expected" = "$actual" ] || die "checksum mismatch for $name
  expected $expected
  actual   $actual"
}

# ~/.local/bin is not on PATH by default on most systems, and an install the
# user cannot invoke is not an install.
path_hint() {
    case "$(basename "${SHELL:-/bin/sh}")" in
        fish) echo "    fish_add_path $BIN_DIR" ;;
        zsh) echo "    echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> $HOME/.zshrc" ;;
        bash)
            if [ "$(uname -s)" = "Darwin" ]; then
                echo "    echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> $HOME/.bash_profile"
            else
                echo "    echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> $HOME/.bashrc"
            fi
            ;;
        *) echo "    echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> $HOME/.profile" ;;
    esac
}

report_path() {
    case ":$PATH:" in
        *":$BIN_DIR:"*)
            shadow="$(command -v docker-backup || true)"
            if [ -n "$shadow" ] && [ "$shadow" != "$BIN_DIR/docker-backup" ]; then
                echo
                echo "Warning: $shadow comes earlier on your PATH and will be run instead."
            fi
            ;;
        *)
            echo
            echo "$BIN_DIR is not on your PATH. Add it with:"
            echo
            path_hint
            echo
            echo "then open a new shell."
            ;;
    esac
}

main() {
    require curl
    require tar
    require install

    target="$(detect_target)"
    version="${DOCKER_BACKUP_VERSION:-$(latest_version)}"
    version="${version#v}"

    tarball="docker-backup-${version}-${target}.tar.gz"
    base="https://github.com/$REPO/releases/download/v${version}"

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT INT TERM

    echo "Downloading docker-backup $version for $target"
    curl -fsSL -o "$tmp/$tarball" "$base/$tarball" ||
        die "could not download $base/$tarball"
    curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" ||
        die "could not download $base/SHA256SUMS"

    verify "$tmp/$tarball" "$tmp/SHA256SUMS"
    echo "Checksum verified"

    tar -xzf "$tmp/$tarball" -C "$tmp"
    binary="$tmp/docker-backup-${version}-${target}/docker-backup"
    [ -f "$binary" ] || die "the archive did not contain docker-backup"

    mkdir -p "$BIN_DIR"
    install -m 755 "$binary" "$BIN_DIR/docker-backup"

    if "$BIN_DIR/docker-backup" --version >/dev/null 2>&1; then
        echo "Installed $("$BIN_DIR/docker-backup" --version) to $BIN_DIR/docker-backup"
    else
        die "installed $BIN_DIR/docker-backup but it would not run on this machine"
    fi

    report_path
}

main "$@"
