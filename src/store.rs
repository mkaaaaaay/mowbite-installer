//! The backup folder of one mower: files are only ever created, never replaced, and each one is listed with its
//! sha256 in SHA256SUMS once it is complete. A file without its line is unfinished and is never used.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

pub const SUMS: &str = "SHA256SUMS";

pub struct Store {
    root: PathBuf,
}

/// A file being written; it counts only after finish().
pub struct NewFile {
    file: File,
    hash: Sha256,
    name: String,
    root: PathBuf,
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name != SUMS
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !ok {
        bail!("not a valid file name in the backup: {name:?}");
    }
    Ok(())
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root).with_context(|| format!("create {}", root.display()))?;
        Ok(Store {
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Fails when the name exists, finished or not: an earlier backup is never overwritten.
    pub fn create_new(&mut self, name: &str) -> Result<NewFile> {
        check_name(name)?;
        let path = self.root.join(name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("{} exists or cannot be created", path.display()))?;
        Ok(NewFile {
            file,
            hash: Sha256::new(),
            name: name.to_string(),
            root: self.root.clone(),
        })
    }

    /// The sha256 recorded for a finished file.
    pub fn recorded(&self, name: &str) -> Result<Option<String>> {
        let path = self.root.join(SUMS);
        let Ok(file) = File::open(&path) else {
            return Ok(None);
        };
        let mut found = None;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if let Some((sum, file_name)) = line.split_once("  ") {
                if file_name == name {
                    if found.is_some() {
                        bail!("{name} is listed twice in {SUMS}");
                    }
                    found = Some(sum.to_string());
                }
            }
        }
        Ok(found)
    }

    /// Reads the whole file and compares it with its line in SHA256SUMS; use the file only after this passed.
    pub fn verify(&self, name: &str) -> Result<()> {
        check_name(name)?;
        let Some(want) = self.recorded(name)? else {
            bail!("{name} is not finished")
        };
        let mut file = File::open(self.root.join(name))?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        let got = hex(&hash.finalize());
        if got != want {
            bail!("{name} changed since the backup: sha256 {got}, recorded {want}");
        }
        Ok(())
    }

    /// Removes a file left unfinished by an abort, so create_new can start it again; a finished one stays.
    pub fn discard_unfinished(&mut self, name: &str) -> Result<()> {
        check_name(name)?;
        if self.recorded(name)?.is_some() {
            bail!("{name} is finished and stays");
        }
        match std::fs::remove_file(self.root.join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    pub fn path(&self, name: &str) -> Result<PathBuf> {
        check_name(name)?;
        Ok(self.root.join(name))
    }
}

impl NewFile {
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.hash.update(data);
        self.file.write_all(data)?;
        Ok(())
    }

    /// Syncs the file, then appends its line to SHA256SUMS and syncs that too. Returns the sha256.
    pub fn finish(self) -> Result<String> {
        self.file.sync_all()?;
        let sum = hex(&self.hash.finalize());
        let mut sums = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(SUMS))?;
        writeln!(sums, "{sum}  {}", self.name)?;
        sums.sync_all()?;
        Ok(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_counts_only_when_finished() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut f = store.create_new("rootfs.img").unwrap();
        f.write(b"abc").unwrap();
        assert!(store.verify("rootfs.img").is_err());
        let sum = f.finish().unwrap();
        assert_eq!(
            sum,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        store.verify("rootfs.img").unwrap();
    }

    #[test]
    fn an_existing_file_is_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.create_new("a.img").unwrap().finish().unwrap();
        assert!(store.create_new("a.img").is_err());
        std::fs::write(dir.path().join("b.img"), b"left over").unwrap();
        assert!(store.create_new("b.img").is_err());
    }

    #[test]
    fn a_changed_file_fails_verify() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut f = store.create_new("boot.img").unwrap();
        f.write(b"original").unwrap();
        f.finish().unwrap();
        std::fs::write(dir.path().join("boot.img"), b"changed!").unwrap();
        let err = store.verify("boot.img").unwrap_err().to_string();
        assert!(err.contains("changed since the backup"), "{err}");
    }

    #[test]
    fn names_stay_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        for bad in ["../x", "a/b", "", ".hidden", SUMS, "a b"] {
            assert!(store.create_new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_an_unfinished_file_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut half = store.create_new("a.img").unwrap();
        half.write(b"abc").unwrap();
        drop(half);
        assert!(store.create_new("a.img").is_err());
        store.discard_unfinished("a.img").unwrap();
        store.discard_unfinished("a.img").unwrap();
        let mut whole = store.create_new("a.img").unwrap();
        whole.write(b"abc").unwrap();
        whole.finish().unwrap();
        assert!(store.discard_unfinished("a.img").is_err());
        store.verify("a.img").unwrap();
        assert!(store.discard_unfinished("../x").is_err());
    }
}
