//! Sector access to the flash of a mower: a module's USB layer implements Disk, FileDisk stands in for it in tests.
//! Writing goes through Bounded, which refuses every byte outside the one window it was made for.

use anyhow::{Result, bail, ensure};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const SECTOR: u64 = 512;

#[allow(async_fn_in_trait)]
pub trait Disk {
    /// Size of the flash in sectors.
    fn sectors(&self) -> u64;
    /// Fills buf, a whole number of sectors, from sector first on.
    async fn read(&mut self, first: u64, buf: &mut [u8]) -> Result<()>;
    /// Writes buf, a whole number of sectors, from sector first on.
    async fn write(&mut self, first: u64, buf: &[u8]) -> Result<()>;
}

/// Sector count of a buffer, an error unless it is whole sectors and not empty.
fn count(buf_len: usize) -> Result<u64> {
    let len = buf_len as u64;
    ensure!(
        len > 0 && len % SECTOR == 0,
        "{len} bytes are no whole number of sectors"
    );
    Ok(len / SECTOR)
}

/// Sectors first..first + sectors, the only ones Bounded writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub first: u64,
    pub sectors: u64,
}

impl Window {
    pub fn holds(&self, first: u64, sectors: u64) -> bool {
        first >= self.first
            && sectors <= self.sectors
            && first - self.first <= self.sectors - sectors
    }
}

/// A disk that writes only inside its window; reading is free.
pub struct Bounded<D> {
    disk: D,
    window: Window,
}

impl<D: Disk> Bounded<D> {
    pub fn new(disk: D, window: Window) -> Result<Self> {
        ensure!(
            window.sectors > 0
                && window
                    .first
                    .checked_add(window.sectors)
                    .is_some_and(|end| end <= disk.sectors()),
            "the window {window:?} is not on the disk"
        );
        Ok(Bounded { disk, window })
    }

    pub fn into_inner(self) -> D {
        self.disk
    }
}

impl<D: Disk> Disk for Bounded<D> {
    fn sectors(&self) -> u64 {
        self.disk.sectors()
    }

    async fn read(&mut self, first: u64, buf: &mut [u8]) -> Result<()> {
        self.disk.read(first, buf).await
    }

    async fn write(&mut self, first: u64, buf: &[u8]) -> Result<()> {
        let sectors = count(buf.len())?;
        if !self.window.holds(first, sectors) {
            bail!(
                "refused: sectors {first}+{sectors} lie outside the window {:?}",
                self.window
            );
        }
        self.disk.write(first, buf).await
    }
}

/// Errors a FileDisk makes on purpose; calls count reads and writes together from 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Call n fails, the calls after it work.
    FailAt(u64),
    /// From call n on every call fails, as with a pulled cable.
    GoneFrom(u64),
    /// Reading this sector returns its first byte flipped; the file stays right.
    Flip(u64),
}

/// A file as the flash of a mower.
pub struct FileDisk {
    file: File,
    sectors: u64,
    faults: Vec<Fault>,
    calls: u64,
    writable: bool,
}

impl FileDisk {
    pub fn open(path: &Path, faults: Vec<Fault>) -> Result<Self> {
        Self::opened(path, faults, true)
    }

    /// For an original image that must never change: the file is opened for reading only and every write fails.
    pub fn open_read_only(path: &Path, faults: Vec<Fault>) -> Result<Self> {
        Self::opened(path, faults, false)
    }

    fn opened(path: &Path, faults: Vec<Fault>, writable: bool) -> Result<Self> {
        let file = File::options().read(true).write(writable).open(path)?;
        let len = file.metadata()?.len();
        ensure!(
            len % SECTOR == 0,
            "{} is no whole number of sectors",
            path.display()
        );
        Ok(FileDisk {
            file,
            sectors: len / SECTOR,
            faults,
            calls: 0,
            writable,
        })
    }

    fn call(&mut self, first: u64, sectors: u64) -> Result<()> {
        let n = self.calls;
        self.calls += 1;
        for fault in &self.faults {
            match fault {
                Fault::FailAt(at) if *at == n => bail!("injected: call {n} failed"),
                Fault::GoneFrom(at) if *at <= n => {
                    bail!("injected: the device is gone since call {at}")
                }
                _ => {}
            }
        }
        ensure!(
            first
                .checked_add(sectors)
                .is_some_and(|end| end <= self.sectors),
            "sectors {first}+{sectors} are past the end ({})",
            self.sectors
        );
        self.file.seek(SeekFrom::Start(first * SECTOR))?;
        Ok(())
    }
}

impl Disk for FileDisk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    async fn read(&mut self, first: u64, buf: &mut [u8]) -> Result<()> {
        let sectors = count(buf.len())?;
        self.call(first, sectors)?;
        self.file.read_exact(buf)?;
        for fault in &self.faults {
            if let Fault::Flip(sector) = *fault {
                if sector >= first && sector < first + sectors {
                    buf[((sector - first) * SECTOR) as usize] ^= 0xff;
                }
            }
        }
        Ok(())
    }

    async fn write(&mut self, first: u64, buf: &[u8]) -> Result<()> {
        ensure!(self.writable, "the disk is open for reading only");
        let sectors = count(buf.len())?;
        self.call(first, sectors)?;
        self.file.write_all(buf)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(sectors: u64, faults: Vec<Fault>) -> (tempfile::NamedTempFile, FileDisk) {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(sectors * SECTOR).unwrap();
        let disk = FileDisk::open(file.path(), faults).unwrap();
        (file, disk)
    }

    fn sectors(n: u64, byte: u8) -> Vec<u8> {
        vec![byte; (n * SECTOR) as usize]
    }

    #[tokio::test]
    async fn writes_only_inside_the_window() {
        let (_file, raw) = disk(100, vec![]);
        let mut d = Bounded::new(
            raw,
            Window {
                first: 10,
                sectors: 20,
            },
        )
        .unwrap();
        d.write(10, &sectors(20, 1)).await.unwrap();
        d.write(29, &sectors(1, 2)).await.unwrap();
        for (first, n) in [(9, 1), (29, 2), (30, 1), (0, 1), (u64::MAX, 1), (10, 21)] {
            assert!(d.write(first, &sectors(n, 3)).await.is_err(), "{first}+{n}");
        }
        let mut back = sectors(100, 0);
        d.read(0, &mut back).await.unwrap();
        let s = SECTOR as usize;
        assert!(back[..10 * s].iter().all(|b| *b == 0));
        assert!(back[10 * s..29 * s].iter().all(|b| *b == 1));
        assert!(back[29 * s..30 * s].iter().all(|b| *b == 2));
        assert!(back[30 * s..].iter().all(|b| *b == 0));
    }

    #[tokio::test]
    async fn a_window_off_the_disk_is_refused() {
        for window in [
            Window {
                first: 90,
                sectors: 11,
            },
            Window {
                first: 0,
                sectors: 0,
            },
            Window {
                first: u64::MAX,
                sectors: 2,
            },
        ] {
            let (_file, raw) = disk(100, vec![]);
            assert!(Bounded::new(raw, window).is_err(), "{window:?}");
        }
    }

    #[tokio::test]
    async fn parts_of_sectors_are_refused() {
        let (_file, mut d) = disk(4, vec![]);
        assert!(d.write(0, &[0u8; 100]).await.is_err());
        assert!(d.read(0, &mut []).await.is_err());
        assert!(d.read(3, &mut sectors(2, 0)).await.is_err());
    }

    #[tokio::test]
    async fn injected_faults() {
        let (_file, mut d) = disk(
            8,
            vec![Fault::FailAt(1), Fault::Flip(5), Fault::GoneFrom(4)],
        );
        d.write(0, &sectors(8, 7)).await.unwrap();
        assert!(d.read(0, &mut sectors(1, 0)).await.is_err());
        let mut buf = sectors(4, 0);
        d.read(4, &mut buf).await.unwrap();
        assert_eq!(buf[0], 7);
        assert_eq!(buf[SECTOR as usize], !7);
        assert_eq!(buf[SECTOR as usize + 1], 7);
        d.read(0, &mut sectors(1, 0)).await.unwrap();
        assert!(d.read(0, &mut sectors(1, 0)).await.is_err());
        assert!(d.write(0, &sectors(1, 0)).await.is_err());
    }

    #[tokio::test]
    async fn a_read_only_disk_never_writes() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(4 * SECTOR).unwrap();
        let mut d = FileDisk::open_read_only(file.path(), vec![]).unwrap();
        assert!(d.write(0, &sectors(1, 9)).await.is_err());
        let mut back = sectors(4, 1);
        d.read(0, &mut back).await.unwrap();
        assert!(back.iter().all(|b| *b == 0));
    }
}
