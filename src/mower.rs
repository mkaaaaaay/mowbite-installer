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
    /// Never more than partition_sectors; a smaller image grows on the first start.
    pub sectors: u64,
    /// From the partition table read from the device.
    pub partition_sectors: u64,
    pub image: PathBuf,
    /// Of the prepared image; the read back is compared with it.
    pub sha256: String,
}

impl WritePlan {
    pub fn fits(&self) -> bool {
        self.sectors > 0 && self.sectors <= self.partition_sectors
    }
}

/// What USB shows after the restart; the frame checks the network itself.
pub enum StartSeen {
    /// The device is back in USB mode: a failed start. The frame offers to write again or to go back.
    UsbAgain(Found),
    NotSeen,
}

/// The signed base system with the place for the per-device pack.
pub struct BaseImage {
    pub path: PathBuf,
    pub sha256: String,
    /// Byte offset and length of the pack file inside the image, from the signed sidecar.
    pub pack_offset: u64,
    pub pack_len: u64,
}

/// What the owner typed and what the frame loaded; the module only puts it into the pack.
pub struct Inputs {
    pub wifi_name: String,
    pub wifi_key: Secret<String>,
    pub device_name: String,
    pub setup_wifi_key: Secret<String>,
    /// Signed bundles (os, app, planner) for the inbox.
    pub bundles: Vec<PathBuf>,
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

    async fn detect(&self) -> Result<Option<Found>>;
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
    fn a_plan_never_reaches_past_its_partition() {
        assert!(plan(100, 100).fits());
        assert!(plan(50, 100).fits());
        assert!(!plan(101, 100).fits());
        assert!(!plan(0, 100).fits());
    }
}
