//! The Tron as a mower of the installer: backup of the whole flash, checks on the backup, a rootfs with the pack
//! of this device, written into the rootfs partition only.

use super::backup::{self, file_name, open_raw};
use super::port::Port;
use super::{back, checks, contents, gpt, identity, image, pack, rootfs, tables, texts};
use crate::disk::{Disk, SECTOR};
use crate::mower::*;
use anyhow::{Context, Result, anyhow, ensure};
use std::fs::File;
use std::path::PathBuf;

const NO_SERIAL: &str = "tron.no_serial";

/// Packed backup of a Tron: 19.5 GB measured on ours (62.6 GB flash), with room.
const BACKUP_BYTES: u64 = 25_000_000_000;
/// The unpacked vendor rootfs (16 GiB partition) for the checks plus the new rootfs image.
const WORK_BYTES: u64 = 2 * 17_179_869_184;

pub struct Tron<P> {
    pub port: P,
}

async fn read_table<D: Disk>(disk: &mut D) -> Result<Vec<Part>> {
    let mut start = vec![0u8; gpt::PRIMARY_SECTORS * gpt::SECTOR];
    disk.read(0, &mut start).await?;
    Ok(gpt::parse_primary(&start)?
        .into_iter()
        .map(|e| e.part)
        .collect())
}

fn part<'a>(table: &'a [Part], name: &str) -> Result<&'a Part> {
    table
        .iter()
        .find(|p| p.name == name)
        .with_context(|| format!("no partition {name}"))
}

fn sectors(p: &Part) -> u64 {
    p.last_sector - p.first_sector + 1
}

/// The serial from sn or mac_addr on the meta partition; only those two files are read.
async fn serial<D: Disk>(disk: &mut D, table: &[Part]) -> Result<String> {
    let meta = part(table, identity::META_PART)?;
    let mut data = vec![0u8; (sectors(meta) * SECTOR) as usize];
    disk.read(meta.first_sector, &mut data).await?;
    let fs = ext4_view::Ext4::load(Box::new(data)).map_err(|e| {
        anyhow!(
            "{NO_SERIAL}: {} is no readable ext4: {e}",
            identity::META_PART
        )
    })?;
    identity::serial(&fs).map_err(|e| anyhow!("{NO_SERIAL}: {e:#}"))
}

fn saved(store: &crate::store::Store, area: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut open_raw(store, &file_name(area))?, &mut out)?;
    Ok(out)
}

/// The saved vendor rootfs unpacked into the work folder, for ext4 reads; kept until prepare() is done.
fn unpacked_rootfs(ctx: &Ctx<'_>, b: &Backup) -> Result<PathBuf> {
    let path = ctx.work.path.join("vendor-rootfs.img");
    let want = sectors(part(&b.table, "rootfs")?) * SECTOR;
    if path.metadata().is_ok_and(|m| m.len() == want) {
        return Ok(path);
    }
    std::fs::create_dir_all(&ctx.work.path)?;
    let tmp = path.with_extension("part");
    let mut out = File::create(&tmp)?;
    std::io::copy(&mut open_raw(ctx.store, &file_name("rootfs"))?, &mut out)?;
    out.sync_all()?;
    ensure!(
        out.metadata()?.len() == want,
        "the saved rootfs has the wrong size"
    );
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

impl<P: Port> Mower for Tron<P> {
    type Device = P::Disk;

    fn id(&self) -> &'static str {
        "tron"
    }

    fn guide(&self) -> Guide {
        Guide {
            steps: vec![
                ("tron.guide_battery", ""),
                ("tron.guide_camera", ""),
                ("tron.guide_cable", ""),
                ("tron.guide_button", ""),
            ],
        }
    }

    fn texts(&self) -> &'static [(&'static str, &'static str, &'static str)] {
        texts::TEXTS
    }

    fn space_needed(&self) -> Space {
        Space {
            backup_bytes: BACKUP_BYTES,
            work_bytes: WORK_BYTES,
        }
    }

    async fn detect(&self) -> Result<Option<Found>> {
        self.port.find().await
    }

    async fn open(&self, found: &Found, ctx: &mut Ctx<'_>) -> Result<(Self::Device, Identity)> {
        let loader = tables::loader(&ctx.tables.raw)?;
        let mut disk = self.port.open(found, &loader.sha256).await?;
        let table = read_table(&mut disk).await?;
        let serial = serial(&mut disk, &table).await?;
        let identity = Identity {
            model: "Airseekers Tron".into(),
            board: None,
            serial_hash: identity::serial_hash(&serial),
            vendor_version: None,
            flash_sectors: disk.sectors(),
            table,
        };
        Ok((disk, identity))
    }

    async fn backup(&self, dev: &mut Self::Device, ctx: &mut Ctx<'_>) -> Result<Backup> {
        let table = read_table(dev).await?;
        let files = backup::backup(dev, &table, ctx.store, ctx.cancel, ctx.progress).await?;
        let fingerprint = back::fingerprint(&files)?;
        Ok(Backup {
            table,
            files,
            fingerprint,
        })
    }

    async fn check(
        &self,
        dev: &mut Self::Device,
        b: &Backup,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Finding>> {
        if read_table(dev).await? != b.table {
            return Ok(vec![Finding::Stop {
                code: "tron.table_changed".into(),
                details: String::new(),
            }]);
        }
        let known = tables::parse(&ctx.tables.raw)?;
        let start = saved(ctx.store, "flash-start")?;
        let boot = saved(ctx.store, "boot")?;
        let rootfs = unpacked_rootfs(ctx, b)?;
        Ok(checks::check(&start, &boot, &rootfs, &known).findings)
    }

    async fn prepare(
        &self,
        b: &Backup,
        base: &BaseImage,
        inputs: &Inputs,
        ctx: &mut Ctx<'_>,
    ) -> Result<WritePlan> {
        let known = tables::parse(&ctx.tables.raw)?;
        let vendor_path = unpacked_rootfs(ctx, b)?;
        let vendor = rootfs::open(&vendor_path, 0)?;
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let archive = contents::build(&vendor, &known, inputs, mtime)?;
        drop(vendor);
        let filled = pack::pack(archive, base.pack_len)?;
        let out = ctx.work.path.join("mowbite-rootfs.img");
        if out.exists() {
            std::fs::remove_file(&out)?;
        }
        let sha256 = image::fill_base(base, &filled, &out)?;
        std::fs::remove_file(&vendor_path)?;
        let rootfs = part(&b.table, "rootfs")?;
        let plan = WritePlan {
            partition: rootfs.name.clone(),
            first_sector: rootfs.first_sector,
            sectors: out.metadata()?.len() / SECTOR,
            partition_sectors: sectors(rootfs),
            image: out,
            sha256,
        };
        ensure!(
            plan.fits(),
            "the new system is larger than the rootfs partition"
        );
        Ok(plan)
    }

    async fn write(&self, dev: Self::Device, plan: &WritePlan, ctx: &mut Ctx<'_>) -> Result<()> {
        let mut src = File::open(&plan.image)?;
        let disk = image::write_and_verify(dev, &mut src, plan, ctx.cancel, ctx.progress).await?;
        self.port.restart(disk).await
    }

    async fn start_seen(&self) -> Result<StartSeen> {
        Ok(match self.port.find().await? {
            Some(found) => StartSeen::UsbAgain(found),
            None => StartSeen::NotSeen,
        })
    }

    async fn back(&self, mut dev: Self::Device, b: &Backup, ctx: &mut Ctx<'_>) -> Result<()> {
        let table = read_table(&mut dev).await?;
        back::belongs_here(&mut dev, &table, b).await?;
        let (plan, mut src) = back::plan(ctx.store, b)?;
        let disk = image::write_and_verify(dev, &mut src, &plan, ctx.cancel, ctx.progress).await?;
        self.port.restart(disk).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::run::{Driver, Goal, Next};
    use crate::secret::Secret;
    use crate::testing::World;
    use crate::tron::fit::tests::{KERNEL, boot_partition};
    use crate::tron::gpt::tests::{ROOTFS_UUID, image as gpt_start};
    use crate::tron::port::FilePort;
    use crate::tron::rootfs::tests::{GOOD_VERMAGIC, ext4_from, ext4_small_inodes, vendor_tree};
    use sha2::{Digest, Sha256};
    use std::path::Path;

    const BOOTARGS: &str = "console=ttyFIQ0 root=PARTUUID=614e0000-0000 rw rootwait";
    /// 4 MiB, the size ext4_from makes.
    const FS: u64 = 8192;

    fn layout(boot_sectors: u64) -> Vec<(&'static str, u64, u64)> {
        let mut at = 64;
        [
            ("uboot", 64),
            ("misc", 64),
            ("boot", boot_sectors),
            ("recovery", 64),
            ("backup", 64),
            ("rootfs", FS),
            ("oem", 64),
            ("userdata", FS),
        ]
        .into_iter()
        .map(|(n, len)| {
            let p = (n, at, at + len - 1);
            at += len;
            p
        })
        .collect()
    }

    fn ext4(dir: &Path, name: &str, small_inodes: bool, fill: impl FnOnce(&Path)) -> Vec<u8> {
        let tree = dir.join(name);
        std::fs::create_dir_all(&tree).unwrap();
        fill(&tree);
        let img = dir.join(format!("{name}.img"));
        if small_inodes {
            ext4_small_inodes(&tree, &img);
        } else {
            ext4_from(&tree, &img);
        }
        std::fs::read(img).unwrap()
    }

    /// A flash like a Tron's, small: the returned section of the tables knows its partition table.
    fn flash(dir: &Path) -> (PathBuf, Vec<u8>) {
        let mut boot = boot_partition(KERNEL, BOOTARGS, false);
        boot.resize(boot.len().next_multiple_of(SECTOR as usize), 0);
        let parts = layout(boot.len() as u64 / SECTOR);
        let rootfs = ext4(dir, "rootfs", false, |t| vendor_tree(t, GOOD_VERMAGIC));
        let meta = ext4(dir, "meta", true, |t| {
            std::fs::write(t.join("sn"), "1234567890123456\n").unwrap()
        });
        let end = parts.last().unwrap().2 + 1 + 34;
        let mut disk = vec![0u8; (end * SECTOR) as usize];
        let entries: Vec<_> = parts
            .iter()
            .enumerate()
            .map(|(i, &(n, a, b))| {
                (
                    n,
                    a,
                    b,
                    if n == "rootfs" {
                        ROOTFS_UUID
                    } else {
                        [i as u8 + 1; 16]
                    },
                )
            })
            .collect();
        let start = gpt_start(&entries);
        disk[..start.len()].copy_from_slice(&start);
        for (name, a, _) in &parts {
            let data: &[u8] = match *name {
                "boot" => &boot,
                "rootfs" => &rootfs,
                "userdata" => &meta,
                _ => &[7u8; 512],
            };
            let at = (*a * SECTOR) as usize;
            disk[at..at + data.len()].copy_from_slice(data);
        }
        let path = dir.join("flash.img");
        std::fs::write(&path, &disk).unwrap();
        let table: Vec<_> = parts
            .iter()
            .map(|(n, a, b)| format!(r#"{{"name":"{n}","first_sector":{a},"last_sector":{b}}}"#))
            .collect();
        let section = format!(
            r#"{{"partition_tables":[[{}]],"required":["/usr/lib/modules/5.10.209"],"optional":["/etc/iqfiles"],"vendor_versions":["v1.1.1"],"loader":{{"file":"l.bin","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}}"#,
            table.join(",")
        );
        (path, section.into_bytes())
    }

    fn base(dir: &Path) -> BaseImage {
        let mut img = vec![0u8; 4096 * SECTOR as usize];
        img[1 << 19..(1 << 19) + pack::EMPTY.len()].copy_from_slice(pack::EMPTY);
        let path = dir.join("base.img");
        std::fs::write(&path, &img).unwrap();
        BaseImage {
            path,
            sha256: format!("{:x}", Sha256::digest(&img)),
            pack_offset: 1 << 19,
            pack_len: 1 << 20,
        }
    }

    /// Byte range of rootfs in the small flash.
    fn rootfs_range() -> std::ops::Range<usize> {
        let mut boot = boot_partition(KERNEL, BOOTARGS, false).len() as u64;
        boot = boot.div_ceil(SECTOR);
        let (_, a, b) = layout(boot)[5];
        (a * SECTOR) as usize..((b + 1) * SECTOR) as usize
    }

    #[tokio::test]
    async fn install_and_back_on_a_small_tron() {
        let mut w = World::new();
        let (path, section) = flash(w.dir.path());
        w.tables = Tables { raw: section };
        w.base = base(w.dir.path());
        w.inputs.setup_wifi_key = Secret::new("setup-key-1".into());
        let before = std::fs::read(&path).unwrap();

        let tron = Tron {
            port: FilePort {
                path: path.clone(),
                read_only: false,
                started: Default::default(),
            },
        };
        let mut d = Driver::open(&tron, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        let after = std::fs::read(&path).unwrap();
        let r = rootfs_range();
        assert!(after[r.clone()] != before[r.clone()], "rootfs unchanged");
        assert!(
            after[..r.start] == before[..r.start],
            "written before rootfs"
        );
        assert!(after[r.end..] == before[r.end..], "written after rootfs");
        let new = &after[r];
        let pack_head = &new[1 << 19..(1 << 19) + pack::MAGIC.len()];
        assert_eq!(pack_head, pack::MAGIC.as_bytes());
        // The Tron came back on USB instead of starting: go back to the vendor system.
        tron.port.started.set(false);
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::WriteOrBack);
        d.choose(true).unwrap();
        let next = w.until_asked(&mut d).await.unwrap();
        let now = std::fs::read(&path).unwrap();
        let diff = now.iter().zip(&before).position(|(a, b)| a != b);
        assert!(now == before, "back differs at {diff:?}");
        assert_eq!(next, Next::Done);
    }

    #[tokio::test]
    async fn a_tron_without_serial_is_not_opened() {
        let w = World::new();
        let (path, _) = flash(w.dir.path());
        let mut disk = std::fs::read(&path).unwrap();
        let (_, a, b) =
            *layout((boot_partition(KERNEL, BOOTARGS, false).len() as u64).div_ceil(SECTOR))
                .last()
                .unwrap();
        disk[(a * SECTOR) as usize..((b + 1) * SECTOR) as usize].fill(0);
        std::fs::write(&path, &disk).unwrap();
        let port = FilePort {
            path,
            read_only: true,
            started: Default::default(),
        };
        let found = port.find().await.unwrap().unwrap();
        let mut dev = port.open(&found, "").await.unwrap();
        let table = read_table(&mut dev).await.unwrap();
        let e = serial(&mut dev, &table).await.unwrap_err();
        assert!(format!("{e:#}").starts_with(NO_SERIAL), "{e:#}");
    }
}
