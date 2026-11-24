//! Who a Tron is. The serial comes from the small vendor partition "userdata" (mounted as /meta), which the
//! installer never writes, so the same Tron is recognised again after MowBite OS is on it. The vendor stack
//! writes /meta/sn from the hostname and /meta/mac_addr from wlan0 on every update.

use anyhow::{Result, bail};
use ext4_view::Ext4;
use sha2::{Digest, Sha256};

/// GPT name of the partition holding sn and mac_addr.
pub const META_PART: &str = "userdata";

/// Where /workspace points on a vendor rootfs; the folder name is the vendor version.
const UPGRADE: &str = "/userdata/RobotData/upgrade/";

fn plain(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_graphic())).then_some(s)
}

/// The serial with its source ("sn:…" or, when sn is missing, "mac:…").
pub fn serial(meta: &Ext4) -> Result<String> {
    for (file, kind) in [("/sn", "sn"), ("/mac_addr", "mac")] {
        if let Ok(s) = meta.read_to_string(file) {
            match plain(&s) {
                Some(v) => return Ok(format!("{kind}:{}", v.to_ascii_uppercase())),
                None => bail!("{file} on {META_PART} is not a plain value"),
            }
        }
    }
    bail!("neither sn nor mac_addr on {META_PART}")
}

/// Shortened sha256 for logs and the run file; the plain serial stays in the backup.
pub fn serial_hash(serial: &str) -> String {
    format!("{:x}", Sha256::digest(serial.as_bytes()))[..16].to_string()
}

/// The vendor version from /workspace, None on a rootfs without it (MowBite OS) or with an odd target.
pub fn vendor_version(rootfs: &Ext4) -> Option<String> {
    let target = rootfs.read_link("/workspace").ok()?;
    let v = target.to_str().ok()?.strip_prefix(UPGRADE)?;
    (!v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)))
    .then(|| v.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tron::rootfs::{
        self,
        tests::{ext4_from, ext4_small_inodes},
    };
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn fs_of(build: impl FnOnce(&Path)) -> (tempfile::TempDir, Ext4) {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        build(&tree);
        let img = dir.path().join("p.img");
        ext4_from(&tree, &img);
        let fs = rootfs::open(&img, 0).unwrap();
        (dir, fs)
    }

    #[test]
    fn serial_from_sn_then_mac() {
        let (_d, fs) = fs_of(|t| {
            std::fs::write(t.join("sn"), "1234567890abcdef\n").unwrap();
            std::fs::write(t.join("mac_addr"), "aa:bb:cc:dd:ee:ff\n").unwrap();
        });
        assert_eq!(serial(&fs).unwrap(), "sn:1234567890ABCDEF");
        let (_d, fs) = fs_of(|t| std::fs::write(t.join("mac_addr"), "aa:bb:cc:dd:ee:ff").unwrap());
        assert_eq!(serial(&fs).unwrap(), "mac:AA:BB:CC:DD:EE:FF");
        let (_d, fs) = fs_of(|_| {});
        assert!(serial(&fs).is_err());
        let (_d, fs) = fs_of(|t| std::fs::write(t.join("sn"), "12 34\n").unwrap());
        assert!(serial(&fs).is_err());
        let (_d, fs) = fs_of(|t| std::fs::write(t.join("sn"), "\n").unwrap());
        assert!(serial(&fs).is_err());
    }

    /// Byte offset of the inode of `path` in a test image with 1 KiB blocks, from debugfs imap.
    fn inode_offset(image: &Path, path: &str) -> usize {
        let out = std::process::Command::new("/usr/sbin/debugfs")
            .args(["-R", &format!("imap {path}")])
            .arg(image)
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let rest = text.split("located at block ").nth(1).unwrap();
        let (block, rest) = rest.split_once(", offset 0x").unwrap();
        let off = usize::from_str_radix(rest.trim(), 16).unwrap();
        block.parse::<usize>().unwrap() * 1024 + off
    }

    #[test]
    fn serial_from_a_partition_with_small_inodes() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("sn"), "1234567890123456\n").unwrap();
        let img = dir.path().join("meta.img");
        ext4_small_inodes(&tree, &img);
        let fs = rootfs::open(&img, 0).unwrap();
        assert_eq!(serial(&fs).unwrap(), "sn:1234567890123456");
        let mut bytes = std::fs::read(&img).unwrap();
        let at = inode_offset(&img, "/sn") + 0x10;
        bytes[at] ^= 1;
        std::fs::write(&img, bytes).unwrap();
        let fs = rootfs::open(&img, 0).unwrap();
        assert!(
            serial(&fs).is_err(),
            "a changed inode must fail its checksum"
        );
    }

    #[test]
    fn hash_is_short_and_stable() {
        let h = serial_hash("sn:1");
        assert_eq!(h.len(), 16);
        assert_eq!(h, serial_hash("sn:1"));
        assert_ne!(h, serial_hash("sn:2"));
    }

    #[test]
    fn vendor_version_from_workspace() {
        let (_d, fs) = fs_of(|t| {
            symlink(
                "/userdata/RobotData/upgrade/v1.3.32-tron.rc.8",
                t.join("workspace"),
            )
            .unwrap()
        });
        assert_eq!(vendor_version(&fs).as_deref(), Some("v1.3.32-tron.rc.8"));
        let (_d, fs) = fs_of(|_| {});
        assert_eq!(vendor_version(&fs), None);
        let (_d, fs) = fs_of(|t| symlink("/opt/elsewhere/v1", t.join("workspace")).unwrap());
        assert_eq!(vendor_version(&fs), None);
        let (_d, fs) =
            fs_of(|t| symlink("/userdata/RobotData/upgrade/v1/../x", t.join("workspace")).unwrap());
        assert_eq!(vendor_version(&fs), None);
    }
}
