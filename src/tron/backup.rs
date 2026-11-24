//! The backup of the whole flash, area by area: everything once into the store (zstd, the sha256 is over the raw
//! bytes), the start areas and the rootfs a second time and compared, so a reading fault cannot slip in unnoticed.

use crate::disk::{Disk, SECTOR};
use crate::mower::{Cancel, Part, Progress};
use crate::store::{NewFile, Store};
use anyhow::{Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};

/// Sectors per read call (8 MiB).
pub const READ_SECTORS: u64 = 16384;
/// Areas read twice: everything the loader and kernel start from, and the partition that gets written.
const TWICE: [&str; 6] = ["flash-start", "uboot", "misc", "boot", "recovery", "rootfs"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Area {
    pub name: String,
    pub first: u64,
    pub sectors: u64,
    pub twice: bool,
}

impl Area {
    pub fn file(&self) -> String {
        file_name(&self.name)
    }
}

/// The store file of an area.
pub fn file_name(area: &str) -> String {
    format!("{area}.img.zst")
}

/// The whole flash as areas in order: before the first partition, the partitions, gaps between them, after the last.
pub fn areas(table: &[Part], disk_sectors: u64) -> Result<Vec<Area>> {
    let mut parts = table.to_vec();
    parts.sort_by_key(|p| p.first_sector);
    let mut out = Vec::new();
    let mut at = 0;
    let mut push = |name: String, first: u64, end: u64| {
        if end > first {
            let twice = TWICE.contains(&name.as_str());
            out.push(Area {
                name,
                first,
                sectors: end - first,
                twice,
            });
        }
    };
    for (i, p) in parts.iter().enumerate() {
        ensure!(
            p.first_sector >= at,
            "partition {} overlaps the one before",
            p.name
        );
        ensure!(
            p.last_sector < disk_sectors,
            "partition {} reaches past the flash",
            p.name
        );
        ensure!(
            p.name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !p.name.is_empty(),
            "partition name {:?} cannot name a backup file",
            p.name
        );
        let gap = if i == 0 {
            "flash-start".to_string()
        } else {
            format!("gap-{at}")
        };
        push(gap, at, p.first_sector);
        push(p.name.clone(), p.first_sector, p.last_sector + 1);
        at = p.last_sector + 1;
    }
    push("flash-end".to_string(), at, disk_sectors);
    Ok(out)
}

/// Reads one area front to back, hands every piece to `sink` and returns the sha256 of all of it.
pub async fn read_area<D: Disk>(
    disk: &mut D,
    area: &Area,
    cancel: &Cancel,
    done: &mut dyn FnMut(u64),
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; (READ_SECTORS * SECTOR) as usize];
    let mut at = 0;
    while at < area.sectors {
        if cancel.is_set() {
            bail!("cancelled");
        }
        let n = READ_SECTORS.min(area.sectors - at);
        let piece = &mut buf[..(n * SECTOR) as usize];
        disk.read(area.first + at, piece).await?;
        hash.update(&*piece);
        sink(piece)?;
        at += n;
        done(n * SECTOR);
    }
    Ok(format!("{:x}", hash.finalize()))
}

struct ToFile<'a>(&'a mut NewFile);

impl Write for ToFile<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf).map_err(std::io::Error::other)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The raw bytes of a saved area, after its file passed the check against SHA256SUMS.
pub fn open_raw(store: &Store, name: &str) -> Result<impl Read + use<>> {
    store.verify(name)?;
    Ok(zstd::Decoder::new(File::open(store.path(name)?)?)?)
}

pub fn raw_sha256(store: &Store, name: &str) -> Result<String> {
    let mut hash = Sha256::new();
    std::io::copy(&mut open_raw(store, name)?, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

/// Saves every area that is not in the store yet and returns (file, sha256) of all of them. Finished files are kept
/// as they are, so a run can go on after an abort; a second read that differs ends the backup with an error.
pub async fn backup<D: Disk>(
    disk: &mut D,
    table: &[Part],
    store: &mut Store,
    cancel: &Cancel,
    progress: &dyn Progress,
) -> Result<Vec<(String, String)>> {
    let areas = areas(table, disk.sectors())?;
    let total: u64 = areas
        .iter()
        .map(|a| a.sectors * SECTOR * if a.twice { 2 } else { 1 })
        .sum();
    let mut done_bytes = 0u64;
    let mut out = Vec::new();
    for area in &areas {
        let name = area.file();
        let mut done = |n: u64| {
            done_bytes += n;
            progress.bytes(done_bytes, total);
        };
        let sum = if store.recorded(&name)?.is_some() {
            done(area.sectors * SECTOR);
            raw_sha256(store, &name)?
        } else {
            store.discard_unfinished(&name)?;
            let mut file = store.create_new(&name)?;
            let mut packed = zstd::Encoder::new(ToFile(&mut file), 3)?;
            let sum = read_area(disk, area, cancel, &mut done, &mut |b| {
                Ok(packed.write_all(b)?)
            })
            .await?;
            packed.finish()?;
            file.finish()?;
            ensure!(
                raw_sha256(store, &name)? == sum,
                "{name}: the saved file does not unpack to what was read"
            );
            sum
        };
        if area.twice {
            let again = read_area(disk, area, cancel, &mut done, &mut |_| Ok(())).await?;
            ensure!(
                again == sum,
                "{}: the second read differs from the first, the USB connection is not reliable",
                area.name
            );
        }
        out.push((name, sum));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::{Fault, FileDisk};
    use std::cell::Cell;

    fn part(name: &str, first: u64, last: u64) -> Part {
        Part {
            name: name.into(),
            first_sector: first,
            last_sector: last,
        }
    }

    fn table() -> Vec<Part> {
        vec![
            part("boot", 64, 127),
            part("uboot", 16, 31),
            part("rootfs", 128, 299),
            part("oem", 310, 399),
        ]
    }

    #[test]
    fn areas_cover_the_flash_without_overlap() {
        let a = areas(&table(), 512).unwrap();
        let names: Vec<&str> = a.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "flash-start",
                "uboot",
                "gap-32",
                "boot",
                "rootfs",
                "gap-300",
                "oem",
                "flash-end"
            ]
        );
        let mut at = 0;
        for x in &a {
            assert_eq!(x.first, at);
            at += x.sectors;
        }
        assert_eq!(at, 512);
        let twice: Vec<&str> = a
            .iter()
            .filter(|a| a.twice)
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(twice, ["flash-start", "uboot", "boot", "rootfs"]);
        assert!(areas(&[part("a", 10, 20), part("b", 20, 30)], 512).is_err());
        assert!(areas(&[part("a", 10, 600)], 512).is_err());
        assert!(areas(&[part("../x", 10, 20)], 512).is_err());
    }

    struct Bytes(Cell<(u64, u64)>);
    impl Progress for Bytes {
        fn step(&self, _: &'static str) {}
        fn bytes(&self, done: u64, total: u64) {
            self.0.set((done, total));
        }
    }

    fn flash(sectors: u64) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..sectors * SECTOR).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(f.path(), data).unwrap();
        f
    }

    #[tokio::test]
    async fn saves_each_area_with_its_hash_and_counts_the_second_reads() {
        let f = flash(512);
        let mut disk = FileDisk::open(f.path(), vec![]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let p = Bytes(Cell::new((0, 0)));
        let files = backup(&mut disk, &table(), &mut store, &Cancel::default(), &p)
            .await
            .unwrap();
        assert_eq!(files.len(), 8);
        let raw = std::fs::read(f.path()).unwrap();
        let mut rootfs = Vec::new();
        open_raw(&store, "rootfs.img.zst")
            .unwrap()
            .read_to_end(&mut rootfs)
            .unwrap();
        assert_eq!(rootfs, &raw[128 * 512..300 * 512]);
        let (_, sum) = files.iter().find(|(f, _)| f == "rootfs.img.zst").unwrap();
        assert_eq!(*sum, format!("{:x}", Sha256::digest(&rootfs)));
        for (name, _) in &files {
            store.verify(name).unwrap();
        }
        assert_eq!(
            p.0.get(),
            (
                512 * 512 + (16 + 16 + 64 + 172) * 512,
                512 * 512 + (16 + 16 + 64 + 172) * 512
            )
        );
    }

    #[tokio::test]
    async fn goes_on_after_an_abort_and_keeps_finished_files() {
        let f = flash(512);
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let p = Bytes(Cell::new((0, 0)));
        // Reads: flash-start, again, uboot, again, then the gap fails.
        let mut disk = FileDisk::open(f.path(), vec![Fault::FailAt(4)]).unwrap();
        assert!(
            backup(&mut disk, &table(), &mut store, &Cancel::default(), &p)
                .await
                .is_err()
        );
        let before = std::fs::metadata(store.path("uboot.img.zst").unwrap())
            .unwrap()
            .modified()
            .unwrap();
        assert!(store.path("gap-32.img.zst").unwrap().exists());
        let mut disk = FileDisk::open(f.path(), vec![]).unwrap();
        backup(&mut disk, &table(), &mut store, &Cancel::default(), &p)
            .await
            .unwrap();
        let after = std::fs::metadata(store.path("uboot.img.zst").unwrap())
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
    }

    /// Flips a byte of one sector on its second read only.
    struct FlipOnce {
        disk: FileDisk,
        sector: u64,
        seen: u32,
    }
    impl Disk for FlipOnce {
        fn sectors(&self) -> u64 {
            self.disk.sectors()
        }
        async fn read(&mut self, first: u64, buf: &mut [u8]) -> Result<()> {
            self.disk.read(first, buf).await?;
            let n = buf.len() as u64 / SECTOR;
            if (first..first + n).contains(&self.sector) {
                self.seen += 1;
                if self.seen == 2 {
                    buf[((self.sector - first) * SECTOR) as usize] ^= 1;
                }
            }
            Ok(())
        }
        async fn write(&mut self, first: u64, buf: &[u8]) -> Result<()> {
            self.disk.write(first, buf).await
        }
    }

    #[tokio::test]
    async fn a_second_read_that_differs_stops_the_backup() {
        let f = flash(512);
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let p = Bytes(Cell::new((0, 0)));
        let mut disk = FlipOnce {
            disk: FileDisk::open(f.path(), vec![]).unwrap(),
            sector: 200,
            seen: 0,
        };
        let err = backup(&mut disk, &table(), &mut store, &Cancel::default(), &p)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("rootfs: the second read differs"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn cancel_stops_before_the_next_read() {
        let f = flash(512);
        let mut disk = FileDisk::open(f.path(), vec![]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let cancel = Cancel::default();
        cancel.set();
        let p = Bytes(Cell::new((0, 0)));
        let err = backup(&mut disk, &table(), &mut store, &cancel, &p)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "cancelled");
    }
}
