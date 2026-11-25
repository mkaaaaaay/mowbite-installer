//! The Tron over USB (Rockchip 2207:*): sectors through rockusb on nusb, the loader into RAM in maskrom mode, and
//! the restart back into maskrom. Every transfer has a time limit; after one ran out the handle refuses all calls,
//! because nusb leaves the endpoint in an unknown state.

use crate::disk::{Disk, SECTOR};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rockfile::boot::{
    RkBootEntry, RkBootEntryBytes, RkBootHeader, RkBootHeaderBytes, RkBootHeaderEntry,
};
use rockusb::protocol::ResetOpcode;
use std::time::Duration;

/// Sectors per transfer; rockusb sends the count as u16.
pub const CHUNK_SECTORS: u64 = 2048;
const LIMIT: Duration = Duration::from_secs(10);

/// (first sector, byte range in the buffer) of each transfer for `len` bytes from `first` on.
pub fn chunks(first: u64, len: usize) -> Result<Vec<(u32, std::ops::Range<usize>)>> {
    ensure!(
        len > 0 && len as u64 % SECTOR == 0,
        "{len} bytes are no whole number of sectors"
    );
    let sectors = len as u64 / SECTOR;
    ensure!(
        first
            .checked_add(sectors)
            .is_some_and(|end| end <= u64::from(u32::MAX)),
        "sector {first} out of reach of the USB protocol"
    );
    let mut out = Vec::new();
    let mut done = 0;
    while done < sectors {
        let n = CHUNK_SECTORS.min(sectors - done);
        let at = (done * SECTOR) as usize;
        out.push(((first + done) as u32, at..at + (n * SECTOR) as usize));
        done += n;
    }
    Ok(out)
}

pub struct TronUsb {
    dev: rockusb::nusb::Device,
    sectors: u64,
    broken: bool,
}

async fn limited<T, E: std::fmt::Debug>(
    broken: &mut bool,
    what: &str,
    f: impl std::future::Future<Output = std::result::Result<T, E>>,
) -> Result<T> {
    ensure!(
        !*broken,
        "the USB connection failed before, plug the cable in again"
    );
    match tokio::time::timeout(LIMIT, f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            *broken = true;
            Err(anyhow!("{what}: {e:?}"))
        }
        Err(_) => {
            *broken = true;
            Err(anyhow!("{what}: no answer within {} s", LIMIT.as_secs()))
        }
    }
}

/// The Rockchip devices on USB; nothing is sent to them.
pub async fn list() -> Result<Vec<nusb::DeviceInfo>> {
    Ok(rockusb::nusb::devices().await?.collect())
}

impl TronUsb {
    /// Opens a device that runs the loader (after load_loader or in loader mode) and reads the flash size.
    pub async fn open(info: nusb::DeviceInfo) -> Result<Self> {
        let mut dev = rockusb::nusb::Device::from_usb_device_info(info)
            .await
            .map_err(|e| anyhow!("open the USB device: {e:?}"))?;
        let mut broken = false;
        let info = limited(&mut broken, "flash info", dev.flash_info()).await?;
        let sectors = u64::from(info.sectors());
        ensure!(sectors > 0, "the device reports no flash");
        Ok(TronUsb {
            dev,
            sectors,
            broken,
        })
    }

    /// Restarts into the system on the flash; the handle is used up.
    pub async fn restart(mut self) -> Result<()> {
        limited(
            &mut self.broken,
            "restart",
            self.dev.reset_device(ResetOpcode::Reset),
        )
        .await
    }
}

/// Restarts a device in the loader mode of U-Boot into maskrom mode. Writes no flash.
pub async fn reboot_maskrom(info: nusb::DeviceInfo) -> Result<()> {
    let mut dev = rockusb::nusb::Device::from_usb_device_info(info)
        .await
        .map_err(|e| anyhow!("open the USB device: {e:?}"))?;
    let mut broken = false;
    limited(
        &mut broken,
        "restart into maskrom",
        dev.reset_device(ResetOpcode::Maskrom),
    )
    .await
}

/// Puts the loader from a Rockchip boot file into RAM of a device in maskrom mode: first the 0x471 blobs (memory
/// setup), then the 0x472 blobs (USB protocol), each followed by the delay the file asks for. Writes no flash.
/// Nothing else is sent before: the boot ROM takes no loader once it got other requests.
pub async fn load_loader(info: nusb::DeviceInfo, boot_file: &[u8]) -> Result<()> {
    let head: &RkBootHeaderBytes = boot_file
        .get(..102)
        .context("loader file too short")?
        .try_into()?;
    let head = RkBootHeader::from_bytes(head).context("not a Rockchip boot file")?;
    let mut dev = rockusb::nusb::Device::from_usb_device_info(info)
        .await
        .map_err(|e| anyhow!("open the USB device: {e:?}"))?;
    let mut broken = false;
    for (area, entry) in [(0x471u16, head.entry_471), (0x472, head.entry_472)] {
        for blob in blobs(boot_file, &entry)? {
            let (data, delay) = blob;
            limited(
                &mut broken,
                "load the loader",
                dev.write_maskrom_area(area, data),
            )
            .await?;
            tokio::time::sleep(Duration::from_millis(u64::from(delay))).await;
        }
    }
    Ok(())
}

/// The blobs of one area with their delay in ms, every range checked against the file.
fn blobs<'a>(file: &'a [u8], entry: &RkBootHeaderEntry) -> Result<Vec<(&'a [u8], u32)>> {
    ensure!(entry.count > 0, "loader file without blobs for an area");
    let mut out = Vec::new();
    for i in 0..usize::from(entry.count) {
        let at = entry.offset as usize + usize::from(entry.size) * i;
        let raw: &RkBootEntryBytes = file
            .get(at..at + 57)
            .context("loader entry outside the file")?
            .try_into()?;
        let e = RkBootEntry::from_bytes(raw);
        let (start, len) = (e.data_offset as usize, e.data_size as usize);
        let data = file
            .get(start..start + len)
            .context("loader blob outside the file")?;
        out.push((data, e.data_delay));
    }
    Ok(out)
}

impl Disk for TronUsb {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    async fn read(&mut self, first: u64, buf: &mut [u8]) -> Result<()> {
        for (at, range) in chunks(first, buf.len())? {
            limited(
                &mut self.broken,
                "read",
                self.dev.read_lba(at, &mut buf[range]),
            )
            .await?;
        }
        Ok(())
    }

    async fn write(&mut self, first: u64, buf: &[u8]) -> Result<()> {
        if first
            .checked_add(buf.len() as u64 / SECTOR)
            .is_none_or(|end| end > self.sectors)
        {
            bail!("write past the end of the flash");
        }
        for (at, range) in chunks(first, buf.len())? {
            limited(
                &mut self.broken,
                "write",
                self.dev.write_lba(at, &buf[range]),
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfers_cover_the_buffer_in_order_and_never_exceed_the_chunk() {
        let len = (CHUNK_SECTORS * 2 + 3) as usize * 512;
        let c = chunks(100, len).unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], (100, 0..CHUNK_SECTORS as usize * 512));
        assert_eq!(c[1].0, 100 + CHUNK_SECTORS as u32);
        assert_eq!(c[2], (100 + 2 * CHUNK_SECTORS as u32, c[1].1.end..len));
        assert!(CHUNK_SECTORS <= u64::from(u16::MAX));
    }

    #[test]
    fn odd_sizes_and_far_sectors_are_refused() {
        assert!(chunks(0, 0).is_err());
        assert!(chunks(0, 513).is_err());
        assert!(chunks(u64::from(u32::MAX), 512).is_err());
        assert!(chunks(u64::from(u32::MAX) - 1, 512).is_ok());
    }

    fn boot_file() -> Vec<u8> {
        let mut f = vec![0u8; 400];
        f[..4].copy_from_slice(b"BOOT");
        // entry_471 at byte 25 and entry_472 at byte 31 of the header: count, offset, size.
        f[25] = 1;
        f[26..30].copy_from_slice(&102u32.to_le_bytes());
        f[30] = 57;
        f[31] = 1;
        f[32..36].copy_from_slice(&159u32.to_le_bytes());
        f[36] = 57;
        for (at, data_at, delay) in [(102usize, 300u32, 1u32), (159, 350, 2)] {
            f[at + 45..at + 49].copy_from_slice(&data_at.to_le_bytes());
            f[at + 49..at + 53].copy_from_slice(&10u32.to_le_bytes());
            f[at + 53..at + 57].copy_from_slice(&delay.to_le_bytes());
        }
        f
    }

    #[test]
    fn loader_blobs_are_found_and_checked_against_the_file() {
        let f = boot_file();
        let head = RkBootHeader::from_bytes(f[..102].try_into().unwrap()).unwrap();
        let b = blobs(&f, &head.entry_471).unwrap();
        assert_eq!(b, [(&f[300..310], 1)]);
        assert_eq!(blobs(&f, &head.entry_472).unwrap()[0].1, 2);
        assert!(blobs(&f[..320], &head.entry_472).is_err());
    }
}
