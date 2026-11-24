//! From the signed base image to the rootfs partition: copy the base into the work folder, put the pack into the
//! file it reserves, write the result into the rootfs window only and read it back.

use super::pack;
use crate::disk::{Bounded, Disk, SECTOR, Window};
use crate::mower::{BaseImage, Cancel, Progress, WritePlan};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const COPY: usize = 8 << 20;

/// Copies the base image to `out` while checking its sha256, then writes the pack over the reserved file, which must
/// still hold the empty mark. Returns the sha256 of the result.
pub fn fill_base(base: &BaseImage, filled_pack: &[u8], out: &Path) -> Result<String> {
    ensure!(
        filled_pack.len() as u64 == base.pack_len,
        "pack is {} bytes, the base reserves {}",
        filled_pack.len(),
        base.pack_len
    );
    let mut src =
        File::open(&base.path).with_context(|| format!("open {}", base.path.display()))?;
    let mut dst = File::options()
        .write(true)
        .create_new(true)
        .open(out)
        .with_context(|| format!("create {}", out.display()))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; COPY];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
    }
    ensure!(
        format!("{:x}", hash.finalize()) == base.sha256,
        "the base image is not the signed one"
    );
    let len = dst.metadata()?.len();
    ensure!(
        len % SECTOR == 0,
        "the base image is no whole number of sectors"
    );
    ensure!(
        base.pack_offset
            .checked_add(base.pack_len)
            .is_some_and(|end| end <= len),
        "the pack place lies outside the base image"
    );

    let mut first = vec![0u8; pack::BLOCK];
    let mut probe = File::open(out)?;
    probe.seek(SeekFrom::Start(base.pack_offset))?;
    probe.read_exact(&mut first)?;
    ensure!(
        pack::is_empty_mark(&first),
        "the pack place of the base image does not hold the empty mark"
    );
    dst.seek(SeekFrom::Start(base.pack_offset))?;
    dst.write_all(filled_pack)?;
    dst.sync_all()?;
    drop(dst);
    sha256_file(out)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; COPY];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Writes plan.image into the rootfs window and reads the same sectors back; Ok only when the sha256 matches the
/// plan. Nothing outside first_sector..first_sector + partition_sectors can be written.
pub async fn write_and_verify<D: Disk>(
    disk: D,
    plan: &WritePlan,
    cancel: &Cancel,
    progress: &dyn Progress,
) -> Result<D> {
    ensure!(plan.fits(), "the image does not fit the partition");
    let window = Window {
        first: plan.first_sector,
        sectors: plan.partition_sectors,
    };
    let mut disk = Bounded::new(disk, window)?;
    let mut src = File::open(&plan.image)?;
    ensure!(
        src.metadata()?.len() == plan.sectors * SECTOR,
        "the image changed size since prepare"
    );
    let total = plan.sectors * SECTOR * 2;
    let mut buf = vec![0u8; COPY];
    let mut at = 0u64;
    let mut written = Sha256::new();
    progress.step("tron.write");
    while at < plan.sectors {
        // Cancel counts only before the first block; after it the frame offers back or go on.
        if at == 0 && cancel.is_set() {
            bail!("cancelled");
        }
        let n = (COPY as u64 / SECTOR).min(plan.sectors - at);
        let piece = &mut buf[..(n * SECTOR) as usize];
        src.read_exact(piece)?;
        written.update(&*piece);
        disk.write(plan.first_sector + at, piece).await?;
        at += n;
        progress.bytes(at * SECTOR, total);
    }
    ensure!(
        format!("{:x}", written.finalize()) == plan.sha256,
        "the image changed since prepare"
    );

    progress.step("tron.read_back");
    let mut back = Sha256::new();
    at = 0;
    while at < plan.sectors {
        let n = (COPY as u64 / SECTOR).min(plan.sectors - at);
        let piece = &mut buf[..(n * SECTOR) as usize];
        disk.read(plan.first_sector + at, piece).await?;
        back.update(&*piece);
        at += n;
        progress.bytes((plan.sectors + at) * SECTOR, total);
    }
    ensure!(
        format!("{:x}", back.finalize()) == plan.sha256,
        "the read back differs from the image"
    );
    Ok(disk.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::{Fault, FileDisk};
    use std::path::PathBuf;

    struct Quiet;
    impl Progress for Quiet {
        fn step(&self, _: &'static str) {}
        fn bytes(&self, _: u64, _: u64) {}
    }

    fn base(dir: &Path, mark: &[u8]) -> BaseImage {
        let mut img = vec![0x55u8; 64 * 1024];
        img[8192..8192 + mark.len()].copy_from_slice(mark);
        img[8192 + mark.len()..8192 + 4096].fill(0);
        let path = dir.join("base.img");
        std::fs::write(&path, &img).unwrap();
        BaseImage {
            sha256: format!("{:x}", Sha256::digest(&img)),
            path,
            pack_offset: 8192,
            pack_len: 4096,
        }
    }

    #[test]
    fn the_pack_lands_in_its_place_and_nowhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let b = base(dir.path(), pack::EMPTY);
        let out = dir.path().join("rootfs.img");
        let filled = vec![0xabu8; 4096];
        let sum = fill_base(&b, &filled, &out).unwrap();
        let img = std::fs::read(&out).unwrap();
        assert_eq!(sum, format!("{:x}", Sha256::digest(&img)));
        assert!(img[..8192].iter().chain(&img[12288..]).all(|&x| x == 0x55));
        assert!(img[8192..12288].iter().all(|&x| x == 0xab));
    }

    #[test]
    fn a_wrong_base_or_a_used_place_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = base(dir.path(), pack::EMPTY);
        b.sha256 = "0".repeat(64);
        assert!(
            fill_base(&b, &[0; 4096], &dir.path().join("a.img"))
                .unwrap_err()
                .to_string()
                .contains("not the signed")
        );
        let b = base(dir.path(), b"MOWBITE-PACK 1\n");
        assert!(
            fill_base(&b, &[0; 4096], &dir.path().join("b.img"))
                .unwrap_err()
                .to_string()
                .contains("empty mark")
        );
        let b = base(dir.path(), pack::EMPTY);
        assert!(fill_base(&b, &[0; 100], &dir.path().join("c.img")).is_err());
    }

    fn setup(faults: Vec<Fault>) -> (tempfile::TempDir, FileDisk, WritePlan, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let flash = dir.path().join("flash.img");
        std::fs::write(&flash, vec![0x11u8; 100 * 512]).unwrap();
        let image = dir.path().join("rootfs.img");
        let data: Vec<u8> = (0..30 * 512).map(|i| (i % 253) as u8).collect();
        std::fs::write(&image, &data).unwrap();
        let plan = WritePlan {
            partition: "rootfs".into(),
            first_sector: 40,
            sectors: 30,
            partition_sectors: 50,
            image,
            sha256: format!("{:x}", Sha256::digest(&data)),
        };
        let disk = FileDisk::open(&flash, faults).unwrap();
        (dir, disk, plan, flash)
    }

    #[tokio::test]
    async fn writes_inside_the_partition_and_reads_it_back() {
        let (_dir, disk, plan, flash) = setup(vec![]);
        write_and_verify(disk, &plan, &Cancel::default(), &Quiet)
            .await
            .unwrap();
        let after = std::fs::read(flash).unwrap();
        assert!(
            after[..40 * 512]
                .iter()
                .chain(&after[70 * 512..])
                .all(|&x| x == 0x11)
        );
        assert_eq!(
            &after[40 * 512..70 * 512],
            &std::fs::read(&plan.image).unwrap()[..]
        );
    }

    #[tokio::test]
    async fn a_bad_read_back_or_a_plan_past_the_partition_fails() {
        let (_dir, disk, plan, _) = setup(vec![Fault::Flip(45)]);
        let Err(err) = write_and_verify(disk, &plan, &Cancel::default(), &Quiet).await else {
            panic!("a flipped byte passed the read back");
        };
        assert!(err.to_string().contains("read back differs"), "{err}");
        let (_dir, disk, mut plan, _) = setup(vec![]);
        plan.sectors = 51;
        assert!(
            write_and_verify(disk, &plan, &Cancel::default(), &Quiet)
                .await
                .is_err()
        );
        let (_dir, disk, mut plan, _) = setup(vec![]);
        plan.first_sector = 80;
        assert!(
            write_and_verify(disk, &plan, &Cancel::default(), &Quiet)
                .await
                .is_err()
        );
    }
}
