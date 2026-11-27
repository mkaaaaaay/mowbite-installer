//! How the Tron module reaches the flash: over USB, or a file standing in for it.

use super::usb::{self, TronUsb};
use crate::disk::{Disk, FileDisk};
use crate::mower::{Found, UsbMode};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

const VENDOR: u16 = 0x2207;
/// RK3588 in maskrom and in loader mode.
const PRODUCT: u16 = 0x350b;

#[allow(async_fn_in_trait)]
pub trait Port {
    type Disk: Disk;
    /// A Tron on the port; nothing is sent to it.
    async fn find(&self) -> Result<Option<Found>>;
    /// Makes the flash readable (in maskrom the loader goes into RAM, the flash stays as it is) and opens it.
    /// `loader_sha256` comes from the signed tables.
    async fn open(&self, found: &Found, loader_sha256: &str) -> Result<Self::Disk>;
    /// Restarts into the system on the flash; the disk is used up.
    async fn restart(&self, disk: Self::Disk) -> Result<()>;
}

pub struct UsbPort {
    /// The Rockchip boot file whose loader goes into RAM.
    pub loader: Vec<u8>,
}

fn bus_path(info: &nusb::DeviceInfo) -> String {
    let chain: Vec<String> = info.port_chain().iter().map(u8::to_string).collect();
    format!("{}-{}", info.bus_id(), chain.join("."))
}

const MODE_UNKNOWN: &str = "tron.usb_mode_unknown";
const LOADER_NOT_STARTED: &str = "tron.loader_not_started";
/// How long the Tron may take to come back on USB in its next mode.
const NEXT_MODE: Duration = Duration::from_secs(20);

/// The USB modes of a Tron, told apart by what the system already knows about the device. Nothing is asked of the
/// device for it: the boot ROM takes no loader any more once it got other requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// The boot ROM of the chip; it only takes the loader.
    Maskrom,
    /// The loader this installer puts into RAM; it reads and writes the whole flash.
    Plug,
    /// The loader mode of U-Boot. It answers reads past the first 32 MiB with fill bytes and no error, so the flash
    /// is never read through it.
    UBoot,
    Unknown,
}

/// Values measured on a Tron: maskrom and the loader in RAM differ in the product string only.
fn seen(usb_version: u16, device_version: u16, product: Option<&str>) -> Seen {
    match (
        usb_version,
        device_version,
        product.filter(|p| !p.is_empty()),
    ) {
        (0x0200, 0x0100, None) => Seen::Maskrom,
        (0x0200, 0x0100, Some("USB-MSC")) => Seen::Plug,
        (0x0201, _, Some("USB download gadget")) => Seen::UBoot,
        _ => Seen::Unknown,
    }
}

fn seen_of(info: &nusb::DeviceInfo) -> Seen {
    seen(
        info.usb_version(),
        info.device_version(),
        info.product_string(),
    )
}

fn mode(info: &nusb::DeviceInfo) -> UsbMode {
    match seen_of(info) {
        Seen::Maskrom => UsbMode::Maskrom,
        Seen::Plug | Seen::UBoot => UsbMode::Loader,
        Seen::Unknown => UsbMode::Unknown,
    }
}

/// Windows opens the Tron only through WinUSB.
#[cfg(windows)]
fn needs_driver(info: &nusb::DeviceInfo) -> bool {
    !info
        .driver()
        .is_some_and(|d| d.eq_ignore_ascii_case("winusb"))
}

/// Linux opens it only when this user may use its device file (the udev rule).
#[cfg(target_os = "linux")]
fn needs_driver(info: &nusb::DeviceInfo) -> bool {
    !super::usb_driver::may_use(info.busnum(), info.device_address())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn needs_driver(_info: &nusb::DeviceInfo) -> bool {
    false
}

impl UsbPort {
    async fn info(&self, found: &Found) -> Result<nusb::DeviceInfo> {
        usb::list()
            .await?
            .into_iter()
            .find(|i| {
                i.vendor_id() == VENDOR
                    && i.product_id() == PRODUCT
                    && bus_path(i) == found.bus_path
            })
            .context("the Tron is no longer on USB")
    }

    /// Waits until the Tron is back on its port in mode `want`.
    async fn wait_for(&self, found: &Found, want: Seen) -> Result<nusb::DeviceInfo> {
        let end = tokio::time::Instant::now() + NEXT_MODE;
        loop {
            match self.info(found).await {
                Ok(info) if seen_of(&info) == want => return Ok(info),
                _ if tokio::time::Instant::now() >= end => {
                    bail!(
                        "the Tron did not show up as {want:?} within {} s",
                        NEXT_MODE.as_secs()
                    )
                }
                _ => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
    }
}

impl Port for UsbPort {
    type Disk = TronUsb;

    async fn find(&self) -> Result<Option<Found>> {
        let all: Vec<_> = usb::list()
            .await?
            .into_iter()
            .filter(|i| i.vendor_id() == VENDOR && i.product_id() == PRODUCT)
            .collect();
        match all.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(Found {
                mode: mode(one),
                bus_path: bus_path(one),
                needs_driver: needs_driver(one),
            })),
            _ => bail!("more than one Rockchip device on USB, keep only the Tron connected"),
        }
    }

    async fn open(&self, found: &Found, loader_sha256: &str) -> Result<TronUsb> {
        let mut info = self.info(found).await?;
        let mut now = seen_of(&info);
        if now == Seen::UBoot {
            // The answer to the restart may be lost with the device; the mode it comes back in decides.
            let sent = usb::reboot_maskrom(info).await;
            info = self
                .wait_for(found, Seen::Maskrom)
                .await
                .with_context(|| format!("restart into maskrom: {sent:?}"))?;
            now = Seen::Maskrom;
        }
        if now == Seen::Maskrom {
            ensure!(
                format!("{:x}", Sha256::digest(&self.loader)) == loader_sha256,
                "the loader is not the one the tables name"
            );
            usb::load_loader(info, &self.loader)
                .await
                .context(LOADER_NOT_STARTED)?;
            info = self
                .wait_for(found, Seen::Plug)
                .await
                .context(LOADER_NOT_STARTED)?;
            now = Seen::Plug;
        }
        ensure!(
            now == Seen::Plug,
            "{MODE_UNKNOWN}: USB {:04x}, device {:04x}, product {:?}",
            info.usb_version(),
            info.device_version(),
            info.product_string()
        );
        TronUsb::open(info).await
    }

    async fn restart(&self, disk: TronUsb) -> Result<()> {
        disk.restart().await
    }
}

/// A file as the flash; read_only for originals, which must never change. After restart() it is gone from the
/// port, as a Tron that started its system.
pub struct FilePort {
    pub path: PathBuf,
    pub read_only: bool,
    pub started: std::cell::Cell<bool>,
}

impl Port for FilePort {
    type Disk = FileDisk;

    async fn find(&self) -> Result<Option<Found>> {
        Ok((!self.started.get() && self.path.exists()).then(|| Found {
            mode: UsbMode::Loader,
            bus_path: self.path.display().to_string(),
            needs_driver: false,
        }))
    }

    async fn open(&self, _: &Found, _: &str) -> Result<FileDisk> {
        if self.read_only {
            FileDisk::open_read_only(&self.path, vec![])
        } else {
            FileDisk::open(&self.path, vec![])
        }
    }

    async fn restart(&self, _: FileDisk) -> Result<()> {
        self.started.set(true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_measured_modes_are_told_apart() {
        assert_eq!(seen(0x0200, 0x0100, None), Seen::Maskrom);
        assert_eq!(seen(0x0200, 0x0100, Some("")), Seen::Maskrom);
        assert_eq!(seen(0x0200, 0x0100, Some("USB-MSC")), Seen::Plug);
        assert_eq!(
            seen(0x0201, 0x0223, Some("USB download gadget")),
            Seen::UBoot
        );
    }

    #[test]
    fn anything_else_is_unknown() {
        for (usb, device, product) in [
            (0x0201, 0x0100, None),
            (0x0200, 0x0223, None),
            (0x0200, 0x0100, Some("USB download gadget")),
            (0x0201, 0x0223, Some("USB-MSC")),
            (0x0200, 0x0100, Some("usb-msc")),
            (0x0300, 0x0100, None),
        ] {
            assert_eq!(seen(usb, device, product), Seen::Unknown, "{product:?}");
        }
    }
}
