//! The steps of a run in their order, kept in .run.json of the backup folder after each one, so a run goes on after
//! a crash or a closed program.
//!
//! Before the first written block a run starts again at Connect and keeps its backup (a module resumes it, never
//! replaces it); the device has to be the same mower. From the first written block on only "write again" or "back"
//! are offered, never a half written system.

use crate::mower::{
    Backup, BaseImage, Ctx, Finding, Identity, Inputs, Mower, StartSeen, WritePlan,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = ".run.json";
const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Goal {
    Install,
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Connect,
    Backup,
    Check,
    Agree,
    Prepare,
    Write,
    FirstStart,
    Done,
    /// A check found a reason not to write; the backup stays.
    Stopped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub version: u32,
    pub mower: String,
    pub goal: Goal,
    pub step: Step,
    pub identity: Option<Identity>,
    pub backup: Option<Backup>,
    pub findings: Vec<Finding>,
    pub plan: Option<WritePlan>,
    /// Set before the first block is written.
    pub writing: bool,
}

impl Run {
    pub fn new(mower: &str, goal: Goal) -> Self {
        Run {
            version: VERSION,
            mower: mower.to_owned(),
            goal,
            step: Step::Connect,
            identity: None,
            backup: None,
            findings: Vec::new(),
            plan: None,
            writing: false,
        }
    }

    pub fn path(store_root: &Path) -> PathBuf {
        store_root.join(FILE)
    }

    pub fn load(store_root: &Path) -> Result<Option<Run>> {
        let path = Self::path(store_root);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let run: Run = serde_json::from_str(&text)?;
        if run.version != VERSION {
            bail!(
                "{} is of version {}, this program reads {VERSION}",
                path.display(),
                run.version
            );
        }
        Ok(Some(run))
    }

    /// Whole or not at all: a new file, synced, then renamed over the old one.
    pub fn save(&self, store_root: &Path) -> Result<()> {
        let path = Self::path(store_root);
        let tmp = path.with_extension("json.new");
        {
            let mut file = std::fs::File::create(&tmp)?;
            serde_json::to_writer_pretty(&mut file, self)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// The step after a restart of the program.
    fn resumed(mut self) -> Self {
        self.step = match self.step {
            Step::Done | Step::Stopped | Step::FirstStart => self.step,
            _ if self.writing => Step::Write,
            _ => Step::Connect,
        };
        self
    }
}

/// What the frame has to do or ask before the next step.
#[derive(Debug, PartialEq)]
pub enum Next {
    Go,
    /// The owner connects the mower in USB mode (photo guide).
    Connect,
    /// The owner reads the warnings and agrees, or ends the run.
    Agree(Vec<Finding>),
    /// Writing began and did not finish, or the new system did not start: write again or go back.
    WriteOrBack,
    /// The mower left USB mode: the frame finds it on the network and confirms its start.
    Network,
    Stopped(Vec<Finding>),
    Done,
}

pub struct Driver<'m, M: Mower> {
    mower: &'m M,
    pub run: Run,
    root: PathBuf,
    dev: Option<M::Device>,
}

impl<'m, M: Mower> Driver<'m, M> {
    /// The kept run of this backup folder, else a new one.
    pub fn open(mower: &'m M, store_root: &Path, goal: Goal) -> Result<Self> {
        let run = match Run::load(store_root)? {
            Some(run) if run.mower != mower.id() => bail!("this backup belongs to a {}", run.mower),
            Some(run) => run.resumed(),
            None => Run::new(mower.id(), goal),
        };
        let driver = Driver {
            mower,
            run,
            root: store_root.to_path_buf(),
            dev: None,
        };
        driver.run.save(&driver.root)?;
        Ok(driver)
    }

    fn to(&mut self, step: Step) -> Result<()> {
        self.run.step = step;
        self.run.save(&self.root)
    }

    /// Runs the current step; Next tells what the frame does before calling again.
    pub async fn step(
        &mut self,
        ctx: &mut Ctx<'_>,
        base: &BaseImage,
        inputs: &Inputs,
    ) -> Result<Next> {
        match self.run.step {
            Step::Connect => self.connect(ctx).await,
            Step::Backup => {
                let dev = self.dev.as_mut().expect("opened at Connect");
                let backup = self.mower.backup(dev, ctx).await?;
                self.run.backup = Some(backup);
                self.to(Step::Check)?;
                Ok(Next::Go)
            }
            Step::Check => {
                let dev = self.dev.as_mut().expect("opened at Connect");
                let backup = self.run.backup.as_ref().expect("made at Backup");
                let findings = self.mower.check(dev, backup, ctx).await?;
                let stop = findings.iter().any(|f| matches!(f, Finding::Stop { .. }));
                self.run.findings = findings;
                if stop {
                    self.to(Step::Stopped)?;
                    return Ok(Next::Stopped(self.run.findings.clone()));
                }
                self.to(Step::Agree)?;
                Ok(Next::Go)
            }
            Step::Agree => {
                let warnings: Vec<Finding> = self
                    .run
                    .findings
                    .iter()
                    .filter(|f| matches!(f, Finding::Warn { .. }))
                    .cloned()
                    .collect();
                if warnings.is_empty() {
                    self.to(Step::Prepare)?;
                    return Ok(Next::Go);
                }
                Ok(Next::Agree(warnings))
            }
            Step::Prepare => {
                let backup = self.run.backup.as_ref().expect("made at Backup");
                let plan = self.mower.prepare(backup, base, inputs, ctx).await?;
                if !plan.fits() {
                    bail!("the prepared image does not fit its partition");
                }
                self.run.plan = Some(plan);
                self.to(Step::Write)?;
                Ok(Next::Go)
            }
            Step::Write => {
                if self.run.writing {
                    return Ok(Next::WriteOrBack);
                }
                self.write(ctx).await
            }
            Step::FirstStart => match self.mower.start_seen().await? {
                StartSeen::UsbAgain(_) => Ok(Next::WriteOrBack),
                StartSeen::NotSeen => Ok(Next::Network),
            },
            Step::Done => Ok(Next::Done),
            Step::Stopped => Ok(Next::Stopped(self.run.findings.clone())),
        }
    }

    async fn connect(&mut self, ctx: &mut Ctx<'_>) -> Result<Next> {
        let Some(found) = self.mower.detect().await? else {
            return Ok(Next::Connect);
        };
        let (dev, identity) = self.mower.open(&found, ctx).await?;
        if let Some(known) = &self.run.identity {
            if known.serial_hash != identity.serial_hash || known.table != identity.table {
                bail!("this is another mower than the one of this backup");
            }
        }
        if self.run.goal == Goal::Back && self.run.backup.is_none() {
            bail!("no backup of this mower in this folder");
        }
        self.run.identity = Some(identity);
        self.dev = Some(dev);
        let next = match (self.run.goal, self.run.writing, self.run.backup.is_some()) {
            (Goal::Back, _, _) | (_, true, _) => Step::Write,
            (Goal::Install, false, true) => Step::Check,
            (Goal::Install, false, false) => Step::Backup,
        };
        self.to(next)?;
        Ok(Next::Go)
    }

    async fn write(&mut self, ctx: &mut Ctx<'_>) -> Result<Next> {
        let Some(dev) = self.dev.take() else {
            self.to(Step::Connect)?;
            return Ok(Next::Connect);
        };
        self.run.writing = true;
        self.run.save(&self.root)?;
        match self.run.goal {
            Goal::Install => {
                let plan = self.run.plan.as_ref().expect("made at Prepare");
                self.mower.write(dev, plan, ctx).await?;
                self.run.writing = false;
                self.to(Step::FirstStart)?;
            }
            Goal::Back => {
                let backup = self.run.backup.as_ref().expect("the backup of this mower");
                self.mower.back(dev, backup, ctx).await?;
                self.run.writing = false;
                self.to(Step::Done)?;
            }
        }
        Ok(Next::Go)
    }

    /// The owner agreed to the warnings.
    pub fn agree(&mut self) -> Result<()> {
        if self.run.step != Step::Agree {
            bail!("nothing to agree to now");
        }
        self.to(Step::Prepare)
    }

    /// After WriteOrBack: write the prepared image again, or the backed up system.
    pub fn choose(&mut self, back: bool) -> Result<()> {
        if back {
            self.run.goal = Goal::Back;
        }
        self.run.writing = false;
        self.to(Step::Connect)
    }

    /// The frame confirmed the first start on the network.
    pub fn started(&mut self) -> Result<()> {
        if self.run.step != Step::FirstStart {
            bail!("no first start to confirm now");
        }
        self.to(Step::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mower::{
        Cancel, Found, Guide, Log, Part, Progress, Space, Tables, UsbMode, WorkDir,
    };
    use crate::secret::Secret;
    use crate::store::Store;
    use std::cell::RefCell;

    struct Quiet;
    impl Progress for Quiet {
        fn step(&self, _: &'static str) {}
        fn bytes(&self, _: u64, _: u64) {}
    }
    impl Log for Quiet {
        fn info(&self, _: &'static str, _: &str) {}
    }

    /// A mower whose answers the test sets; calls lists what ran.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<&'static str>>,
        absent: RefCell<bool>,
        serial: RefCell<String>,
        findings: RefCell<Vec<Finding>>,
        fail_write: RefCell<bool>,
        usb_again: RefCell<bool>,
    }

    impl Fake {
        fn ran(&self, call: &'static str) -> usize {
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

    struct World {
        dir: tempfile::TempDir,
        store: Store,
        work: WorkDir,
        tables: Tables,
        cancel: Cancel,
        base: BaseImage,
        inputs: Inputs,
    }

    impl World {
        fn new() -> Self {
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

        fn root(&self) -> PathBuf {
            self.store.root().to_path_buf()
        }

        async fn step(&mut self, driver: &mut Driver<'_, Fake>) -> Result<Next> {
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
        async fn until_asked(&mut self, driver: &mut Driver<'_, Fake>) -> Result<Next> {
            loop {
                match self.step(driver).await? {
                    Next::Go => continue,
                    other => return Ok(other),
                }
            }
        }
    }

    fn warn() -> Finding {
        Finding::Warn {
            code: "tron.vendor_unknown".into(),
            details: String::new(),
        }
    }

    #[tokio::test]
    async fn a_whole_run() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.absent.borrow_mut() = true;
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Connect);
        *fake.absent.borrow_mut() = false;
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        d.started().unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Done);
        assert_eq!(
            *fake.calls.borrow(),
            ["open", "backup", "check", "prepare", "write"]
        );
        assert_eq!(Run::load(&w.root()).unwrap().unwrap().step, Step::Done);
        drop(w.dir);
    }

    #[tokio::test]
    async fn warnings_wait_for_the_owner_and_a_stop_ends_before_writing() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.findings.borrow_mut() = vec![warn()];
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(
            w.until_asked(&mut d).await.unwrap(),
            Next::Agree(vec![warn()])
        );
        assert_eq!(
            w.until_asked(&mut d).await.unwrap(),
            Next::Agree(vec![warn()])
        );
        assert_eq!(fake.ran("prepare"), 0);
        d.agree().unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);

        let mut w = World::new();
        let fake = Fake::default();
        let stop = Finding::Stop {
            code: "tron.root_verified".into(),
            details: String::new(),
        };
        *fake.findings.borrow_mut() = vec![warn(), stop.clone()];
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert!(
            matches!(w.until_asked(&mut d).await.unwrap(), Next::Stopped(f) if f.contains(&stop))
        );
        assert!(d.agree().is_err());
        assert_eq!(fake.ran("write"), 0);
        let mut again = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert!(matches!(
            w.until_asked(&mut again).await.unwrap(),
            Next::Stopped(_)
        ));
    }

    #[tokio::test]
    async fn a_restart_before_writing_keeps_the_backup_and_wants_the_same_mower() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.findings.borrow_mut() = vec![warn()];
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        w.until_asked(&mut d).await.unwrap();
        drop(d);
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(d.run.step, Step::Connect);
        assert!(matches!(
            w.until_asked(&mut d).await.unwrap(),
            Next::Agree(_)
        ));
        assert_eq!(fake.ran("backup"), 1);
        assert_eq!(fake.ran("check"), 2);

        drop(d);
        *fake.serial.borrow_mut() = "another".into();
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert!(w.until_asked(&mut d).await.is_err());
        assert_eq!(fake.ran("prepare"), 0);
    }

    #[tokio::test]
    async fn an_unfinished_write_offers_only_write_again_or_back() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.fail_write.borrow_mut() = true;
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert!(w.until_asked(&mut d).await.is_err());
        drop(d);
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(d.run.step, Step::Write);
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::WriteOrBack);
        assert!(d.agree().is_err());
        d.choose(true).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Done);
        assert_eq!(fake.ran("back"), 1);
        assert_eq!(fake.ran("write"), 1);
    }

    #[tokio::test]
    async fn a_failed_start_offers_write_again() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.usb_again.borrow_mut() = true;
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::WriteOrBack);
        *fake.usb_again.borrow_mut() = false;
        d.choose(false).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        assert_eq!(fake.ran("write"), 2);
        assert_eq!(fake.ran("backup"), 1);
    }

    #[tokio::test]
    async fn back_needs_a_backup_of_this_mower() {
        let mut w = World::new();
        let fake = Fake::default();
        let mut d = Driver::open(&fake, &w.root(), Goal::Back).unwrap();
        assert!(w.until_asked(&mut d).await.is_err());
        assert_eq!(fake.ran("back"), 0);
    }

    #[test]
    fn another_module_does_not_take_this_backup() {
        let dir = tempfile::tempdir().unwrap();
        Run::new("other", Goal::Install).save(dir.path()).unwrap();
        assert!(Driver::open(&Fake::default(), dir.path(), Goal::Install).is_err());
    }
}
