//! Reads the vendor rootfs from the backup without mounting it. ext4-view replays the journal in memory, so a
//! rootfs saved from a running system reads like after a clean shutdown; the backup file stays untouched.

use anyhow::{Context, Result, anyhow, bail};
use ext4_view::{Ext4, Ext4Read, FileType};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::Path;

pub const MODULES: &str = "/usr/lib/modules";

struct At {
    file: File,
    offset: u64,
}

impl Ext4Read for At {
    fn read(
        &mut self,
        start: u64,
        dst: &mut [u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use std::io::{Read, Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(self.offset + start))?;
        self.file.read_exact(dst)?;
        Ok(())
    }
}

/// Opens the ext4 file system that starts `offset` bytes into `image`.
pub fn open(image: &Path, offset: u64) -> Result<Ext4> {
    let file = File::open(image).with_context(|| format!("open {}", image.display()))?;
    Ext4::load(Box::new(At { file, offset }))
        .map_err(|e| anyhow!("{}: no readable ext4: {e}", image.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File { size: u64, sha256: String },
    Link { target: String },
}

/// One entry of a vendor part, with what is needed to put it back exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
}

impl Entry {
    /// Same line as the debugfs listing of the measurements: path, type, mode, uid, gid, size, sha256 or target.
    pub fn line(&self) -> String {
        let (t, size, last) = match &self.kind {
            Kind::Dir => ("d", "-".to_string(), "-".to_string()),
            Kind::File { size, sha256 } => ("f", size.to_string(), sha256.clone()),
            Kind::Link { target } => ("l", "-".to_string(), target.clone()),
        };
        format!(
            "{} {t} {:04o} {} {} {size} {last}",
            self.path,
            self.mode & 0o7777,
            self.uid,
            self.gid
        )
    }
}

fn entry(fs: &Ext4, path: &str) -> Result<Entry> {
    let m = fs
        .symlink_metadata(path)
        .with_context(|| format!("{path} missing"))?;
    let kind = match m.file_type() {
        FileType::Directory => Kind::Dir,
        FileType::Regular => {
            let data = fs.read(path).with_context(|| format!("read {path}"))?;
            Kind::File {
                size: data.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&data)),
            }
        }
        FileType::Symlink => Kind::Link {
            target: fs.read_link(path)?.display().to_string(),
        },
        other => bail!("{path} is a {other:?}, not a file, folder or link"),
    };
    Ok(Entry {
        path: path.to_string(),
        kind,
        mode: m.mode(),
        uid: m.uid(),
        gid: m.gid(),
    })
}

fn names(fs: &Ext4, dir: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for e in fs.read_dir(dir).with_context(|| format!("list {dir}"))? {
        let e = e?;
        let name = e.file_name();
        let name = name
            .as_str()
            .map_err(|_| anyhow!("name in {dir} is not UTF-8"))?;
        if name != "." && name != ".." {
            out.push(name.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// A vendor part and everything below it, folders before their contents. `part` is a path from the root.
pub fn collect(fs: &Ext4, part: &str) -> Result<Vec<Entry>> {
    let mut out = vec![entry(fs, part)?];
    if out[0].kind == Kind::Dir {
        for name in names(fs, part)? {
            out.extend(collect(fs, &format!("{part}/{name}"))?);
        }
    }
    Ok(out)
}

/// Folders under /usr/lib/modules; the vendor rootfs carries one for an older kernel next to the running one.
pub fn module_dirs(fs: &Ext4) -> Result<Vec<String>> {
    names(fs, MODULES)
}

/// The vermagic string of a kernel module ("5.10.209 SMP mod_unload aarch64"), None when it has none.
pub fn vermagic(module: &[u8]) -> Option<String> {
    const MARK: &[u8] = b"vermagic=";
    let at = module.windows(MARK.len()).position(|w| w == MARK)? + MARK.len();
    let end = module[at..].iter().position(|&c| c == 0)?;
    String::from_utf8(module[at..at + end].to_vec()).ok()
}

pub fn read(fs: &Ext4, path: &str) -> Result<Vec<u8>> {
    fs.read(path).with_context(|| format!("read {path}"))
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::process::Command;

    /// A small ext4 made by mke2fs from a folder (e2fsprogs, often only in /usr/sbin).
    pub fn ext4_from(dir: &Path, image: &Path) {
        let tool = ["/usr/sbin/mke2fs", "/sbin/mke2fs"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .unwrap_or("mke2fs");
        let ok = Command::new(tool)
            .args([
                "-q",
                "-t",
                "ext4",
                "-b",
                "1024",
                "-E",
                "root_owner=0:0",
                "-d",
            ])
            .arg(dir)
            .arg(image)
            .arg("4M")
            .status()
            .expect("these tests need mke2fs from e2fsprogs")
            .success();
        assert!(ok, "mke2fs failed");
    }

    /// A tree like the vendor parts: a module folder for the running and for an older kernel, a link, odd modes.
    pub fn vendor_tree(root: &Path, vermagic: &str) {
        let m = root.join("usr/lib/modules/5.10.209");
        std::fs::create_dir_all(&m).unwrap();
        std::fs::write(
            m.join("bcmdhd.ko"),
            format!("\x7fELF..vermagic={vermagic}\0author=x\0"),
        )
        .unwrap();
        std::fs::write(m.join("modules.dep"), "bcmdhd.ko:\n").unwrap();
        let old = root.join("usr/lib/modules/5.10.160");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join("bcmdhd.ko"),
            "vermagic=5.10.160 SMP mod_unload aarch64\0",
        )
        .unwrap();
        let lib = root.join("usr/lib/aarch64-linux-gnu");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("librga.so.2.1.0"), "lib").unwrap();
        symlink("librga.so.2.1.0", lib.join("librga.so.2")).unwrap();
        let iq = root.join("etc/iqfiles");
        std::fs::create_dir_all(&iq).unwrap();
        std::fs::write(iq.join("a.json"), "{}").unwrap();
        std::fs::set_permissions(iq.join("a.json"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        std::fs::set_permissions(&iq, std::fs::Permissions::from_mode(0o777)).unwrap();
    }

    pub const GOOD_VERMAGIC: &str = "5.10.209 SMP mod_unload aarch64";

    #[test]
    fn lists_parts_with_mode_owner_hash_and_link() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        vendor_tree(&tree, GOOD_VERMAGIC);
        let img = dir.path().join("rootfs.img");
        ext4_from(&tree, &img);
        let fs = open(&img, 0).unwrap();
        assert_eq!(module_dirs(&fs).unwrap(), ["5.10.160", "5.10.209"]);
        // mke2fs -d keeps the owner of the source files.
        let src = std::fs::metadata(tree.join("etc/iqfiles/a.json")).unwrap();
        let owner = format!("{} {}", src.uid(), src.gid());
        let iq: Vec<String> = collect(&fs, "/etc/iqfiles")
            .unwrap()
            .iter()
            .map(Entry::line)
            .collect();
        assert_eq!(iq[0], format!("/etc/iqfiles d 0777 {owner} - -"));
        assert_eq!(
            iq[1],
            format!(
                "/etc/iqfiles/a.json f 0644 {owner} 2 44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
            )
        );
        let link = collect(&fs, "/usr/lib/aarch64-linux-gnu/librga.so.2").unwrap();
        assert_eq!(
            link[0].kind,
            Kind::Link {
                target: "librga.so.2.1.0".into()
            }
        );
        let ko = read(&fs, "/usr/lib/modules/5.10.209/bcmdhd.ko").unwrap();
        assert_eq!(vermagic(&ko).as_deref(), Some(GOOD_VERMAGIC));
        assert!(collect(&fs, "/usr/lib/kernel/driver").is_err());
    }

    #[test]
    fn vermagic_needs_its_end() {
        assert_eq!(
            vermagic(b"x\0vermagic=5.10 SMP\0y"),
            Some("5.10 SMP".into())
        );
        assert_eq!(vermagic(b"vermagic=5.10 SMP"), None);
        assert_eq!(vermagic(b"nothing"), None);
    }
}
