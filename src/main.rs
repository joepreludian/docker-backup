//! docker-backup: coordinate docker CLI backups and restores of volumes,
//! images and containers.
//!
//! Copyright (C) 2026 Jon Trigueiro
//!
//! This program is free software: you can redistribute it and/or modify it
//! under the terms of the GNU General Public License as published by the
//! Free Software Foundation, either version 3 of the License, or (at your
//! option) any later version. This program is distributed WITHOUT ANY
//! WARRANTY; see the LICENSE file for the full text.

fn main() {
    std::process::exit(docker_backup::cli::run());
}
