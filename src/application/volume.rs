//! Single-volume archives: `backup-volume`, `restore-volume`, and the check that
//! `info` shares with `restore-volume`.

use std::path::Path;

use crate::application::ports::ArchiveStore;
use crate::domain::error::AppResult;
use crate::domain::refs::ItemKind;
use crate::domain::verification::{FileCheck, FileStatus, VerificationReport};
use crate::domain::volume_manifest::{VOLUME_DATA_FILE, VOLUME_MANIFEST_FILE, VolumeManifest};

/// Reads `volume.json` in `root` and checks `backup.tar` against it. `info` and
/// `restore-volume` both call this, so they never disagree on what "verified" means.
pub fn verify_volume_archive(
    store: &dyn ArchiveStore,
    root: &Path,
) -> AppResult<(VolumeManifest, VerificationReport)> {
    let manifest = VolumeManifest::from_json(&store.read_text(&root.join(VOLUME_MANIFEST_FILE))?)?;
    let actual = store.hash_file(&root.join(VOLUME_DATA_FILE))?;
    let check = FileCheck {
        kind: ItemKind::Volume,
        name: manifest.volume.clone(),
        file: VOLUME_DATA_FILE.to_string(),
        size_bytes: manifest.size_bytes,
        status: FileStatus::classify(&manifest.sha256, actual.as_ref()),
    };
    Ok((manifest, VerificationReport { files: vec![check] }))
}
