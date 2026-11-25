//! What a mower family has to provide so the installer can bring MowBite OS onto it and back.
//!
//! The frame runs the steps in order, keeps their results after each step, shows the pages, loads and verifies
//! signed files, keeps the PC awake and does the network part of the first start. A module does everything that
//! depends on the device. It gets no free file or network access: only what Ctx hands over.

use crate::secret::Secret;
use crate::store::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Progress of the running step, shown by the frame.
pub trait Progress {
    fn step(&self, code: &'static str);
    fn bytes(&self, done: u64, total: u64);
}

/// Set by the owner. Before writing a module stops at the next block; from the first written block on the frame
/// only offers "back" or "go on", never a half written system.
#[derive(Default)]
pub struct Cancel(AtomicBool);

impl Cancel {
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The log of a run; it takes text only, so a Secret cannot be passed in. Serial numbers only as hash.
pub trait Log {
    fn info(&self, code: &'static str, details: &str);
}

/// Folder for the work image; it holds keys, the frame deletes it after the read back.
pub struct WorkDir {
    pub path: PathBuf,
}

/// Signed tables from the releases (vendor versions, kernels, partition tables, boards, firmware), verified by the
/// frame; each module reads its own format.
pub struct Tables {
    pub raw: Vec<u8>,
}

pub struct Ctx<'a> {
    pub store: &'a mut Store,
    pub work: &'a WorkDir,
    pub tables: &'a Tables,
    pub progress: &'a dyn Progress,
    pub cancel: &'a Cancel,
    pub log: &'a dyn Log,
    /// This backup folder wrote the system partition of the device: it differs from the backup on purpose.
    pub system_written: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsbMode {
    Maskrom,
    Loader,
    Unknown,
}

/// A device seen on USB; nothing is loaded or written to find it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Found {
    pub mode: UsbMode,
    /// Lets the frame find the same device again after a restart.
    pub bus_path: String,
    /// The system has no driver yet that opens it (Windows without WinUSB): install_usb_driver first.
    #[serde(default)]
    pub needs_driver: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    pub first_sector: u64,
    pub last_sector: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub model: String,
    /// Board revision from the tables, None when unknown.
    pub board: Option<String>,
    /// Shortened sha256 of the serial number; the plain one stays in the backup.
    pub serial_hash: String,
    /// Vendor software version, None when unknown (the owner is warned, the install goes on).
    pub vendor_version: Option<String>,
    pub flash_sectors: u64,
    /// The partition table as read from the device.
    pub table: Vec<Part>,
}

/// What the backup holds; fingerprint lets back() prove the backup belongs to the device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backup {
    pub table: Vec<Part>,
    /// Store file name and sha256 of each saved area.
    pub files: Vec<(String, String)>,
    /// sha256 of areas that never change between installs (on the Tron flash-start, uboot, boot and recovery).
    pub fingerprint: Vec<(String, String)>,
}

/// Stop ends the run before anything is written; Warn needs the owner to agree. code is also the text key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Finding {
    Stop { code: String, details: String },
    Warn { code: String, details: String },
    Info { code: String, details: String },
}

/// What will be written; made only by prepare(), kept by the frame and shown before write().
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WritePlan {
    pub partition: String,
    pub first_sector: u64,
    /// Exactly partition_sectors until a smaller image that grows on the first start is decided (N3).
    pub sectors: u64,
    /// From the partition table read from the device.
    pub partition_sectors: u64,
    pub image: PathBuf,
    /// Of the prepared image; the read back is compared with it.
    pub sha256: String,
}

impl WritePlan {
    pub fn fits(&self) -> bool {
        self.sectors > 0 && self.sectors == self.partition_sectors
    }
}

/// Context of the error of write() or back() when it failed before the first block went to the device: the system
/// on the mower is as it was.
#[derive(Debug)]
pub struct NothingWritten;

impl std::fmt::Display for NothingWritten {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("nothing was written")
    }
}

/// Context of the error of write() or back() when the image was written and read back and only the restart
/// failed: the system is complete, the owner switches the mower off and on instead of writing again.
#[derive(Debug)]
pub struct WrittenNotRestarted;

impl std::fmt::Display for WrittenNotRestarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("written and read back, but the restart failed")
    }
}

/// What USB shows after the restart; the frame checks the network itself.
pub enum StartSeen {
    /// The device is back in USB mode: a failed start. The frame offers to write again or to go back.
    UsbAgain(Found),
    NotSeen,
}

/// The bytes of a base image: a file whose name ends in .zst is unpacked while it is read, any other is read as it
/// is. Size and sha256 of the sidecar are those of these bytes.
pub fn open_base(path: &std::path::Path) -> Result<Box<dyn std::io::Read>> {
    use anyhow::Context;
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(if path.extension().is_some_and(|e| e == "zst") {
        Box::new(zstd::Decoder::new(file)?)
    } else {
        Box::new(file)
    })
}

/// The signed base system with the place for the per-device pack.
pub struct BaseImage {
    pub path: PathBuf,
    pub sha256: String,
    /// Where the pack file lies in the image, from the signed sidecar: (byte offset, length) of each piece in the
    /// order of the file; ext4 may split it around the reserved blocks of a group.
    pub pack: Vec<(u64, u64)>,
}

/// What the owner typed and what the frame loaded; the module only puts it into the pack.
pub struct Inputs {
    pub wifi_name: String,
    pub wifi_key: Secret<String>,
    pub device_name: String,
    pub setup_wifi_key: Secret<String>,
    /// 32 hex digits from the sheet: the mower proves its answers on the network with it.
    pub install_id: Secret<String>,
    /// Signed bundles (os, app, planner) and their .sig files for the inbox.
    pub bundles: Vec<Bundle>,
}

/// A file for the inbox with its sha256 when its signature was checked.
pub struct Bundle {
    pub path: PathBuf,
    pub sha256: String,
}

impl Inputs {
    /// The bundles are still the bytes whose signatures were checked at the start.
    pub fn unchanged(&self) -> Result<()> {
        for b in &self.bundles {
            anyhow::ensure!(
                crate::store::sha256_file(&b.path)? == b.sha256,
                "{} changed since its signature was checked",
                b.path.display()
            );
        }
        Ok(())
    }
}

pub struct Space {
    pub backup_bytes: u64,
    pub work_bytes: u64,
}

/// Photo guide for opening the mower and connecting it: text keys and photo file names.
pub struct Guide {
    pub steps: Vec<(&'static str, &'static str)>,
}

// Modules run inside this program on one thread (Ctx is not Send), so the futures need no Send bound.
#[allow(async_fn_in_trait)]
pub trait Mower {
    /// Handle of an opened device; only it can write, and a restart uses it up.
    type Device;

    fn id(&self) -> &'static str;
    fn guide(&self) -> Guide;
    /// (key, de, en)
    fn texts(&self) -> &'static [(&'static str, &'static str, &'static str)];
    fn space_needed(&self) -> Space;
    /// Name and key of the home Wi-Fi by the rules of this mower, checked while the owner types them.
    fn check_wifi(&self, _name: &str, _key: &str) -> Result<()> {
        Ok(())
    }
    /// Whether the inputs fit the base image (room of the pack), before anything is read from the mower.
    fn check_inputs(&self, _base: &BaseImage, _inputs: &Inputs) -> Result<()> {
        Ok(())
    }

    async fn detect(&self) -> Result<Option<Found>>;
    /// Sets up the driver a Found with needs_driver lacks; the system asks the owner for permission.
    async fn install_usb_driver(&self) -> Result<()> {
        anyhow::bail!("this mower needs no USB driver")
    }
    /// Makes the device readable (the Tron loads its loader into RAM) and reads who it is.
    async fn open(&self, found: &Found, ctx: &mut Ctx<'_>) -> Result<(Self::Device, Identity)>;
    /// Everything once, system and boot areas twice and compared; resumes after an abort, never replaces a backup.
    async fn backup(&self, dev: &mut Self::Device, ctx: &mut Ctx<'_>) -> Result<Backup>;
    async fn check(
        &self,
        dev: &mut Self::Device,
        backup: &Backup,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Finding>>;
    async fn prepare(
        &self,
        backup: &Backup,
        base: &BaseImage,
        inputs: &Inputs,
        ctx: &mut Ctx<'_>,
    ) -> Result<WritePlan>;
    /// Writes plan.image into plan.partition only, reads it back and restarts when the sha256 matches.
    async fn write(&self, dev: Self::Device, plan: &WritePlan, ctx: &mut Ctx<'_>) -> Result<()>;
    async fn start_seen(&self) -> Result<StartSeen>;
    /// Checks that the backup belongs to this device, then writes back its system partition only.
    async fn back(&self, dev: Self::Device, backup: &Backup, ctx: &mut Ctx<'_>) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(sectors: u64, partition_sectors: u64) -> WritePlan {
        WritePlan {
            partition: "rootfs".into(),
            first_sector: 1,
            sectors,
            partition_sectors,
            image: PathBuf::new(),
            sha256: String::new(),
        }
    }

    #[test]
    fn a_plan_fills_its_partition_exactly() {
        assert!(plan(100, 100).fits());
        assert!(!plan(50, 100).fits());
        assert!(!plan(101, 100).fits());
        assert!(!plan(0, 100).fits());
    }
}
