//! In-memory implementations of the ports, used only by unit tests.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::application::ports::{
    ArchiveStore, Clock, DockerPort, Operation, ProgressSink, StoredFile, ToolLocator,
};
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::{Compression, DockerInfo, Sha256Digest, ToolInfo};
use crate::domain::refs::{ContainerRef, ImageOrigin, ImageRef, ItemKind, VolumeRef};
use crate::domain::report::ItemOutcome;

fn failed(what: &str) -> AppError {
    AppError::DockerCommandFailed {
        command: what.to_string(),
        stderr: format!("fake failure for {what}"),
    }
}

pub struct FakeDocker {
    pub available: bool,
    pub info: DockerInfo,
    pub volumes: RefCell<BTreeMap<String, Vec<u8>>>,
    pub volatile: HashSet<String>,
    pub images: Vec<(ImageRef, Vec<u8>)>,
    pub containers: Vec<(ContainerRef, Vec<u8>)>,
    pub fail_on: HashSet<String>,
    pub calls: RefCell<Vec<String>>,
    pub loaded_images: RefCell<Vec<Vec<u8>>>,
    pub imported_containers: RefCell<Vec<(String, Vec<u8>)>>,
}

impl Default for FakeDocker {
    fn default() -> Self {
        Self {
            available: true,
            info: DockerInfo {
                server_version: "29.5.2".into(),
                client_version: "29.8.1".into(),
                host: "unix:///fake.sock".into(),
                context: "fake".into(),
                os: "linux".into(),
                arch: "arm64".into(),
            },
            volumes: RefCell::new(BTreeMap::new()),
            volatile: HashSet::new(),
            images: Vec::new(),
            containers: Vec::new(),
            fail_on: HashSet::new(),
            calls: RefCell::new(Vec::new()),
            loaded_images: RefCell::new(Vec::new()),
            imported_containers: RefCell::new(Vec::new()),
        }
    }
}

impl FakeDocker {
    pub fn unavailable() -> Self {
        Self {
            available: false,
            ..Self::default()
        }
    }

    pub fn with_volume(mut self, name: &str, content: &[u8]) -> Self {
        self.volumes
            .get_mut()
            .insert(name.to_string(), content.to_vec());
        self
    }

    pub fn with_volatile_volume(mut self, name: &str, content: &[u8]) -> Self {
        self.volatile.insert(name.to_string());
        self.with_volume(name, content)
    }

    pub fn with_image(mut self, tag: &str, origin: ImageOrigin, content: &[u8]) -> Self {
        let id = format!("sha256:{:0>64}", self.images.len() + 1);
        self.images.push((
            ImageRef {
                id,
                tags: vec![tag.to_string()],
                origin,
            },
            content.to_vec(),
        ));
        self
    }

    pub fn with_container(mut self, name: &str, image: &str, content: &[u8]) -> Self {
        let id = format!("c{}", self.containers.len() + 1);
        self.containers.push((
            ContainerRef {
                id,
                name: name.to_string(),
                image: image.to_string(),
                running: true,
            },
            content.to_vec(),
        ));
        self
    }

    /// Make export/save/import of this name (volume, image tag, or container name) fail.
    pub fn failing(mut self, name: &str) -> Self {
        self.fail_on.insert(name.to_string());
        self
    }

    fn record(&self, call: String) {
        self.calls.borrow_mut().push(call);
    }

    fn check(&self, name: &str) -> AppResult<()> {
        if self.fail_on.contains(name) {
            Err(failed(name))
        } else {
            Ok(())
        }
    }

    fn image(&self, reference: &str) -> Option<&(ImageRef, Vec<u8>)> {
        self.images
            .iter()
            .find(|(i, _)| i.id == reference || i.tags.iter().any(|t| t == reference))
    }
}

impl DockerPort for FakeDocker {
    fn engine_info(&self) -> AppResult<DockerInfo> {
        self.record("engine_info".into());
        if self.available {
            Ok(self.info.clone())
        } else {
            Err(AppError::DockerUnavailable("fake daemon down".into()))
        }
    }

    fn list_volumes(&self) -> AppResult<Vec<VolumeRef>> {
        self.record("list_volumes".into());
        Ok(self
            .volumes
            .borrow()
            .keys()
            .map(|name| {
                let labels = if self.volatile.contains(name) {
                    "com.docker.volume.anonymous="
                } else {
                    ""
                };
                VolumeRef::new(name.clone(), labels)
            })
            .collect())
    }

    fn inspect_volume(&self, name: &str) -> AppResult<Value> {
        Ok(json!({"Name": name, "Driver": "local"}))
    }

    fn list_images(&self) -> AppResult<Vec<ImageRef>> {
        self.record("list_images".into());
        self.check("list_images")?;
        Ok(self.images.iter().map(|(i, _)| i.clone()).collect())
    }

    fn inspect_image(&self, id: &str) -> AppResult<Value> {
        Ok(json!({"Id": id}))
    }

    fn list_containers(&self) -> AppResult<Vec<ContainerRef>> {
        self.record("list_containers".into());
        Ok(self.containers.iter().map(|(c, _)| c.clone()).collect())
    }

    fn inspect_container(&self, name: &str) -> AppResult<Value> {
        Ok(json!({"Name": format!("/{name}")}))
    }

    fn ensure_helper_image(&self) -> AppResult<()> {
        self.record("ensure_helper_image".into());
        Ok(())
    }

    fn export_volume(&self, name: &str, sink: &mut dyn Write) -> AppResult<()> {
        self.record(format!("export_volume:{name}"));
        self.check(name)?;
        let volumes = self.volumes.borrow();
        let content = volumes.get(name).ok_or_else(|| failed(name))?;
        sink.write_all(content)?;
        Ok(())
    }

    fn save_image(&self, reference: &str, sink: &mut dyn Write) -> AppResult<()> {
        self.record(format!("save_image:{reference}"));
        self.check(reference)?;
        let (_, content) = self.image(reference).ok_or_else(|| failed(reference))?;
        sink.write_all(content)?;
        Ok(())
    }

    fn export_container(&self, name: &str, sink: &mut dyn Write) -> AppResult<()> {
        self.record(format!("export_container:{name}"));
        self.check(name)?;
        let (_, content) = self
            .containers
            .iter()
            .find(|(c, _)| c.name == name)
            .ok_or_else(|| failed(name))?;
        sink.write_all(content)?;
        Ok(())
    }

    fn create_volume(&self, name: &str) -> AppResult<()> {
        self.record(format!("create_volume:{name}"));
        self.volumes
            .borrow_mut()
            .insert(name.to_string(), Vec::new());
        Ok(())
    }

    fn import_volume(&self, name: &str, source: &mut dyn Read, wipe: bool) -> AppResult<()> {
        self.record(format!("import_volume:{name}:wipe={wipe}"));
        self.check(name)?;
        let mut content = Vec::new();
        source.read_to_end(&mut content)?;
        let mut volumes = self.volumes.borrow_mut();
        let slot = volumes.entry(name.to_string()).or_default();
        if wipe {
            slot.clear();
        }
        slot.extend_from_slice(&content);
        Ok(())
    }

    fn load_image(&self, source: &mut dyn Read) -> AppResult<()> {
        self.record("load_image".into());
        let mut content = Vec::new();
        source.read_to_end(&mut content)?;
        if self.fail_on.contains("load_image") {
            return Err(failed("load_image"));
        }
        self.loaded_images.borrow_mut().push(content);
        Ok(())
    }

    fn import_container_fs(&self, source: &mut dyn Read, tag: &str) -> AppResult<()> {
        self.record(format!("import_container_fs:{tag}"));
        let mut content = Vec::new();
        source.read_to_end(&mut content)?;
        self.imported_containers
            .borrow_mut()
            .push((tag.to_string(), content));
        Ok(())
    }
}

/// Files live in a map; directories are implicit. `pack_folder` serialises the
/// folder's files into the archive path as JSON so `unpack_archive` can restore
/// them (with the top-level folder stripped, like `--strip-components=1`).
#[derive(Default)]
pub struct MemoryArchiveStore {
    pub files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    pub dirs: RefCell<BTreeSet<PathBuf>>,
    pub packed: RefCell<Vec<(PathBuf, PathBuf)>>,
    pub unpacked: RefCell<Vec<(PathBuf, PathBuf)>>,
    pub fail_pack: Cell<bool>,
    pub fail_unpack: Cell<bool>,
    temp_counter: RefCell<usize>,
}

impl MemoryArchiveStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn file(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        self.files.borrow().get(path.as_ref()).cloned()
    }

    pub fn put(&self, path: impl AsRef<Path>, content: &[u8]) {
        self.files
            .borrow_mut()
            .insert(path.as_ref().to_path_buf(), content.to_vec());
    }

    pub fn paths_under(&self, prefix: &Path) -> Vec<PathBuf> {
        self.files
            .borrow()
            .keys()
            .filter(|p| p.starts_with(prefix))
            .cloned()
            .collect()
    }
}

impl ArchiveStore for MemoryArchiveStore {
    fn create_dir_all(&self, path: &Path) -> AppResult<()> {
        self.dirs.borrow_mut().insert(path.to_path_buf());
        Ok(())
    }

    fn remove_dir_all(&self, path: &Path) -> AppResult<()> {
        self.files.borrow_mut().retain(|p, _| !p.starts_with(path));
        self.dirs.borrow_mut().retain(|p| !p.starts_with(path));
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> AppResult<()> {
        self.files.borrow_mut().remove(path);
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.borrow().contains_key(path)
            || self.dirs.borrow().contains(path)
            || self.files.borrow().keys().any(|p| p.starts_with(path))
    }

    fn write_text(&self, path: &Path, contents: &str) -> AppResult<()> {
        self.put(path, contents.as_bytes());
        Ok(())
    }

    fn read_text(&self, path: &Path) -> AppResult<String> {
        let bytes = self.file(path).ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                path.display().to_string(),
            ))
        })?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn write_item(
        &self,
        path: &Path,
        _compression: Compression,
        producer: &mut dyn FnMut(&mut dyn Write) -> AppResult<()>,
    ) -> AppResult<StoredFile> {
        let mut buffer: Vec<u8> = Vec::new();
        producer(&mut buffer)?;
        let stored = StoredFile {
            size_bytes: buffer.len() as u64,
            sha256: Sha256Digest::of(&buffer),
        };
        self.put(path, &buffer);
        Ok(stored)
    }

    fn open_item(&self, path: &Path, _compression: Compression) -> AppResult<Box<dyn Read>> {
        let bytes = self.file(path).ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                path.display().to_string(),
            ))
        })?;
        Ok(Box::new(Cursor::new(bytes)))
    }

    fn hash_file(&self, path: &Path) -> AppResult<Option<Sha256Digest>> {
        Ok(self.file(path).map(|bytes| Sha256Digest::of(&bytes)))
    }

    fn pack_folder(&self, folder: &Path, archive: &Path) -> AppResult<()> {
        if self.fail_pack.get() {
            return Err(AppError::ToolFailed {
                command: "tar -cjf".into(),
                stderr: "fake pack failure".into(),
            });
        }
        let entries: Vec<(String, Vec<u8>)> = self
            .files
            .borrow()
            .iter()
            .filter(|(p, _)| p.starts_with(folder))
            .map(|(p, bytes)| {
                (
                    p.strip_prefix(folder)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    bytes.clone(),
                )
            })
            .collect();
        let blob = serde_json::to_vec(&entries).expect("serialisable");
        self.put(archive, &blob);
        self.packed
            .borrow_mut()
            .push((folder.to_path_buf(), archive.to_path_buf()));
        Ok(())
    }

    fn unpack_archive(&self, archive: &Path, into: &Path) -> AppResult<()> {
        if self.fail_unpack.get() {
            return Err(AppError::ToolFailed {
                command: "tar -xjf".into(),
                stderr: "fake unpack failure".into(),
            });
        }
        let blob = self.file(archive).ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                archive.display().to_string(),
            ))
        })?;
        let entries: Vec<(String, Vec<u8>)> =
            serde_json::from_slice(&blob).map_err(|e| AppError::ToolFailed {
                command: "tar -x".into(),
                stderr: e.to_string(),
            })?;
        for (relative, bytes) in entries {
            self.put(into.join(relative), &bytes);
        }
        self.unpacked
            .borrow_mut()
            .push((archive.to_path_buf(), into.to_path_buf()));
        Ok(())
    }

    fn make_temp_dir(&self, parent: &Path) -> AppResult<PathBuf> {
        let mut counter = self.temp_counter.borrow_mut();
        *counter += 1;
        let dir = parent.join(format!(".docker-backup-tmp-{}", *counter));
        self.dirs.borrow_mut().insert(dir.clone());
        Ok(dir)
    }
}

pub struct FixedClock(pub OffsetDateTime);

impl Clock for FixedClock {
    fn now_utc(&self) -> OffsetDateTime {
        self.0
    }
}

#[derive(Default)]
pub struct RecordingProgress {
    pub events: RefCell<Vec<String>>,
}

impl ProgressSink for RecordingProgress {
    fn start(&self, operation: Operation, total: usize) {
        self.events
            .borrow_mut()
            .push(format!("start:{operation}:{total}"));
    }
    fn item_started(&self, kind: ItemKind, name: &str, index: usize) {
        self.events
            .borrow_mut()
            .push(format!("started:{kind}:{name}:{index}"));
    }
    fn item_finished(&self, kind: ItemKind, name: &str, outcome: &ItemOutcome) {
        self.events
            .borrow_mut()
            .push(format!("finished:{kind}:{name}:{}", outcome.label()));
    }
    fn finish(&self) {
        self.events.borrow_mut().push("finish".into());
    }
}

#[derive(Default)]
pub struct FakeTools {
    pub available: HashMap<String, ToolInfo>,
}

impl FakeTools {
    pub fn with(mut self, name: &str) -> Self {
        self.available.insert(
            name.to_string(),
            ToolInfo {
                path: format!("/usr/bin/{name}"),
                version: "1.0".into(),
            },
        );
        self
    }
}

impl ToolLocator for FakeTools {
    fn locate(&self, name: &str) -> Option<ToolInfo> {
        self.available.get(name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_pack_and_unpack_strip_the_top_folder() {
        let store = MemoryArchiveStore::new();
        store.put("/tmp/work/out/manifest.json", b"{}");
        store.put("/tmp/work/out/volumes/a.tar", b"A");
        store
            .pack_folder(Path::new("/tmp/work/out"), Path::new("/tmp/out.tar.bz2"))
            .unwrap();
        store.remove_dir_all(Path::new("/tmp/work")).unwrap();
        assert!(!store.exists(Path::new("/tmp/work/out/manifest.json")));
        store
            .unpack_archive(Path::new("/tmp/out.tar.bz2"), Path::new("/tmp/restore"))
            .unwrap();
        assert_eq!(store.file("/tmp/restore/manifest.json").unwrap(), b"{}");
        assert_eq!(store.file("/tmp/restore/volumes/a.tar").unwrap(), b"A");
    }

    #[test]
    fn fake_docker_round_trips_a_volume() {
        let docker = FakeDocker::default().with_volume("v", b"data");
        let mut out = Vec::new();
        docker.export_volume("v", &mut out).unwrap();
        assert_eq!(out, b"data");
        docker
            .import_volume("v", &mut Cursor::new(b"new".to_vec()), true)
            .unwrap();
        assert_eq!(docker.volumes.borrow()["v"], b"new");
        assert!(
            docker
                .calls
                .borrow()
                .contains(&"import_volume:v:wipe=true".to_string())
        );
    }

    #[test]
    fn fake_docker_can_fail_named_items() {
        let docker = FakeDocker::default().with_volume("v", b"data").failing("v");
        assert!(docker.export_volume("v", &mut Vec::new()).is_err());
    }
}
