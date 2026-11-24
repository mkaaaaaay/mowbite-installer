//! The pack: everything that belongs to one device, written raw into a file the base image reserves for it. A unit
//! on the first start checks it, unpacks it over / and overwrites the file with the empty mark again.
//!
//! Layout: one 512-byte header block of text ("MOWBITE-PACK 1", "len <bytes>", "sha256 <hex>", zero padded), then
//! a ustar archive of `len` bytes, then zeros up to the end of the file. The empty file starts with EMPTY.

use super::rootfs::{self, Kind};
use anyhow::{Result, bail, ensure};
use ext4_view::Ext4;
use sha2::{Digest, Sha256};

pub const BLOCK: usize = 512;
pub const MAGIC: &str = "MOWBITE-PACK 1\n";
pub const EMPTY: &[u8] = b"MOWBITE-PACK-EMPTY 1\n";

/// A ustar archive built in memory; paths are relative to / without a leading slash.
pub struct Archive {
    data: Vec<u8>,
    mtime: u64,
}

fn octal(field: &mut [u8], value: u64) -> Result<()> {
    let digits = format!("{value:0w$o}", w = field.len() - 1);
    ensure!(
        digits.len() < field.len(),
        "{value} does not fit a tar field"
    );
    field[..digits.len()].copy_from_slice(digits.as_bytes());
    Ok(())
}

/// ustar keeps up to 100 bytes of name and 155 of prefix, split at a slash.
fn split_path(path: &str) -> Result<(&str, &str)> {
    if path.len() <= 100 {
        return Ok(("", path));
    }
    for (i, _) in path.match_indices('/') {
        if i <= 155 && path.len() - i - 1 <= 100 && i > 0 {
            return Ok((&path[..i], &path[i + 1..]));
        }
    }
    bail!("path too long for the pack: {path}")
}

impl Archive {
    pub fn new(mtime: u64) -> Self {
        Archive {
            data: Vec::new(),
            mtime,
        }
    }

    fn header(
        &mut self,
        path: &str,
        kind: u8,
        mode: u16,
        owner: (u32, u32),
        size: u64,
        link: &str,
    ) -> Result<()> {
        let plain = path.strip_suffix('/').unwrap_or(path);
        ensure!(
            plain
                .split('/')
                .all(|c| !c.is_empty() && c != ".." && c != "."),
            "bad path in the pack: {path}"
        );
        ensure!(link.len() <= 100, "link target too long: {link}");
        let (prefix, name) = split_path(path)?;
        let mut h = [0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        octal(&mut h[100..108], u64::from(mode & 0o7777))?;
        octal(&mut h[108..116], u64::from(owner.0))?;
        octal(&mut h[116..124], u64::from(owner.1))?;
        octal(&mut h[124..136], size)?;
        octal(&mut h[136..148], self.mtime)?;
        h[156] = kind;
        h[157..157 + link.len()].copy_from_slice(link.as_bytes());
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        octal(&mut h[148..155], u64::from(sum))?;
        self.data.extend_from_slice(&h);
        Ok(())
    }

    pub fn dir(&mut self, path: &str, mode: u16, owner: (u32, u32)) -> Result<()> {
        self.header(&format!("{path}/"), b'5', mode, owner, 0, "")
    }

    pub fn file(&mut self, path: &str, mode: u16, owner: (u32, u32), content: &[u8]) -> Result<()> {
        self.header(path, b'0', mode, owner, content.len() as u64, "")?;
        self.data.extend_from_slice(content);
        self.data.resize(self.data.len().next_multiple_of(BLOCK), 0);
        Ok(())
    }

    pub fn link(&mut self, path: &str, target: &str, owner: (u32, u32)) -> Result<()> {
        self.header(path, b'2', 0o777, owner, 0, target)
    }

    /// A vendor part with everything below it, owned by root and without write access for group and others, as
    /// the image of our own Tron has them.
    pub fn vendor_part(&mut self, fs: &Ext4, part: &str) -> Result<()> {
        for e in rootfs::collect(fs, part)? {
            let path = e.path.trim_start_matches('/');
            let mode = e.mode & !0o022;
            match &e.kind {
                Kind::Dir => self.dir(path, mode, (0, 0))?,
                Kind::File { sha256, .. } => {
                    let data = rootfs::read(fs, &e.path)?;
                    ensure!(
                        format!("{:x}", Sha256::digest(&data)) == *sha256,
                        "{} changed while reading",
                        e.path
                    );
                    self.file(path, mode, (0, 0), &data)?
                }
                Kind::Link { target } => self.link(path, target, (0, 0))?,
            }
        }
        Ok(())
    }

    /// The archive with its end mark.
    pub fn finish(mut self) -> Vec<u8> {
        self.data.extend_from_slice(&[0u8; 2 * BLOCK]);
        self.data
    }
}

/// Header block plus archive, zero padded to `room` bytes (the size of the reserved file).
pub fn pack(archive: Vec<u8>, room: u64) -> Result<Vec<u8>> {
    let mut head = format!(
        "{MAGIC}len {}\nsha256 {:x}\n",
        archive.len(),
        Sha256::digest(&archive)
    )
    .into_bytes();
    ensure!(head.len() <= BLOCK, "pack header too long");
    head.resize(BLOCK, 0);
    let total = (BLOCK + archive.len()) as u64;
    ensure!(
        total <= room,
        "pack needs {total} bytes, the base image has room for {room}"
    );
    let mut out = head;
    out.extend_from_slice(&archive);
    out.resize(room as usize, 0);
    Ok(out)
}

/// Whether the reserved file still holds the empty mark; anything else is never overwritten.
pub fn is_empty_mark(first_block: &[u8]) -> bool {
    first_block.starts_with(EMPTY)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tron::rootfs::tests::{GOOD_VERMAGIC, ext4_from, vendor_tree};
    use std::process::Command;

    fn tar_list(archive: &[u8]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.tar");
        std::fs::write(&f, archive).unwrap();
        let out = Command::new("tar")
            .args(["-tvf"])
            .arg(&f)
            .args(["--numeric-owner"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn gnu_tar_reads_what_we_write() {
        let mut a = Archive::new(1_700_000_000);
        a.dir("etc/ssh", 0o755, (0, 0)).unwrap();
        a.file("etc/ssh/ssh_host_ed25519_key", 0o600, (0, 0), b"key")
            .unwrap();
        a.link("usr/lib/librga.so", "librga.so.2", (0, 0)).unwrap();
        let long = format!("usr/lib/modules/5.10.209/{}/x.ko", "d".repeat(90));
        a.file(&long, 0o644, (0, 0), &[7u8; 700]).unwrap();
        let list = tar_list(&a.finish());
        let lines: Vec<&str> = list.lines().collect();
        assert!(
            lines[0].starts_with("drwxr-xr-x 0/0") && lines[0].ends_with("etc/ssh/"),
            "{list}"
        );
        assert!(
            lines[1].starts_with("-rw------- 0/0")
                && lines[1].ends_with("etc/ssh/ssh_host_ed25519_key"),
            "{list}"
        );
        assert!(
            lines[2].ends_with("usr/lib/librga.so -> librga.so.2"),
            "{list}"
        );
        assert!(
            lines[3].contains(" 700 ") && lines[3].ends_with(&long),
            "{list}"
        );
    }

    #[test]
    fn vendor_parts_come_owned_by_root_without_group_write() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        vendor_tree(&tree, GOOD_VERMAGIC);
        let img = dir.path().join("rootfs.img");
        ext4_from(&tree, &img);
        let fs = rootfs::open(&img, 0).unwrap();
        let mut a = Archive::new(0);
        a.vendor_part(&fs, "/etc/iqfiles").unwrap();
        a.vendor_part(&fs, "/usr/lib/aarch64-linux-gnu/librga.so.2")
            .unwrap();
        let list = tar_list(&a.finish());
        assert!(
            list.lines().next().unwrap().starts_with("drwxr-xr-x 0/0"),
            "{list}"
        );
        assert!(list.contains("-rw-r--r-- 0/0"), "{list}");
        assert!(list.contains("librga.so.2 -> librga.so.2.1.0"), "{list}");
    }

    #[test]
    fn the_pack_names_length_and_hash_and_fits_its_room() {
        let mut a = Archive::new(0);
        a.file("x", 0o644, (0, 0), b"abc").unwrap();
        let archive = a.finish();
        let p = pack(archive.clone(), 64 * 1024).unwrap();
        assert_eq!(p.len(), 64 * 1024);
        let head = String::from_utf8_lossy(&p[..BLOCK]);
        assert!(
            head.starts_with(&format!("{MAGIC}len {}\nsha256 ", archive.len())),
            "{head}"
        );
        assert_eq!(&p[BLOCK..BLOCK + archive.len()], &archive[..]);
        assert!(p[BLOCK + archive.len()..].iter().all(|&b| b == 0));
        assert!(pack(archive, 1024).is_err());
        assert!(!is_empty_mark(&p[..BLOCK]));
        assert!(is_empty_mark(b"MOWBITE-PACK-EMPTY 1\n\0\0"));
    }

    #[test]
    fn paths_that_leave_the_root_are_refused() {
        let mut a = Archive::new(0);
        assert!(a.file("/etc/x", 0o644, (0, 0), b"").is_err());
        assert!(a.file("etc/../x", 0o644, (0, 0), b"").is_err());
        assert!(a.file(&"a".repeat(300), 0o644, (0, 0), b"").is_err());
    }
}
