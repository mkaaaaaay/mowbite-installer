//! A mower and the things around a run for the tests of the frame.

use crate::mower::*;
use crate::run::{Driver, Next};
use crate::secret::Secret;
use crate::store::Store;
use anyhow::{Result, bail};
use std::cell::RefCell;
use std::path::PathBuf;

pub struct Quiet;
impl Progress for Quiet {
    fn step(&self, _: &'static str) {}
    fn bytes(&self, _: u64, _: u64) {}
}
impl Log for Quiet {
    fn info(&self, _: &'static str, _: &str) {}
}

/// A mower whose answers the test sets; calls lists what ran.
#[derive(Default)]
pub struct Fake {
    pub calls: RefCell<Vec<&'static str>>,
    pub absent: RefCell<bool>,
    pub serial: RefCell<String>,
    pub findings: RefCell<Vec<Finding>>,
    pub fail_write: RefCell<bool>,
    pub usb_again: RefCell<bool>,
}

impl Fake {
    pub fn ran(&self, call: &'static str) -> usize {
        self.calls.borrow().iter().filter(|c| **c == call).count()
    }
}

impl Mower for Fake {
    type Device = ();
    fn id(&self) -> &'static str {
        "fake"
    }
    fn guide(&self) -> Guide {
        Guide { steps: vec![] }
    }
    fn texts(&self) -> &'static [(&'static str, &'static str, &'static str)] {
        &[]
    }
    fn space_needed(&self) -> Space {
        Space {
            backup_bytes: 0,
            work_bytes: 0,
        }
    }
    async fn detect(&self) -> Result<Option<Found>> {
        Ok((!*self.absent.borrow()).then(|| Found {
            mode: UsbMode::Loader,
            bus_path: "1-1".into(),
        }))
    }
    async fn open(&self, _: &Found, _: &mut Ctx<'_>) -> Result<((), Identity)> {
        self.calls.borrow_mut().push("open");
        let table = vec![Part {
            name: "rootfs".into(),
            first_sector: 100,
            last_sector: 199,
        }];
        Ok((
            (),
            Identity {
                model: "fake".into(),
                board: None,
                serial_hash: self.serial.borrow().clone(),
                vendor_version: None,
                flash_sectors: 1000,
                table,
            },
        ))
    }
    async fn backup(&self, _: &mut (), _: &mut Ctx<'_>) -> Result<Backup> {
        self.calls.borrow_mut().push("backup");
        Ok(Backup {
            table: vec![],
            files: vec![],
            fingerprint: vec![],
        })
    }
    async fn check(&self, _: &mut (), _: &Backup, _: &mut Ctx<'_>) -> Result<Vec<Finding>> {
        self.calls.borrow_mut().push("check");
        Ok(self.findings.borrow().clone())
    }
    async fn prepare(
        &self,
        _: &Backup,
        _: &BaseImage,
        _: &Inputs,
        _: &mut Ctx<'_>,
    ) -> Result<WritePlan> {
        self.calls.borrow_mut().push("prepare");
        Ok(WritePlan {
            partition: "rootfs".into(),
            first_sector: 100,
            sectors: 50,
            partition_sectors: 100,
            image: PathBuf::new(),
            sha256: String::new(),
        })
    }
    async fn write(&self, _: (), _: &WritePlan, _: &mut Ctx<'_>) -> Result<()> {
        self.calls.borrow_mut().push("write");
        if *self.fail_write.borrow() {
            bail!("the cable came off");
        }
        Ok(())
    }
    async fn start_seen(&self) -> Result<StartSeen> {
        Ok(if *self.usb_again.borrow() {
            StartSeen::UsbAgain(Found {
                mode: UsbMode::Loader,
                bus_path: "1-1".into(),
            })
        } else {
            StartSeen::NotSeen
        })
    }
    async fn back(&self, _: (), _: &Backup, _: &mut Ctx<'_>) -> Result<()> {
        self.calls.borrow_mut().push("back");
        Ok(())
    }
}

pub struct World {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub work: WorkDir,
    pub tables: Tables,
    pub cancel: Cancel,
    pub base: BaseImage,
    pub inputs: Inputs,
}

impl World {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("backup")).unwrap();
        World {
            work: WorkDir {
                path: dir.path().join("work"),
            },
            store,
            dir,
            tables: Tables { raw: vec![] },
            cancel: Cancel::default(),
            base: BaseImage {
                path: PathBuf::new(),
                sha256: String::new(),
                pack_offset: 0,
                pack_len: 0,
            },
            inputs: Inputs {
                wifi_name: "Heim".into(),
                wifi_key: Secret::new("geheim123".into()),
                device_name: "mowbite-ab12".into(),
                setup_wifi_key: Secret::new("x".into()),
                bundles: vec![],
            },
        }
    }

    pub fn root(&self) -> PathBuf {
        self.store.root().to_path_buf()
    }

    pub async fn step(&mut self, driver: &mut Driver<'_, Fake>) -> Result<Next> {
        let mut ctx = Ctx {
            store: &mut self.store,
            work: &self.work,
            tables: &self.tables,
            progress: &Quiet,
            cancel: &self.cancel,
            log: &Quiet,
        };
        driver.step(&mut ctx, &self.base, &self.inputs).await
    }

    // Steps until the run needs the frame or the owner.
    pub async fn until_asked(&mut self, driver: &mut Driver<'_, Fake>) -> Result<Next> {
        loop {
            match self.step(driver).await? {
                Next::Go => continue,
                other => return Ok(other),
            }
        }
    }
}
