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
/// The meta partition is 128 MiB on a Tron; it is read whole, so a larger one is refused.
const META_MAX_BYTES: u64 = 256 << 20;

/// Packed backup of a Tron: 19.5 GB measured on ours (62.6 GB flash), with room.
const BACKUP_BYTES: u64 = 25_000_000_000;
/// The unpacked vendor rootfs (16 GiB partition) for the checks plus the new rootfs image.
const WORK_BYTES: u64 = 2 * 17_179_869_184;
/// Room in the pack for the vendor parts and the small files: 25.4 MB measured on ours, with room.
const VENDOR_ROOM: u64 = 64 << 20;

pub struct Tron<P> {
    pub port: P,
}

/// An opened Tron; `compared` once this run read the start areas and the rootfs from it and found them equal to
/// the backup.
pub struct TronDisk<D> {
    disk: D,
    compared: bool,
}

/// Removes a work file with keys or vendor data when it goes out of scope, unless kept.
struct Removes(Option<PathBuf>);

impl Removes {
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for Removes {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A new work file readable by the owner only.
fn create_private(path: &std::path::Path) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .with_context(|| format!("create {}", path.display()))
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
    let len = sectors(meta)
        .checked_mul(SECTOR)
        .filter(|&n| n <= META_MAX_BYTES)
        .with_context(|| {
            format!(
                "{NO_SERIAL}: {} is larger than on a Tron",
                identity::META_PART
            )
        })?;
    let mut data = vec![0u8; len as usize];
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

/// The saved vendor rootfs unpacked anew into the work folder for ext4 reads; removed when the guard drops. The
/// file is checked against SHA256SUMS while it is unpacked, so nothing left in the folder is ever used.
fn unpacked_rootfs(ctx: &Ctx<'_>, b: &Backup) -> Result<(PathBuf, Removes)> {
    let path = ctx.work.path.join("vendor-rootfs.img");
    let want = sectors(part(&b.table, "rootfs")?) * SECTOR;
    std::fs::create_dir_all(&ctx.work.path)?;
    for old in [&path, &path.with_extension("part")] {
        match std::fs::remove_file(old) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    let tmp = path.with_extension("part");
    let tmp_guard = Removes(Some(tmp.clone()));
    let mut out = create_private(&tmp)?;
    std::io::copy(&mut open_raw(ctx.store, &file_name("rootfs"))?, &mut out)?;
    out.sync_all()?;
    ensure!(
        out.metadata()?.len() == want,
        "the saved rootfs has the wrong size"
    );
    std::fs::rename(&tmp, &path)?;
    tmp_guard.keep();
    Ok((path.clone(), Removes(Some(path))))
}

impl<P: Port> Mower for Tron<P> {
    type Device = TronDisk<P::Disk>;

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

    fn check_wifi(&self, name: &str, key: &str) -> Result<()> {
        ensure!(
            (1..=32).contains(&name.len()),
            "a Wi-Fi name has 1 to 32 bytes"
        );
        contents::psk(key).map(|_| ())
    }

    fn check_inputs(&self, base: &BaseImage, inputs: &Inputs) -> Result<()> {
        let mut bundles = 0u64;
        for path in &inputs.bundles {
            bundles += std::fs::metadata(path)?.len();
        }
        let room = image::room(base);
        ensure!(
            bundles.saturating_add(VENDOR_ROOM) <= room,
            "the bundles take {} MB, the pack of this base system holds {} MB with the vendor parts",
            bundles >> 20,
            room.saturating_sub(VENDOR_ROOM) >> 20
        );
        Ok(())
    }

    async fn detect(&self) -> Result<Option<Found>> {
        self.port.find().await
    }

    async fn open(&self, found: &Found, ctx: &mut Ctx<'_>) -> Result<(Self::Device, Identity)> {
        let loader = tables::loader(&ctx.tables.raw)?;
        let mut disk = self.port.open(found, &loader.sha256).await?;
        let table = read_table(&mut disk).await?;
        // Only a known table is read further: the serial lies in one of its partitions.
        let known = tables::parse(&ctx.tables.raw)?;
        ensure!(
            known.tables.contains(&table) && table.iter().all(|p| p.last_sector < disk.sectors()),
            "tron.table_unknown: the partition table of this Tron is not one the installer knows"
        );
        let serial = serial(&mut disk, &table).await?;
        let identity = Identity {
            model: "Airseekers Tron".into(),
            board: None,
            serial_hash: identity::serial_hash(&serial),
            vendor_version: None,
            flash_sectors: disk.sectors(),
            table,
        };
        Ok((
            TronDisk {
                disk,
                compared: false,
            },
            identity,
        ))
    }

    async fn backup(&self, dev: &mut Self::Device, ctx: &mut Ctx<'_>) -> Result<Backup> {
        let table = read_table(&mut dev.disk).await?;
        let files =
            backup::backup(&mut dev.disk, &table, ctx.store, ctx.cancel, ctx.progress).await?;
        dev.compared = true;
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
        if read_table(&mut dev.disk).await? != b.table {
            return Ok(vec![Finding::Stop {
                code: "tron.table_changed".into(),
                details: String::new(),
            }]);
        }
        // A backup finished in an earlier run: the Tron may have started or been updated since.
        if !dev.compared {
            ctx.progress.step("tron.compare_device");
            // A system this folder wrote differs from the backup on purpose; the boot areas still have to match.
            let skip: &[&str] = if ctx.system_written { &["rootfs"] } else { &[] };
            if let Some(area) =
                backup::differs_from_device(&mut dev.disk, b, skip, ctx.cancel, ctx.progress)
                    .await?
            {
                return Ok(vec![Finding::Stop {
                    code: "tron.device_changed".into(),
                    details: area,
                }]);
            }
            dev.compared = true;
        }
        let known = tables::parse(&ctx.tables.raw)?;
        let start = saved(ctx.store, "flash-start")?;
        let boot = saved(ctx.store, "boot")?;
        let (rootfs, _removes) = unpacked_rootfs(ctx, b)?;
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
        let (vendor_path, removes_vendor) = unpacked_rootfs(ctx, b)?;
        let vendor = rootfs::open(&vendor_path, 0)?;
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let archive = contents::build(&vendor, &known, inputs, mtime)?;
        drop(vendor);
        let filled = pack::pack(archive, image::room(base))?;
        let out = ctx.work.path.join("mowbite-rootfs.img");
        if out.exists() {
            std::fs::remove_file(&out)?;
        }
        let removes_out = Removes(Some(out.clone()));
        let sha256 = image::fill_base(base, &filled, &out)?;
        drop(removes_vendor);
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
            "the new system does not fill the rootfs partition exactly"
        );
        removes_out.keep();
        Ok(plan)
    }

    /// The image holds the Wi-Fi keys: it is removed once it is on the Tron and read back; writing again goes
    /// through prepare().
    async fn write(
        &self,
        mut dev: Self::Device,
        plan: &WritePlan,
        ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        let table = read_table(&mut dev.disk).await.context(NothingWritten)?;
        let target = part(&table, &plan.partition).context(NothingWritten)?;
        ensure!(
            target.first_sector == plan.first_sector && sectors(target) == plan.partition_sectors,
            anyhow!(
                "tron.table_changed: {} is not where the plan writes",
                plan.partition
            )
            .context(NothingWritten)
        );
        let mut src = File::open(&plan.image).context(NothingWritten)?;
        let disk =
            image::write_and_verify(dev.disk, &mut src, plan, ctx.cancel, ctx.progress).await?;
        drop(src);
        if let Err(e) = std::fs::remove_file(&plan.image) {
            ctx.log.info(
                "tron.work_image_kept",
                &format!("{}: {e}", plan.image.display()),
            );
        }
        self.port.restart(disk).await.context(WrittenNotRestarted)
    }

    async fn start_seen(&self) -> Result<StartSeen> {
        Ok(match self.port.find().await? {
            Some(found) => StartSeen::UsbAgain(found),
            None => StartSeen::NotSeen,
        })
    }

    async fn back(&self, mut dev: Self::Device, b: &Backup, ctx: &mut Ctx<'_>) -> Result<()> {
        let table = read_table(&mut dev.disk).await.context(NothingWritten)?;
        back::belongs_here(&mut dev.disk, &table, b)
            .await
            .context(NothingWritten)?;
        let (plan, mut src) = back::plan(ctx.store, b).context(NothingWritten)?;
        let disk =
            image::write_and_verify(dev.disk, &mut src, &plan, ctx.cancel, ctx.progress).await?;
        self.port.restart(disk).await.context(WrittenNotRestarted)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::run::{Driver, Goal, Next};
    use crate::secret::Secret;
    use crate::testing::{Quiet, World};
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
        // As large as the rootfs partition of the small flash.
        let mut img = vec![0u8; (FS * SECTOR) as usize];
        img[1 << 19..(1 << 19) + pack::EMPTY.len()].copy_from_slice(pack::EMPTY);
        let path = dir.join("base.img");
        std::fs::write(&path, &img).unwrap();
        BaseImage {
            path,
            sha256: format!("{:x}", Sha256::digest(&img)),
            pack: vec![(1 << 19, 1 << 19), (3 << 19, 1 << 19)],
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
        assert!(!d.back_in_usb().await.unwrap());
        tron.port.started.set(false);
        assert!(d.back_in_usb().await.unwrap());
        d.choose(true).unwrap();
        let next = w.until_asked(&mut d).await.unwrap();
        let now = std::fs::read(&path).unwrap();
        let diff = now.iter().zip(&before).position(|(a, b)| a != b);
        assert!(now == before, "back differs at {diff:?}");
        assert_eq!(next, Next::Done);
    }

    fn ctx(w: &mut World) -> Ctx<'_> {
        Ctx {
            store: &mut w.store,
            work: &w.work,
            tables: &w.tables,
            progress: &Quiet,
            cancel: &w.cancel,
            log: &Quiet,
            system_written: false,
        }
    }

    fn file_tron(path: &Path) -> Tron<FilePort> {
        Tron {
            port: FilePort {
                path: path.to_path_buf(),
                read_only: false,
                started: Default::default(),
            },
        }
    }

    fn flip_last_rootfs_byte(path: &Path) {
        let mut disk = std::fs::read(path).unwrap();
        disk[rootfs_range().end - 1] ^= 1;
        std::fs::write(path, &disk).unwrap();
    }

    #[tokio::test]
    async fn a_backup_of_an_earlier_run_is_compared_with_the_tron() {
        let mut w = World::new();
        let (path, section) = flash(w.dir.path());
        w.tables = Tables { raw: section };
        let tron = file_tron(&path);
        let found = tron.detect().await.unwrap().unwrap();
        let (mut dev, _) = tron.open(&found, &mut ctx(&mut w)).await.unwrap();
        let b = tron.backup(&mut dev, &mut ctx(&mut w)).await.unwrap();
        drop(dev);
        // A stale work file of the right size is never used.
        std::fs::create_dir_all(&w.work.path).unwrap();
        let stale = w.work.path.join("vendor-rootfs.img");
        std::fs::write(&stale, vec![0u8; (FS * SECTOR) as usize]).unwrap();
        let (mut dev, _) = tron.open(&found, &mut ctx(&mut w)).await.unwrap();
        let found_now = tron.check(&mut dev, &b, &mut ctx(&mut w)).await.unwrap();
        assert!(
            !found_now.iter().any(|f| matches!(f, Finding::Stop { .. })),
            "{found_now:?}"
        );
        assert!(
            !stale.exists(),
            "the vendor rootfs stays in the work folder"
        );
        drop(dev);
        // The Tron started its system in between.
        flip_last_rootfs_byte(&path);
        let (mut dev, _) = tron.open(&found, &mut ctx(&mut w)).await.unwrap();
        assert_eq!(
            tron.check(&mut dev, &b, &mut ctx(&mut w)).await.unwrap(),
            vec![Finding::Stop {
                code: "tron.device_changed".into(),
                details: "rootfs".into()
            }]
        );
        let e = tron.backup(&mut dev, &mut ctx(&mut w)).await.unwrap_err();
        assert!(
            format!("{e:#}").contains("changed since this backup"),
            "{e:#}"
        );
    }

    #[tokio::test]
    async fn the_work_image_with_the_keys_is_gone_after_the_write() {
        let mut w = World::new();
        let (path, section) = flash(w.dir.path());
        w.tables = Tables { raw: section };
        w.base = base(w.dir.path());
        w.inputs.setup_wifi_key = Secret::new("setup-key-1".into());
        let tron = file_tron(&path);
        let mut d = Driver::open(&tron, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        let left: Vec<_> = std::fs::read_dir(&w.work.path).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn an_unknown_table_or_a_large_meta_is_not_read() {
        let mut w = World::new();
        let (path, section) = flash(w.dir.path());
        let other = String::from_utf8(section)
            .unwrap()
            .replace(r#""name":"oem""#, r#""name":"oem2""#);
        w.tables = Tables {
            raw: other.into_bytes(),
        };
        let tron = file_tron(&path);
        let found = tron.detect().await.unwrap().unwrap();
        let Err(e) = tron.open(&found, &mut ctx(&mut w)).await else {
            panic!("an unknown table was opened");
        };
        assert!(format!("{e:#}").starts_with("tron.table_unknown"), "{e:#}");
        let big = Part {
            name: identity::META_PART.into(),
            first_sector: 0,
            last_sector: (META_MAX_BYTES / SECTOR) * 2,
        };
        let mut dev = tron.port.open(&found, "").await.unwrap();
        let e = serial(&mut dev, &[big]).await.unwrap_err();
        assert!(format!("{e:#}").starts_with(NO_SERIAL), "{e:#}");
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
