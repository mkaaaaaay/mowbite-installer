//! The steps of a run in their order, kept in .run.json of the backup folder after each one, so a run goes on after
//! a crash or a closed program. What the backup is (table, files, fingerprint) is also kept in backup.json beside
//! the saved areas, listed in SHA256SUMS, so a lost .run.json does not lose the way back.
//!
//! Before the first written block a run starts again at Connect and keeps its backup (a module resumes it, never
//! replaces it); the device has to be the same mower. From the first written block until the read back passed only
//! "write again" or "back" are offered, whatever goal the owner starts with, never a half written system.

use crate::mower::{
    Backup, BaseImage, Ctx, Finding, Identity, Inputs, Mower, NothingWritten, StartSeen, WritePlan,
    WrittenNotRestarted,
};
use crate::store::Store;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = ".run.json";
/// The backup described, in the store.
pub const BACKUP_FILE: &str = "backup.json";
/// 2: wrote and left_usb; a file of version 1 reads with both false.
const VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Goal {
    Install,
    /// Backup and checks only, nothing is written; install later goes on from the backup.
    Check,
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
    /// A check found a reason not to write; the backup stays. A new start checks again.
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
    /// The system on the device may be half written: set before writing, cleared once nothing was written or the
    /// read back passed.
    pub writing: bool,
    /// This folder wrote the device once (install or back).
    #[serde(default)]
    pub wrote: bool,
    /// After the restart the mower was gone from USB once; only coming back after that is a failed start.
    #[serde(default)]
    pub left_usb: bool,
}

/// backup.json: who the mower is and what its backup holds.
#[derive(Serialize, Deserialize)]
struct Described {
    mower: String,
    identity: Identity,
    backup: Backup,
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
            wrote: false,
            left_usb: false,
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
        let mut run: Run = serde_json::from_str(&text)?;
        if run.version > VERSION {
            bail!(
                "{} is of version {}, this program reads up to {VERSION}: use a newer installer",
                path.display(),
                run.version
            );
        }
        run.version = VERSION;
        Ok(Some(run))
    }

    /// A run made again from backup.json when .run.json is gone: the backup and the mower it belongs to.
    fn described(store_root: &Path, goal: Goal) -> Result<Option<Run>> {
        let store = Store::open(store_root)?;
        if store.recorded(BACKUP_FILE)?.is_none() {
            return Ok(None);
        }
        store.verify(BACKUP_FILE)?;
        let text = std::fs::read(store.path(BACKUP_FILE)?)?;
        let d: Described = serde_json::from_slice(&text).context(BACKUP_FILE)?;
        let mut run = Run::new(&d.mower, goal);
        run.identity = Some(d.identity);
        run.backup = Some(d.backup);
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

    /// The step after a restart of the program. A half written device gets "write again or back" whatever the goal.
    /// Another goal, a finished or stopped run start again at Connect with the backup kept, so the checks run again.
    fn resumed(mut self, goal: Goal) -> Self {
        if self.writing {
            self.step = Step::Write;
            return self;
        }
        if self.goal != goal || matches!(self.step, Step::Done | Step::Stopped) {
            self.goal = goal;
            self.step = Step::Connect;
            self.findings.clear();
            self.discard_plan();
            return self;
        }
        if self.step != Step::FirstStart {
            self.step = Step::Connect;
        }
        self
    }

    /// The prepared image holds the Wi-Fi keys: gone once it is not going to be written.
    fn discard_plan(&mut self) {
        if let Some(plan) = self.plan.take() {
            let _ = std::fs::remove_file(&plan.image);
        }
    }
}

/// What the frame has to do or ask before the next step.
#[derive(Debug, PartialEq)]
pub enum Next {
    Go,
    /// The owner connects the mower in USB mode (photo guide).
    Connect,
    /// The mower is on USB, but the system has no driver for it yet: the owner lets the frame install it.
    UsbDriver,
    /// The owner reads the warnings and agrees, or ends the run.
    Agree(Vec<Finding>),
    /// Writing began and did not finish, or the new system did not start: write again or go back.
    WriteOrBack,
    /// Written and read back, but the restart failed: the owner switches the mower off and on.
    SwitchOffAndOn,
    /// The mower left USB mode: the frame finds it on the network and confirms its start.
    Network,
    Stopped(Vec<Finding>),
    /// check: backup and checks are done, nothing was written; the findings for the owner.
    Checked(Vec<Finding>),
    Done,
}

pub struct Driver<'m, M: Mower> {
    mower: &'m M,
    pub run: Run,
    root: PathBuf,
    dev: Option<M::Device>,
    /// The owner chose "write again" or "back" in this start of the program.
    chosen: bool,
}

impl<'m, M: Mower> Driver<'m, M> {
    /// The kept run of this backup folder, else one made from its backup.json, else a new one.
    pub fn open(mower: &'m M, store_root: &Path, goal: Goal) -> Result<Self> {
        let kept = match Run::load(store_root)? {
            Some(run) => Some(run),
            None => Run::described(store_root, goal)?,
        };
        let run = match kept {
            Some(run) if run.mower != mower.id() => bail!("this backup belongs to a {}", run.mower),
            Some(run) if run.writing && goal == Goal::Check => {
                bail!(
                    "the system on the mower is not complete: start install or back with this backup folder"
                )
            }
            Some(run) => run.resumed(goal),
            None => Run::new(mower.id(), goal),
        };
        let driver = Driver {
            mower,
            run,
            root: store_root.to_path_buf(),
            dev: None,
            chosen: false,
        };
        driver.run.save(&driver.root)?;
        Ok(driver)
    }

    fn to(&mut self, step: Step) -> Result<()> {
        self.run.step = step;
        self.run.save(&self.root)
    }

    fn describe(&self) -> Result<()> {
        let mut store = Store::open(&self.root)?;
        if store.recorded(BACKUP_FILE)?.is_some() {
            return Ok(());
        }
        store.discard_unfinished(BACKUP_FILE)?;
        let described = Described {
            mower: self.run.mower.clone(),
            identity: self
                .run
                .identity
                .clone()
                .context("the mower is known at Backup")?,
            backup: self.run.backup.clone().context("made at Backup")?,
        };
        let mut file = store.create_new(BACKUP_FILE)?;
        file.write(&serde_json::to_vec_pretty(&described)?)?;
        file.finish()?;
        Ok(())
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
                self.describe()?;
                self.to(Step::Check)?;
                Ok(Next::Go)
            }
            Step::Check => {
                let dev = self.dev.as_mut().expect("opened at Connect");
                let backup = self.run.backup.as_ref().expect("made at Backup");
                let findings = self.mower.check(dev, backup, inputs, ctx).await?;
                let stop = findings.iter().any(|f| matches!(f, Finding::Stop { .. }));
                self.run.findings = findings;
                if stop {
                    self.to(Step::Stopped)?;
                    return Ok(Next::Stopped(self.run.findings.clone()));
                }
                if self.run.goal == Goal::Check {
                    self.to(Step::Done)?;
                    return Ok(Next::Checked(self.run.findings.clone()));
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
                inputs.unchanged()?;
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
                if self.run.writing && !self.chosen {
                    return Ok(Next::WriteOrBack);
                }
                self.write(ctx).await
            }
            Step::FirstStart => Ok(Next::Network),
            Step::Done => Ok(Next::Done),
            Step::Stopped => Ok(Next::Stopped(self.run.findings.clone())),
        }
    }

    async fn connect(&mut self, ctx: &mut Ctx<'_>) -> Result<Next> {
        let Some(found) = self.mower.detect().await? else {
            return Ok(Next::Connect);
        };
        if found.needs_driver {
            return Ok(Next::UsbDriver);
        }
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
        let prepared = self.run.plan.as_ref().is_some_and(|p| p.image.exists());
        let next = match (self.run.goal, self.run.backup.is_some()) {
            (Goal::Back, _) => Step::Write,
            (Goal::Install, _) if self.run.writing && prepared => Step::Write,
            (Goal::Install | Goal::Check, true) => Step::Check,
            (Goal::Install | Goal::Check, false) => Step::Backup,
        };
        self.to(next)?;
        Ok(Next::Go)
    }

    async fn write(&mut self, ctx: &mut Ctx<'_>) -> Result<Next> {
        let Some(dev) = self.dev.take() else {
            self.to(Step::Connect)?;
            return Ok(Next::Connect);
        };
        ensure!(self.run.goal != Goal::Check, "check never writes");
        let was_writing = self.run.writing;
        self.run.writing = true;
        self.run.save(&self.root)?;
        let (result, after) = match self.run.goal {
            Goal::Install => {
                let plan = self.run.plan.as_ref().expect("made at Prepare");
                (self.mower.write(dev, plan, ctx).await, Step::FirstStart)
            }
            Goal::Back => {
                let backup = self.run.backup.as_ref().expect("the backup of this mower");
                (self.mower.back(dev, backup, ctx).await, Step::Done)
            }
            Goal::Check => unreachable!("refused above"),
        };
        match result {
            Ok(()) => {
                self.written(after)?;
                Ok(Next::Go)
            }
            Err(e) if e.downcast_ref::<WrittenNotRestarted>().is_some() => {
                self.written(after)?;
                Ok(Next::SwitchOffAndOn)
            }
            Err(e) if e.downcast_ref::<NothingWritten>().is_some() => {
                self.run.writing = was_writing;
                self.to(Step::Connect)?;
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    fn written(&mut self, after: Step) -> Result<()> {
        self.run.writing = false;
        self.run.wrote = true;
        self.run.left_usb = false;
        self.run.plan = None;
        self.to(after)
    }

    /// While the frame waits for the network: whether the mower came back in USB mode after it had left it (a
    /// failed start, or the end of its trial period); then the frame offers write again or back.
    pub async fn back_in_usb(&mut self) -> Result<bool> {
        match self.mower.start_seen().await? {
            StartSeen::NotSeen => {
                if !self.run.left_usb {
                    self.run.left_usb = true;
                    self.run.save(&self.root)?;
                }
                Ok(false)
            }
            StartSeen::UsbAgain(_) => Ok(self.run.left_usb),
        }
    }

    /// The owner agreed to the warnings.
    pub fn agree(&mut self) -> Result<()> {
        if self.run.step != Step::Agree {
            bail!("nothing to agree to now");
        }
        self.to(Step::Prepare)
    }

    /// After WriteOrBack: write MowBite OS again (through Prepare, the image was removed after the read back), or
    /// the backed up system.
    pub fn choose(&mut self, back: bool) -> Result<()> {
        if back {
            self.run.goal = Goal::Back;
        }
        if self.run.plan.as_ref().is_some_and(|p| !p.image.exists()) {
            self.run.plan = None;
        }
        self.chosen = true;
        self.to(Step::Connect)
    }

    /// Whether an error here is mended by plugging in again: before writing a lost cable or a time out is.
    pub fn mendable(&self) -> bool {
        !self.run.writing
            && matches!(
                self.run.step,
                Step::Connect | Step::Backup | Step::Check | Step::Write
            )
    }

    /// After an error before writing: the device is opened again from Connect.
    pub fn reconnect(&mut self) -> Result<()> {
        if self.run.writing {
            bail!("the system on the device is not complete: write again or go back");
        }
        self.dev = None;
        self.to(Step::Connect)
    }

    /// The frame ends the run: without writing the prepared image goes. Ok(true) when nothing was written.
    pub fn leave(&mut self) -> Result<bool> {
        if self.run.writing {
            return Ok(false);
        }
        self.run.discard_plan();
        self.run.save(&self.root)?;
        Ok(true)
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
    use crate::testing::{Fake, World};

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

    // Nothing written in a second attempt leaves the first one half written.
    #[tokio::test]
    async fn nothing_written_again_keeps_a_half_written_device() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.fail_write.borrow_mut() = true;
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert!(w.until_asked(&mut d).await.is_err());
        *fake.fail_write.borrow_mut() = false;
        drop(d);
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::WriteOrBack);
        d.choose(false).unwrap();
        *fake.write_error.borrow_mut() = Some(anyhow::anyhow!("cancelled").context(NothingWritten));
        assert!(w.until_asked(&mut d).await.is_err());
        assert!(d.run.writing);
        drop(d);
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::WriteOrBack);
        assert_eq!(fake.ran("write"), 2);
    }

    #[tokio::test]
    async fn a_failed_start_offers_write_again() {
        let mut w = World::new();
        let fake = Fake::default();
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        // Still listed right after the restart, gone, then back: only the last counts.
        *fake.seen.borrow_mut() = vec![true, false, true];
        assert!(!d.back_in_usb().await.unwrap());
        assert!(!d.back_in_usb().await.unwrap());
        assert!(d.back_in_usb().await.unwrap());
        d.choose(false).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        assert_eq!(fake.ran("write"), 2);
        assert!(!d.run.left_usb);
        assert_eq!(fake.ran("backup"), 1);
    }

    // A copy without dot files or a deleted .run.json: the way back comes from backup.json.
    #[tokio::test]
    async fn a_lost_run_file_is_made_again_from_the_backup() {
        let mut w = World::new();
        let fake = Fake::default();
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        drop(d);
        std::fs::remove_file(Run::path(&w.root())).unwrap();
        let mut d = Driver::open(&fake, &w.root(), Goal::Back).unwrap();
        assert!(d.run.backup.is_some() && d.run.identity.is_some());
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Done);
        assert_eq!((fake.ran("backup"), fake.ran("back")), (1, 1));
    }

    // A run file of version 1 reads; one of a newer installer does not.
    #[test]
    fn older_run_files_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut run = serde_json::to_value(Run::new("tron", Goal::Install)).unwrap();
        run["version"] = 1.into();
        run.as_object_mut().unwrap().remove("wrote");
        run.as_object_mut().unwrap().remove("left_usb");
        std::fs::write(Run::path(dir.path()), run.to_string()).unwrap();
        let read = Run::load(dir.path()).unwrap().unwrap();
        assert_eq!((read.version, read.wrote), (VERSION, false));
        run["version"] = (VERSION + 1).into();
        std::fs::write(Run::path(dir.path()), run.to_string()).unwrap();
        assert!(Run::load(dir.path()).is_err());
    }

    #[tokio::test]
    async fn back_needs_a_backup_of_this_mower() {
        let mut w = World::new();
        let fake = Fake::default();
        let mut d = Driver::open(&fake, &w.root(), Goal::Back).unwrap();
        assert!(w.until_asked(&mut d).await.is_err());
        assert_eq!(fake.ran("back"), 0);
    }

    #[tokio::test]
    async fn back_and_install_again_after_a_finished_run() {
        let mut w = World::new();
        let fake = Fake::default();
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        d.started().unwrap();
        drop(d);
        let mut d = Driver::open(&fake, &w.root(), Goal::Back).unwrap();
        assert_eq!((d.run.goal, d.run.step), (Goal::Back, Step::Connect));
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Done);
        assert_eq!(fake.ran("back"), 1);
        drop(d);
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Network);
        assert_eq!((fake.ran("backup"), fake.ran("write")), (1, 2));
    }

    #[tokio::test]
    async fn back_after_a_stop_and_during_an_unfinished_write() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.fail_write.borrow_mut() = true;
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert!(w.until_asked(&mut d).await.is_err());
        drop(d);
        // Half written: whatever the goal, first write again or back.
        let mut d = Driver::open(&fake, &w.root(), Goal::Back).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::WriteOrBack);
        d.choose(true).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Done);
        assert_eq!(fake.ran("back"), 1);

        let mut w = World::new();
        let fake = Fake::default();
        *fake.findings.borrow_mut() = vec![Finding::Stop {
            code: "tron.root_verified".into(),
            details: String::new(),
        }];
        let mut d = Driver::open(&fake, &w.root(), Goal::Install).unwrap();
        assert!(matches!(
            w.until_asked(&mut d).await.unwrap(),
            Next::Stopped(_)
        ));
        drop(d);
        let mut d = Driver::open(&fake, &w.root(), Goal::Back).unwrap();
        assert_eq!(w.until_asked(&mut d).await.unwrap(), Next::Done);
        assert_eq!((fake.ran("back"), fake.ran("write")), (1, 0));
    }

    #[test]
    fn another_module_does_not_take_this_backup() {
        let dir = tempfile::tempdir().unwrap();
        Run::new("other", Goal::Install).save(dir.path()).unwrap();
        assert!(Driver::open(&Fake::default(), dir.path(), Goal::Install).is_err());
    }
}
