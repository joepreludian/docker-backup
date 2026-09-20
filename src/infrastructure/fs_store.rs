//! Real filesystem store. Items are written to `<file>.tmp` and renamed on
//! success; compression shells out to `bzip2`, archives to `tar`.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread;

use sha2::{Digest, Sha256};

use crate::application::ports::{ArchiveStore, StoredFile};
use crate::domain::error::{AppError, AppResult};
use crate::domain::manifest::{Compression, Sha256Digest};

pub struct FsArchiveStore {
    bzip2: String,
    tar: String,
}

impl Default for FsArchiveStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FsArchiveStore {
    pub fn new() -> Self {
        Self::with_tools("bzip2", "tar")
    }

    pub fn with_tools(bzip2: impl Into<String>, tar: impl Into<String>) -> Self {
        Self {
            bzip2: bzip2.into(),
            tar: tar.into(),
        }
    }

    fn write_plain(
        file: File,
        producer: &mut dyn FnMut(&mut dyn Write) -> AppResult<()>,
    ) -> AppResult<StoredFile> {
        let mut writer = HashingWriter::new(BufWriter::new(file));
        producer(&mut writer)?;
        writer.flush()?;
        let (buffered, stored) = writer.finish();
        buffered
            .into_inner()
            .map_err(|e| AppError::Io(e.into_error()))?
            .sync_all()?;
        Ok(stored)
    }

    fn write_bzip2(
        &self,
        file: File,
        producer: &mut dyn FnMut(&mut dyn Write) -> AppResult<()>,
    ) -> AppResult<StoredFile> {
        let mut child = Command::new(&self.bzip2)
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| AppError::ToolMissing(self.bzip2.clone()))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");

        let writer = thread::spawn(move || -> io::Result<StoredFile> {
            let mut hashing = HashingWriter::new(BufWriter::new(file));
            io::copy(&mut stdout, &mut hashing)?;
            hashing.flush()?;
            let (buffered, stored) = hashing.finish();
            buffered
                .into_inner()
                .map_err(|e| e.into_error())?
                .sync_all()?;
            Ok(stored)
        });
        let stderr_reader = thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });

        let produced = producer(&mut stdin);
        drop(stdin);
        let status = child.wait()?;
        let stored = writer.join().expect("writer thread panicked")?;
        let stderr_text = stderr_reader.join().expect("stderr thread panicked");
        produced?;
        if !status.success() {
            return Err(AppError::ToolFailed {
                command: format!("{} -c", self.bzip2),
                stderr: stderr_text.trim().to_string(),
            });
        }
        Ok(stored)
    }

    fn run_tar(&self, args: &[&str]) -> AppResult<()> {
        let output = Command::new(&self.tar)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|_| AppError::ToolMissing(self.tar.clone()))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(AppError::ToolFailed {
                command: format!("{} {}", self.tar, args.join(" ")),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }
}

/// Counts bytes and computes SHA-256 as they pass through.
pub struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    pub fn finish(self) -> (W, StoredFile) {
        let digest = Sha256Digest::from_raw(&self.hasher.finalize());
        (
            self.inner,
            StoredFile {
                size_bytes: self.bytes,
                sha256: digest,
            },
        )
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hasher.update(&buf[..written]);
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Reads a child's stdout, checking its exit status once EOF is reached, and
/// reaps the child (killing it if it is still running) when dropped.
struct ChildReader {
    child: Child,
    stdout: ChildStdout,
    stderr_thread: Option<thread::JoinHandle<String>>,
    path: String,
    done: bool,
}

impl Read for ChildReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.stdout.read(buf)?;
        if read == 0 && !self.done {
            self.done = true;
            let status = self.child.wait()?;
            let stderr_text = self
                .stderr_thread
                .take()
                .and_then(|handle| handle.join().ok())
                .unwrap_or_default();
            if !status.success() {
                return Err(io::Error::other(format!(
                    "bzip2 -dc {} failed: {}",
                    self.path,
                    stderr_text.trim()
                )));
            }
        }
        Ok(read)
    }
}

impl Drop for ChildReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

fn temp_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

impl ArchiveStore for FsArchiveStore {
    fn create_dir_all(&self, path: &Path) -> AppResult<()> {
        Ok(fs::create_dir_all(path)?)
    }

    fn remove_dir_all(&self, path: &Path) -> AppResult<()> {
        Ok(fs::remove_dir_all(path)?)
    }

    fn remove_file(&self, path: &Path) -> AppResult<()> {
        Ok(fs::remove_file(path)?)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn write_text(&self, path: &Path, contents: &str) -> AppResult<()> {
        let tmp = temp_path(path);
        fs::write(&tmp, contents)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    fn read_text(&self, path: &Path) -> AppResult<String> {
        Ok(fs::read_to_string(path)?)
    }

    fn write_item(
        &self,
        path: &Path,
        compression: Compression,
        producer: &mut dyn FnMut(&mut dyn Write) -> AppResult<()>,
    ) -> AppResult<StoredFile> {
        let tmp = temp_path(path);
        let file = File::create(&tmp)?;
        let result = match compression {
            Compression::None => Self::write_plain(file, producer),
            Compression::Bzip2PerFile => self.write_bzip2(file, producer),
        };
        match result {
            Ok(stored) => {
                fs::rename(&tmp, path)?;
                Ok(stored)
            }
            Err(error) => {
                let _ = fs::remove_file(&tmp);
                Err(error)
            }
        }
    }

    fn open_item(&self, path: &Path, compression: Compression) -> AppResult<Box<dyn Read>> {
        match compression {
            Compression::None => Ok(Box::new(BufReader::new(File::open(path)?))),
            Compression::Bzip2PerFile => {
                let mut child = Command::new(&self.bzip2)
                    .arg("-dc")
                    .arg(path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|_| AppError::ToolMissing(self.bzip2.clone()))?;
                let stdout = child.stdout.take().expect("piped stdout");
                let mut stderr = child.stderr.take().expect("piped stderr");
                let stderr_thread = thread::spawn(move || {
                    let mut text = String::new();
                    let _ = stderr.read_to_string(&mut text);
                    text
                });
                Ok(Box::new(ChildReader {
                    child,
                    stdout,
                    stderr_thread: Some(stderr_thread),
                    path: path.display().to_string(),
                    done: false,
                }))
            }
        }
    }

    fn hash_file(&self, path: &Path) -> AppResult<Option<Sha256Digest>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(Some(Sha256Digest::from_raw(&hasher.finalize())))
    }

    fn pack_folder(&self, folder: &Path, archive: &Path) -> AppResult<()> {
        let parent = folder
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let name = folder
            .file_name()
            .ok_or_else(|| AppError::Conflict(format!("cannot pack {}", folder.display())))?;
        let parent = parent.display().to_string();
        let archive = archive.display().to_string();
        let name = name.to_string_lossy().into_owned();
        self.run_tar(&["-cjf", &archive, "-C", &parent, &name])
    }

    fn unpack_archive(&self, archive: &Path, into: &Path) -> AppResult<()> {
        fs::create_dir_all(into)?;
        let archive = archive.display().to_string();
        let into = into.display().to_string();
        self.run_tar(&["-xjf", &archive, "-C", &into, "--strip-components=1"])
    }

    fn make_temp_dir(&self, parent: &Path) -> AppResult<PathBuf> {
        fs::create_dir_all(parent)?;
        let dir = tempfile::Builder::new()
            .prefix(".docker-backup-")
            .tempdir_in(parent)?;
        Ok(dir.keep())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn has(tool: &str) -> bool {
        which::which(tool).is_ok()
    }

    #[test]
    fn hashing_writer_counts_and_hashes() {
        let mut writer = HashingWriter::new(Vec::new());
        writer.write_all(b"hel").unwrap();
        writer.write_all(b"lo\n").unwrap();
        let (inner, stored) = writer.finish();
        assert_eq!(inner, b"hello\n");
        assert_eq!(stored.size_bytes, 6);
        assert_eq!(
            stored.sha256.to_string(),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn write_item_plain_is_atomic_and_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsArchiveStore::new();
        let path = dir.path().join("v.tar");
        let stored = store
            .write_item(&path, Compression::None, &mut |sink| {
                sink.write_all(b"hello\n")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(stored.size_bytes, 6);
        assert_eq!(fs::read(&path).unwrap(), b"hello\n");
        assert_eq!(store.hash_file(&path).unwrap(), Some(stored.sha256));
        assert!(!dir.path().join("v.tar.tmp").exists());
    }

    #[test]
    fn write_item_failure_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsArchiveStore::new();
        let path = dir.path().join("v.tar");
        let err = store
            .write_item(&path, Compression::None, &mut |sink| {
                sink.write_all(b"partial")?;
                Err(AppError::Conflict("boom".into()))
            })
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
        assert!(!path.exists());
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn hash_file_of_missing_file_is_none() {
        let store = FsArchiveStore::new();
        assert_eq!(
            store.hash_file(Path::new("/definitely/not/here")).unwrap(),
            None
        );
    }

    #[test]
    fn bzip2_round_trip() {
        if !has("bzip2") {
            eprintln!("bzip2 not installed, skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = FsArchiveStore::new();
        let path = dir.path().join("v.tar.bz2");
        let payload = b"0123456789".repeat(1000);
        let stored = store
            .write_item(&path, Compression::Bzip2PerFile, &mut |sink| {
                sink.write_all(&payload)?;
                Ok(())
            })
            .unwrap();
        let on_disk = fs::read(&path).unwrap();
        assert!(on_disk.starts_with(b"BZh"));
        assert_eq!(stored.size_bytes, on_disk.len() as u64);
        assert_eq!(stored.sha256, Sha256Digest::of(&on_disk));
        let mut reader = store.open_item(&path, Compression::Bzip2PerFile).unwrap();
        let mut back = Vec::new();
        reader.read_to_end(&mut back).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn corrupt_bzip2_item_reports_an_error() {
        if !has("bzip2") {
            eprintln!("bzip2 not installed, skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = FsArchiveStore::new();
        let path = dir.path().join("bad.tar.bz2");
        fs::write(&path, b"this is not bzip2 data").unwrap();
        let mut reader = store.open_item(&path, Compression::Bzip2PerFile).unwrap();
        let mut back = Vec::new();
        let result = reader.read_to_end(&mut back);
        assert!(result.is_err());
    }

    #[test]
    fn dropping_a_bzip2_reader_early_does_not_hang() {
        if !has("bzip2") {
            eprintln!("bzip2 not installed, skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = FsArchiveStore::new();
        let path = dir.path().join("big.tar.bz2");
        let payload = vec![b'a'; 4 * 1024 * 1024];
        store
            .write_item(&path, Compression::Bzip2PerFile, &mut |sink| {
                sink.write_all(&payload)?;
                Ok(())
            })
            .unwrap();
        let mut reader = store.open_item(&path, Compression::Bzip2PerFile).unwrap();
        let mut head = [0u8; 16];
        reader.read_exact(&mut head).unwrap();
        assert_eq!(&head, b"aaaaaaaaaaaaaaaa");
        drop(reader);
    }

    #[test]
    fn missing_bzip2_is_tool_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsArchiveStore::with_tools("definitely-not-bzip2-xyz", "tar");
        let err = store
            .write_item(
                &dir.path().join("v.tar.bz2"),
                Compression::Bzip2PerFile,
                &mut |_| Ok(()),
            )
            .unwrap_err();
        assert!(matches!(err, AppError::ToolMissing(_)));
    }

    #[test]
    fn pack_and_unpack_round_trip_with_strip_components() {
        if !has("tar") || !has("bzip2") {
            eprintln!("tar/bzip2 not installed, skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = FsArchiveStore::new();
        let folder = dir.path().join("work").join("out");
        fs::create_dir_all(folder.join("volumes")).unwrap();
        fs::write(folder.join("manifest.json"), b"{}").unwrap();
        fs::write(folder.join("volumes/a.tar"), b"A").unwrap();
        let archive = dir.path().join("out.tar.bz2");
        store.pack_folder(&folder, &archive).unwrap();
        assert!(archive.exists());
        let into = store.make_temp_dir(dir.path()).unwrap();
        assert!(into.starts_with(dir.path()));
        store.unpack_archive(&archive, &into).unwrap();
        assert_eq!(fs::read(into.join("manifest.json")).unwrap(), b"{}");
        assert_eq!(fs::read(into.join("volumes/a.tar")).unwrap(), b"A");
        store.remove_dir_all(&into).unwrap();
        assert!(!into.exists());
    }

    #[test]
    fn text_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsArchiveStore::new();
        let path = dir.path().join("nested").join("m.json");
        store.create_dir_all(path.parent().unwrap()).unwrap();
        store.write_text(&path, "{}").unwrap();
        assert!(store.exists(&path));
        assert_eq!(store.read_text(&path).unwrap(), "{}");
        store.remove_file(&path).unwrap();
        assert!(!store.exists(&path));
    }
}
