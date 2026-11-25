//! From the signed base image to the rootfs partition: copy the base into the work folder, put the pack into the
//! file it reserves, write the result into the rootfs window only and read it back.

use super::pack;
use crate::disk::{Bounded, Disk, SECTOR, Window};
use crate::mower::{BaseImage, Cancel, NothingWritten, Progress, WritePlan};
use crate::store::sha256_file;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const COPY: usize = 8 << 20;
/// Zero runs of this size in a work image are left as holes instead of written.
const SPARSE_BLOCK: usize = 64 << 10;
static ZEROS: [u8; SPARSE_BLOCK] = [0; SPARSE_BLOCK];

/// Copies `src` into the new, empty file `dst` and leaves zero blocks as holes where the file system supports it;
/// `seen` gets every byte in order. Returns the length.
fn copy_sparse(src: &mut dyn Read, dst: &mut File, mut seen: impl FnMut(&[u8])) -> Result<u64> {
    mark_sparse(dst);
    let mut buf = vec![0u8; COPY];
    let mut len = 0u64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        seen(&buf[..n]);
        for block in buf[..n].chunks(SPARSE_BLOCK) {
            if block == &ZEROS[..block.len()] {
                dst.seek(SeekFrom::Current(block.len() as i64))?;
            } else {
                dst.write_all(block)?;
            }
        }
        len += n as u64;
    }
    dst.set_len(len)?;
    Ok(len)
}

/// NTFS keeps holes only in files marked sparse; where marking fails the skipped blocks are filled with zeros.
#[cfg(windows)]
fn mark_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    let mut returned = 0u32;
    // SAFETY: a valid open handle, no buffers, synchronous call.
    unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn mark_sparse(_: &File) {}

/// Bytes the base image reserves for the pack, over all its pieces.
pub fn room(base: &BaseImage) -> u64 {
    base.pack.iter().map(|&(_, len)| len).sum()
}

/// Copies the base image to `out` while checking its sha256, then writes the pack over the reserved file, piece by
/// piece in file order; the first piece must still start with the empty mark. Returns the sha256 of the result.
pub fn fill_base(base: &BaseImage, filled_pack: &[u8], out: &Path) -> Result<String> {
    ensure!(
        filled_pack.len() as u64 == room(base),
        "pack is {} bytes, the base reserves {}",
        filled_pack.len(),
        room(base)
    );
    let mut src = crate::mower::open_base(&base.path)?;
    // It holds the Wi-Fi keys: readable by the owner only.
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut dst = options
        .open(out)
        .with_context(|| format!("create {}", out.display()))?;
    let mut hash = Sha256::new();
    let len = copy_sparse(&mut src, &mut dst, |b| hash.update(b))?;
    ensure!(
        format!("{:x}", hash.finalize()) == base.sha256,
        "the base image is not the signed one"
    );
    ensure!(
        len % SECTOR == 0,
        "the base image is no whole number of sectors"
    );
    ensure!(
        base.pack
            .iter()
            .all(|&(at, n)| at.checked_add(n).is_some_and(|end| end <= len)),
        "a pack piece lies outside the base image"
    );
    let &(first_at, _) = base
        .pack
        .first()
        .context("the base image reserves no pack")?;
    let mut first = vec![0u8; pack::BLOCK];
    let mut probe = File::open(out)?;
    probe.seek(SeekFrom::Start(first_at))?;
    probe.read_exact(&mut first)?;
    ensure!(
        pack::is_empty_mark(&first),
        "the pack place of the base image does not hold the empty mark"
    );
    let mut rest = filled_pack;
    for &(at, n) in &base.pack {
        let (piece, tail) = rest.split_at(n as usize);
        dst.seek(SeekFrom::Start(at))?;
        dst.write_all(piece)?;
        rest = tail;
    }
    dst.sync_all()?;
    drop(dst);
    sha256_file(out)
}

/// Writes plan.sectors sectors from `src` into the rootfs window and reads them back; Ok only when both match
/// plan.sha256. Nothing outside first_sector..first_sector + partition_sectors is written. The caller checks the
/// source against plan.sha256 before (the prepared image, or the unpacked backup).
pub async fn write_and_verify<D: Disk>(
    disk: D,
    src: &mut dyn Read,
    plan: &WritePlan,
    cancel: &Cancel,
    progress: &dyn Progress,
) -> Result<D> {
    ensure!(
        plan.fits(),
        anyhow::anyhow!("the image does not fit the partition").context(NothingWritten)
    );
    let window = Window {
        first: plan.first_sector,
        sectors: plan.partition_sectors,
    };
    let mut disk = Bounded::new(disk, window).context(NothingWritten)?;
    let total = plan.sectors * SECTOR * 2;
    let mut buf = vec![0u8; COPY];
    let mut at = 0u64;
    let mut written = Sha256::new();
    progress.step("tron.write");
    while at < plan.sectors {
        // Cancel counts only before the first block; after it the frame offers back or go on.
        if at == 0 && cancel.is_set() {
            return Err(anyhow::anyhow!("cancelled").context(NothingWritten));
        }
        let n = (COPY as u64 / SECTOR).min(plan.sectors - at);
        let piece = &mut buf[..(n * SECTOR) as usize];
        if let Err(e) = src.read_exact(piece) {
            let e = anyhow::Error::from(e);
            return Err(if at == 0 {
                e.context(NothingWritten)
            } else {
                e
            });
        }
        written.update(&*piece);
        disk.write(plan.first_sector + at, piece).await?;
        at += n;
        progress.bytes(at * SECTOR, total);
    }
    ensure!(
        src.read(&mut buf[..1])? == 0,
        "the source is longer than the plan"
    );
    ensure!(
        format!("{:x}", written.finalize()) == plan.sha256,
        "the source changed since it was checked"
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
            pack: vec![(8192, 2048), (20480, 2048)],
        }
    }

    /// Hands out the data in uneven reads, so zero runs do not line up with the copy blocks.
    struct Uneven<'a>(&'a [u8]);
    impl Read for Uneven<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.0.len()).min(300_007);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn a_sparse_copy_keeps_every_byte_and_skips_zero_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = vec![0u8; 24 << 20];
        data[..100_000].fill(7);
        data[12 << 20] = 1;
        data[(12 << 20) + SPARSE_BLOCK + 3] = 2;
        let path = dir.path().join("out.img");
        let mut out = File::create_new(&path).unwrap();
        let mut seen = Sha256::new();
        let len = copy_sparse(&mut Uneven(&data), &mut out, |b| seen.update(b)).unwrap();
        drop(out);
        assert_eq!(len, data.len() as u64);
        assert_eq!(seen.finalize(), Sha256::digest(&data));
        assert_eq!(std::fs::read(&path).unwrap(), data);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let stored = std::fs::metadata(&path).unwrap().blocks() * 512;
            assert!(stored < 2 << 20, "{stored} bytes stored");
        }
    }

    #[test]
    fn a_packed_base_gives_the_same_image_as_the_raw_one() {
        let dir = tempfile::tempdir().unwrap();
        let raw = base(dir.path(), pack::EMPTY);
        let packed = BaseImage {
            path: dir.path().join("base.img.zst"),
            sha256: raw.sha256.clone(),
            pack: raw.pack.clone(),
        };
        zstd::stream::copy_encode(
            File::open(&raw.path).unwrap(),
            File::create(&packed.path).unwrap(),
            3,
        )
        .unwrap();
        let filled = vec![0xA5u8; 4096];
        let (a, b) = (dir.path().join("a.img"), dir.path().join("b.img"));
        assert_eq!(
            fill_base(&raw, &filled, &a).unwrap(),
            fill_base(&packed, &filled, &b).unwrap()
        );
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        // A packed file that unpacks to something else is refused like a wrong raw one.
        let whole = std::fs::read(&packed.path).unwrap();
        std::fs::write(
            &packed.path,
            zstd::encode_all(&[0u8; 65536][..], 3).unwrap(),
        )
        .unwrap();
        assert!(fill_base(&packed, &filled, &dir.path().join("c.img")).is_err());
        // A packed file cut short ends in an error while it is read, never in a shorter image.
        for cut in [whole.len() - 1, whole.len() / 2, 5] {
            std::fs::write(&packed.path, &whole[..cut]).unwrap();
            let mut got = Vec::new();
            let read = crate::mower::open_base(&packed.path)
                .and_then(|mut r| Ok(r.read_to_end(&mut got)?));
            assert!(read.is_err(), "cut at {cut}: {} bytes read", got.len());
            let out = dir.path().join(format!("cut-{cut}.img"));
            assert!(fill_base(&packed, &filled, &out).is_err());
        }
    }

    #[test]
    fn the_pack_lands_in_its_place_and_nowhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let b = base(dir.path(), pack::EMPTY);
        let out = dir.path().join("rootfs.img");
        let mut filled = vec![0xabu8; 2048];
        filled.extend_from_slice(&[0xcd; 2048]);
        let sum = fill_base(&b, &filled, &out).unwrap();
        let img = std::fs::read(&out).unwrap();
        assert_eq!(sum, format!("{:x}", Sha256::digest(&img)));
        assert!(img[..8192].iter().chain(&img[22528..]).all(|&x| x == 0x55));
        assert!(img[8192..10240].iter().all(|&x| x == 0xab));
        assert!(img[10240..12288].iter().all(|&x| x == 0));
        assert!(img[12288..20480].iter().all(|&x| x == 0x55));
        assert!(img[20480..22528].iter().all(|&x| x == 0xcd));
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
            partition_sectors: 30,
            image,
            sha256: format!("{:x}", Sha256::digest(&data)),
        };
        let disk = FileDisk::open(&flash, faults).unwrap();
        (dir, disk, plan, flash)
    }

    #[tokio::test]
    async fn writes_inside_the_partition_and_reads_it_back() {
        let (_dir, disk, plan, flash) = setup(vec![]);
        write_and_verify(
            disk,
            &mut File::open(&plan.image).unwrap(),
            &plan,
            &Cancel::default(),
            &Quiet,
        )
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
        let Err(err) = write_and_verify(
            disk,
            &mut File::open(&plan.image).unwrap(),
            &plan,
            &Cancel::default(),
            &Quiet,
        )
        .await
        else {
            panic!("a flipped byte passed the read back");
        };
        assert!(err.to_string().contains("read back differs"), "{err}");
        let (_dir, disk, mut plan, _) = setup(vec![]);
        plan.sectors = 51;
        assert!(
            write_and_verify(
                disk,
                &mut File::open(&plan.image).unwrap(),
                &plan,
                &Cancel::default(),
                &Quiet
            )
            .await
            .is_err()
        );
        let (_dir, disk, mut plan, _) = setup(vec![]);
        plan.first_sector = 80;
        assert!(
            write_and_verify(
                disk,
                &mut File::open(&plan.image).unwrap(),
                &plan,
                &Cancel::default(),
                &Quiet
            )
            .await
            .is_err()
        );
    }
}
