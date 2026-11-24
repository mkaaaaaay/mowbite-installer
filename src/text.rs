//! The text mode of the frame: runs a Driver to its end and asks the owner in the terminal. The web page uses the
//! same Driver; this mode serves where no browser is wanted and in the first runs on our own mower.

use crate::awake::Awake;
use crate::mower::{
    BaseImage, Cancel, Ctx, Finding, Inputs, Log, Mower, Progress, Tables, WorkDir,
};
use crate::run::{Driver, Goal, Next};
use crate::store::Store;
use anyhow::Result;
use std::io::Write;
use std::time::{Duration, Instant};

/// How often the frame looks for the mower in USB mode, and for its first start on the network.
const LOOK: Duration = Duration::from_secs(2);
const NETWORK_WAIT: Duration = Duration::from_secs(15 * 60);

/// The owner at the terminal; tests give their own answers.
pub trait Owner {
    fn say(&self, text: &str);
    /// true only on an explicit yes.
    fn agree(&self, question: &str) -> bool;
    /// true for "back", false for "write again".
    fn back(&self) -> bool;
}

/// The network part of the first start: find the mower and confirm its trial period (host.confirm_start).
#[allow(async_fn_in_trait)]
pub trait FirstStart {
    /// Ok(None) once confirmed, Ok(Some(why)) while not yet.
    async fn confirm(&self) -> Result<Option<String>>;
}

pub struct Terminal;

impl Owner for Terminal {
    fn say(&self, text: &str) {
        println!("{text}");
    }

    fn agree(&self, question: &str) -> bool {
        print!("{question} [yes/no] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("yes")
    }

    fn back(&self) -> bool {
        loop {
            print!("Write MowBite OS again (w) or go back to the vendor system (b)? ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).is_err() {
                return true;
            }
            match line.trim() {
                "w" => return false,
                "b" => return true,
                _ => {}
            }
        }
    }
}

/// Progress on stderr, at most once a second.
pub struct Bar {
    last: std::cell::Cell<Option<Instant>>,
}

impl Bar {
    pub fn new() -> Self {
        Bar {
            last: std::cell::Cell::new(None),
        }
    }
}

impl Default for Bar {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress for Bar {
    fn step(&self, code: &'static str) {
        eprintln!("== {code}");
    }

    fn bytes(&self, done: u64, total: u64) {
        let now = Instant::now();
        if self
            .last
            .get()
            .is_some_and(|t| now - t < Duration::from_secs(1))
            && done < total
        {
            return;
        }
        self.last.set(Some(now));
        eprintln!("   {} of {} MB", done >> 20, total >> 20);
    }
}

/// The log of the run in the backup folder; it takes text only, never a Secret.
pub struct FileLog {
    path: std::path::PathBuf,
}

impl FileLog {
    pub fn new(store: &Store) -> Self {
        FileLog {
            path: store.root().join("run.log"),
        }
    }
}

impl Log for FileLog {
    fn info(&self, code: &'static str, details: &str) {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{t} {code} {}", details.replace('\n', " "));
        }
    }
}

fn show(owner: &dyn Owner, findings: &[Finding]) {
    for finding in findings {
        let (kind, code, details) = match finding {
            Finding::Stop { code, details } => ("STOP", code, details),
            Finding::Warn { code, details } => ("WARNING", code, details),
            Finding::Info { code, details } => ("note", code, details),
        };
        owner.say(&format!("  {kind} {code}: {details}"));
    }
}

/// The whole run in the terminal. Ok(true) when MowBite OS (or the vendor system on "back") is on the mower.
#[allow(clippy::too_many_arguments)]
pub async fn run<M: Mower, F: FirstStart>(
    mower: &M,
    store: &mut Store,
    work: &WorkDir,
    tables: &Tables,
    base: &BaseImage,
    inputs: &Inputs,
    goal: Goal,
    first: &F,
    owner: &dyn Owner,
    cancel: &Cancel,
) -> Result<bool> {
    let root = store.root().to_path_buf();
    let mut driver = Driver::open(mower, &root, goal)?;
    let _awake = match Awake::hold("MowBite installer reads or writes the mower") {
        Ok(awake) => Some(awake),
        Err(_) => {
            owner.say("Keep this PC awake until the end: it could not be kept from sleeping.");
            None
        }
    };
    let log = FileLog::new(store);
    let bar = Bar::new();
    let mut waiting_since: Option<Instant> = None;
    loop {
        if cancel.is_set() && !driver.run.writing {
            owner.say("Stopped by the owner; nothing was written. Start again to go on.");
            return Ok(false);
        }
        let next = {
            let mut ctx = Ctx {
                store,
                work,
                tables,
                progress: &bar,
                cancel,
                log: &log,
            };
            driver.step(&mut ctx, base, inputs).await?
        };
        match next {
            Next::Go => {}
            Next::Connect => {
                if waiting_since.is_none() {
                    owner.say("Connect the mower in USB mode as the guide shows.");
                    for (key, photo) in mower.guide().steps {
                        owner.say(&format!("  {key} ({photo})"));
                    }
                    waiting_since = Some(Instant::now());
                }
                tokio::time::sleep(LOOK).await;
            }
            Next::Agree(warnings) => {
                owner.say("Before writing:");
                show(owner, &warnings);
                if !owner.agree("Install MowBite OS anyway?") {
                    owner.say("Nothing was written. The backup stays.");
                    return Ok(false);
                }
                driver.agree()?;
            }
            Next::WriteOrBack => {
                let back = owner.back();
                driver.choose(back)?;
                waiting_since = None;
            }
            Next::Network => {
                owner.say("The mower starts MowBite OS. Looking for it on the network.");
                let until = tokio::time::Instant::now() + NETWORK_WAIT;
                loop {
                    let why = match first.confirm().await {
                        Ok(None) => break,
                        Ok(Some(why)) => why,
                        Err(error) => error.to_string(),
                    };
                    log.info("first_start_wait", &why);
                    if tokio::time::Instant::now() < until {
                        tokio::time::sleep(LOOK).await;
                    } else {
                        owner.say("The mower was not confirmed from here: confirm its first start in the app.");
                        return Ok(false);
                    }
                }
                driver.started()?;
            }
            Next::Stopped(findings) => {
                owner.say("MowBite OS cannot be installed on this mower:");
                show(owner, &findings);
                owner.say("Nothing was written. The backup stays.");
                return Ok(false);
            }
            Next::Done => {
                owner.say(match driver.run.goal {
                    Goal::Install => "Done: MowBite OS runs on the mower.",
                    Goal::Back => "Done: the vendor system is back on the mower.",
                });
                return Ok(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Fake, World};
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct Script {
        said: RefCell<Vec<String>>,
        yes: Cell<bool>,
        backs: RefCell<Vec<bool>>,
    }

    impl Owner for Script {
        fn say(&self, text: &str) {
            self.said.borrow_mut().push(text.to_owned());
        }
        fn agree(&self, _: &str) -> bool {
            self.yes.get()
        }
        fn back(&self) -> bool {
            self.backs.borrow_mut().remove(0)
        }
    }

    struct Confirms(RefCell<Vec<Option<String>>>);

    impl FirstStart for Confirms {
        async fn confirm(&self) -> Result<Option<String>> {
            let mut left = self.0.borrow_mut();
            Ok(if left.is_empty() {
                None
            } else {
                left.remove(0)
            })
        }
    }

    async fn go(
        w: &mut World,
        fake: &Fake,
        owner: &Script,
        confirms: Vec<Option<String>>,
    ) -> Result<bool> {
        let first = Confirms(RefCell::new(confirms));
        run(
            fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &w.inputs,
            Goal::Install,
            &first,
            owner,
            &w.cancel,
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn installs_and_confirms_the_first_start() {
        let mut w = World::new();
        let fake = Fake::default();
        let owner = Script::default();
        assert!(
            go(&mut w, &fake, &owner, vec![Some("not found".into())])
                .await
                .unwrap()
        );
        assert_eq!(fake.ran("write"), 1);
        let log = std::fs::read_to_string(w.store.root().join("run.log")).unwrap();
        assert!(log.contains("first_start_wait not found"));
        assert!(!log.contains("geheim123"));
    }

    #[tokio::test(start_paused = true)]
    async fn warnings_need_a_yes() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.findings.borrow_mut() = vec![Finding::Warn {
            code: "tron.vendor_unknown".into(),
            details: "1.3.40".into(),
        }];
        let owner = Script::default();
        assert!(!go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!(fake.ran("write"), 0);
        owner.yes.set(true);
        assert!(go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!(fake.ran("write"), 1);
        assert!(
            owner
                .said
                .borrow()
                .iter()
                .any(|s| s.contains("tron.vendor_unknown"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_start_goes_back_on_the_owners_word() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.usb_again.borrow_mut() = true;
        let owner = Script::default();
        owner.backs.borrow_mut().push(true);
        assert!(go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!(fake.ran("back"), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancel_before_writing_writes_nothing() {
        let mut w = World::new();
        let fake = Fake::default();
        w.cancel.set();
        assert!(!go(&mut w, &fake, &Script::default(), vec![]).await.unwrap());
        assert_eq!(fake.ran("write"), 0);
    }
}
