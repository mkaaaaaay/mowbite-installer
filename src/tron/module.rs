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
/// The new rootfs image (16 GiB partition), the only work file; the vendor rootfs is read from the backup. Its zero
/// blocks stay holes, about 1 GB is written; this is the bound where the file system keeps no holes.
const WORK_BYTES: u64 = 17_179_869_184;
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

/// The vendor rootfs read straight from its backup file, frame by frame; the file passed the check against
/// SHA256SUMS first.
fn vendor_rootfs(ctx: &Ctx<'_>, b: &Backup) -> Result<ext4_view::Ext4> {
    let name = file_name("rootfs");
    let read = backup::open_seekable(ctx.store, &name)?;
    ensure!(
        read.len() == sectors(part(&b.table, "rootfs")?) * SECTOR,
        "the saved rootfs has the wrong size"
    );
    rootfs::load(read, &name)
}

/// GPT name of the vendor partition mounted as /userdata.
const OEM: &str = "oem";

fn saved_fs(ctx: &Ctx<'_>, area: &str) -> Result<ext4_view::Ext4> {
    let name = file_name(area);
    rootfs::load(backup::open_seekable(ctx.store, &name)?, &name)
}

/// Where the MowBite OS bundle keeps the Tron platform file with the measured board firmware.
const PLATFORM_YAML: &str = "install/platform_tron/share/platform_tron/platform.yaml";

/// The board firmware lists of the MowBite OS bundle in the release: the new system drives with these only.
fn firmware(inputs: &Inputs) -> Result<checks::Firmware> {
    let os = inputs
        .bundles
        .iter()
        .map(|b| &b.path)
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("mowbite-os-") && n.ends_with(".tar.gz"))
        })
        .context("no MowBite OS bundle in the release")?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(
        File::open(os).with_context(|| format!("open {}", os.display()))?,
    ));
    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.path()?.ends_with(PLATFORM_YAML) {
            let mut yaml = String::new();
            std::io::Read::read_to_string(&mut entry, &mut yaml)?;
            return checks::firmware_lists(&yaml).with_context(|| format!("{}", os.display()));
        }
    }
    anyhow::bail!("{}: no {PLATFORM_YAML}", os.display())
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

    fn before(&self) -> &'static str {
        "tron.before"
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
        for b in &inputs.bundles {
            bundles += std::fs::metadata(&b.path)?.len();
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

    async fn install_usb_driver(&self) -> Result<()> {
        super::usb_driver::install().await
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
        ctx.progress.step("tron.backup");
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
        inputs: &Inputs,
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
        // Opening a saved area hashes its whole file first: each one once.
        ctx.progress.step("tron.check_system");
        let vendor = vendor_rootfs(ctx, b);
        let version = vendor.as_ref().ok().and_then(identity::vendor_version);
        let mut c = checks::check(&start, &boot, vendor, &known);
        checks::check_firmware(
            &mut c,
            saved_fs(ctx, OEM),
            version.as_deref(),
            &firmware(inputs)?,
        );
        Ok(c.findings)
    }

    async fn prepare(
        &self,
        b: &Backup,
        base: &BaseImage,
        inputs: &Inputs,
        ctx: &mut Ctx<'_>,
    ) -> Result<WritePlan> {
        let known = tables::parse(&ctx.tables.raw)?;
        let vendor = vendor_rootfs(ctx, b)?;
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let archive = contents::build(&vendor, &known, inputs, mtime)?;
        drop(vendor);
        let filled = pack::pack(archive, image::room(base))?;
        std::fs::create_dir_all(&ctx.work.path)?;
        let out = ctx.work.path.join("mowbite-rootfs.img");
        if out.exists() {
            std::fs::remove_file(&out)?;
        }
        let removes_out = Removes(Some(out.clone()));
        let sha256 = image::fill_base(base, &filled, &out)?;
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
        ctx.progress.step("tron.check_backup");
        let table = read_table(&mut dev.disk).await.context(NothingWritten)?;
        back::belongs_here(&mut dev.disk, &table, b)
            .await
            .context(NothingWritten)?;
        let (plan, mut src) = back::plan(ctx.store, b, ctx.progress).context(NothingWritten)?;
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
            ("oem", FS),
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
        // The vendor stack of v1.1.1 flashes the measured board firmware.
        let oem = ext4(dir, "oem", true, |t| {
            let bin = t.join("RobotData/upgrade/v1.1.1/mcu_bin");
            for (board, v) in [("chassis", "0.6.34"), ("cutter", "0.6.36")] {
                std::fs::create_dir_all(bin.join(board)).unwrap();
                std::os::unix::fs::symlink(format!("{v}.bin"), bin.join(board).join("latest.bin"))
                    .unwrap();
            }
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
                "oem" => &oem,
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
        let mut w = world();
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

    /// An OS bundle whose platform file lists `chassis` for the motor board and 0.6.36 for the blade board.
    fn os_bundle(dir: &Path, chassis: &str) -> Bundle {
        let yaml = format!(
            "tron:\n  ros__parameters:\n    cutter_firmware: [\"0.6.36\"]\n    chassis_firmware: [{chassis}]\n"
        );
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut h = tar::Header::new_gnu();
        h.set_size(yaml.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(
            &mut h,
            format!("0123456789ab/{PLATFORM_YAML}"),
            yaml.as_bytes(),
        )
        .unwrap();
        let gz = tar.into_inner().unwrap().finish().unwrap();
        let path = dir.join("mowbite-os-0123456789ab.tar.gz");
        std::fs::write(&path, &gz).unwrap();
        Bundle {
            path,
            sha256: format!("{:x}", Sha256::digest(&gz)),
        }
    }

    fn world() -> World {
        let mut w = World::new();
        w.inputs.bundles = vec![os_bundle(w.dir.path(), r#""0.6.34""#)];
        w
    }

    fn inputs(bundle: Bundle) -> Inputs {
        Inputs {
            wifi_name: "Heim".into(),
            wifi_key: Secret::new("geheim123".into()),
            device_name: "mowbite-ab12".into(),
            setup_wifi_key: Secret::new("x".into()),
            install_id: Secret::new("0123456789abcdef0123456789abcdef".into()),
            bundles: vec![bundle],
        }
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
        let mut w = world();
        let (path, section) = flash(w.dir.path());
        w.tables = Tables { raw: section };
        let tron = file_tron(&path);
        let found = tron.detect().await.unwrap().unwrap();
        let (mut dev, _) = tron.open(&found, &mut ctx(&mut w)).await.unwrap();
        let b = tron.backup(&mut dev, &mut ctx(&mut w)).await.unwrap();
        drop(dev);
        let (mut dev, _) = tron.open(&found, &mut ctx(&mut w)).await.unwrap();
        let listed = inputs(os_bundle(w.dir.path(), r#""0.6.34""#));
        let found_now = tron
            .check(&mut dev, &b, &listed, &mut ctx(&mut w))
            .await
            .unwrap();
        assert!(
            !found_now.iter().any(|f| matches!(f, Finding::Stop { .. })),
            "{found_now:?}"
        );
        assert!(
            !w.work.path.exists(),
            "the checks read the backup, nothing goes into the work folder"
        );
        // The bundle drives with older or newer motor board firmware than the vendor stack flashes.
        for (chassis, code) in [
            (r#""0.6.31""#, "warn tron.firmware_new"),
            (r#""0.6.35", "0.6.40""#, "stop tron.firmware_old"),
        ] {
            let other = inputs(os_bundle(w.dir.path(), chassis));
            let f = tron
                .check(&mut dev, &b, &other, &mut ctx(&mut w))
                .await
                .unwrap();
            let codes: Vec<String> = f
                .iter()
                .filter_map(|f| match f {
                    Finding::Stop { code, .. } => Some(format!("stop {code}")),
                    Finding::Warn { code, .. } => Some(format!("warn {code}")),
                    Finding::Info { .. } => None,
                })
                .collect();
            assert_eq!(codes, [code], "{chassis}");
            assert_eq!(
                f.contains(&Finding::Info {
                    code: "tron.report".into(),
                    details: "vendor v1.1.1, chassis 0.6.34, cutter 0.6.36".into()
                }),
                code.starts_with("warn"),
                "{f:?}"
            );
        }
        drop(dev);
        // The Tron started its system in between.
        flip_last_rootfs_byte(&path);
        let (mut dev, _) = tron.open(&found, &mut ctx(&mut w)).await.unwrap();
        assert_eq!(
            tron.check(&mut dev, &b, &listed, &mut ctx(&mut w))
                .await
                .unwrap(),
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
        let mut w = world();
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
        let mut w = world();
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
        let w = world();
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
