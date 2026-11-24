//! How the Tron module reaches the flash: over USB, or a file standing in for it.

use super::usb::{self, TronUsb};
use crate::disk::{Disk, FileDisk};
use crate::mower::{Found, UsbMode};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

const VENDOR: u16 = 0x2207;
/// RK3588 in maskrom and in loader mode.
const PRODUCT: u16 = 0x350b;

#[allow(async_fn_in_trait)]
pub trait Port {
    type Disk: Disk;
    /// A Tron on the port; nothing is sent to it.
    async fn find(&self) -> Result<Option<Found>>;
    /// Makes the flash readable (in maskrom the loader goes into RAM, the flash stays as it is) and opens it.
    async fn open(&self, found: &Found) -> Result<Self::Disk>;
    /// Restarts into the system on the flash; the disk is used up.
    async fn restart(&self, disk: Self::Disk) -> Result<()>;
}

pub struct UsbPort {
    /// The Rockchip boot file whose loader goes into RAM.
    pub loader: &'static [u8],
}

fn bus_path(info: &nusb::DeviceInfo) -> String {
    let chain: Vec<String> = info.port_chain().iter().map(u8::to_string).collect();
    format!("{}-{}", info.bus_id(), chain.join("."))
}

/// Maskrom and loader look the same until the USB descriptors of both are measured on a Tron.
fn mode(_info: &nusb::DeviceInfo) -> UsbMode {
    UsbMode::Unknown
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
            })),
            _ => bail!("more than one Rockchip device on USB, keep only the Tron connected"),
        }
    }

    async fn open(&self, found: &Found) -> Result<TronUsb> {
        match found.mode {
            UsbMode::Loader => {}
            UsbMode::Maskrom => {
                usb::load_loader(self.info(found).await?, self.loader).await?;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            UsbMode::Unknown => {
                bail!("the USB mode of the Tron is not known, it cannot be opened safely")
            }
        }
        TronUsb::open(self.info(found).await?).await
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
        }))
    }

    async fn open(&self, _: &Found) -> Result<FileDisk> {
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
