//! The text mode of the frame: runs a Driver to its end and asks the owner in the terminal. The web page uses the
//! same Driver; this mode serves where no browser is wanted and in the first runs on our own mower.

use crate::awake::Awake;
use crate::mower::{
    BaseImage, Cancel, Ctx, Finding, Inputs, Log, Mower, Progress, Tables, WorkDir,
};
use crate::run::{Driver, Goal, Next, Step};
use crate::store::Store;
use anyhow::Result;
use std::io::Write;
use std::time::{Duration, Instant};

/// How often the frame looks for the mower in USB mode, and for its first start on the network.
const LOOK: Duration = Duration::from_secs(2);
/// Longer than the trial period of the mower (10 min after its start, then back into USB mode) with its boot.
const NETWORK_WAIT: Duration = Duration::from_secs(20 * 60);
/// After this the owner hears of the setup Wi-Fi, which the mower opens 3 min after a start without its Wi-Fi.
const SETUP_HINT: Duration = Duration::from_secs(4 * 60);

/// The owner at the terminal; tests give their own answers.
pub trait Owner {
    fn say(&self, text: &str);
    /// true only on an explicit yes.
    fn agree(&self, question: &str) -> bool;
    /// Some(true) for "back", Some(false) for "write again", None when no answer can come.
    fn back(&self, goal: Goal) -> Option<bool>;
    /// true when the owner plugged the cable in again and wants to go on.
    fn again(&self) -> bool;
}

/// The network part of the first start: find the mower and confirm its trial period (host.confirm_start).
#[allow(async_fn_in_trait)]
pub trait FirstStart {
    /// Ok(None) once confirmed, Ok(Some(why)) while not yet.
    async fn confirm(&self) -> Result<Option<String>>;
}

pub struct Terminal;

/// One line from the terminal; None at the end of the input or on a read error.
fn line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_owned()),
    }
}

impl Owner for Terminal {
    fn say(&self, text: &str) {
        println!("{text}");
    }

    fn agree(&self, question: &str) -> bool {
        print!("{question} [yes/no] ");
        let _ = std::io::stdout().flush();
        line().is_some_and(|l| l.eq_ignore_ascii_case("yes"))
    }

    fn back(&self, goal: Goal) -> Option<bool> {
        loop {
            print!(
                "{} Write MowBite OS again (w) or go back to the vendor system (b)? ",
                match goal {
                    Goal::Install => "The system on the mower is not complete or did not start.",
                    Goal::Back => "Writing the vendor system back did not finish.",
                }
            );
            let _ = std::io::stdout().flush();
            match line()?.as_str() {
                "w" => return Some(false),
                "b" => return Some(true),
                _ => {}
            }
        }
    }

    fn again(&self) -> bool {
        print!("Plug the cable in again and press Enter to go on, or q and Enter to stop: ");
        let _ = std::io::stdout().flush();
        line().is_some_and(|l| !l.eq_ignore_ascii_case("q"))
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

/// Texts in German when the system speaks it, else English.
fn german() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| v.starts_with("de"))
}

/// The text of a code, or the code when the module has none.
fn text_of<'a>(texts: &[(&str, &'a str, &'a str)], code: &'a str) -> &'a str {
    texts
        .iter()
        .find(|(key, _, _)| *key == code)
        .map_or(code, |&(_, de, en)| if german() { de } else { en })
}

/// An error whose message starts with a code ("tron.no_serial: ...") with the text of the code instead.
fn explain(texts: &[(&str, &str, &str)], error: &anyhow::Error) -> String {
    let message = format!("{error:#}");
    match message.split_once(": ") {
        Some((code, rest)) if texts.iter().any(|(key, _, _)| *key == code) => {
            format!("{} ({rest})", text_of(texts, code))
        }
        _ => message,
    }
}

fn show(owner: &dyn Owner, texts: &[(&str, &str, &str)], findings: &[Finding]) {
    for finding in findings {
        let (kind, code, details) = match finding {
            Finding::Stop { code, details } => ("STOP", code, details),
            Finding::Warn { code, details } => ("WARNING", code, details),
            Finding::Info { code, details } => ("note", code, details),
        };
        let text = text_of(texts, code);
        if details.is_empty() {
            owner.say(&format!("  {kind}: {text}"));
        } else {
            owner.say(&format!("  {kind}: {text} ({details})"));
        }
    }
}

const NOTHING_WRITTEN: &str = "Nothing was written. The backup stays.";
const INCOMPLETE: &str = "The system on the mower is not complete. Start the installer again with this backup \
     folder: it offers to write again or to go back.";

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
    let texts = mower.texts();
    let mut driver = Driver::open(mower, &root, goal)?;
    if goal == Goal::Back
        && !driver.run.wrote
        && !driver.run.writing
        && !owner.agree(
            "This backup folder never wrote to the mower: it runs the system the backup was made from, or one \
             written from elsewhere. Write the backup back anyway?",
        )
    {
        owner.say(NOTHING_WRITTEN);
        return Ok(false);
    }
    let _awake = match Awake::hold("MowBite installer reads or writes the mower") {
        Ok(awake) => Some(awake),
        Err(_) => {
            owner.say(
                "Keep this PC awake until the end, lid open: it could not be kept from sleeping.",
            );
            None
        }
    };
    let log = FileLog::new(store);
    let bar = Bar::new();
    let mut waiting_since: Option<Instant> = None;
    let leave = |driver: &mut Driver<'_, M>, owner: &dyn Owner| -> Result<bool> {
        if driver.run.writing {
            owner.say(INCOMPLETE);
        } else {
            driver.leave()?;
            owner.say(NOTHING_WRITTEN);
        }
        Ok(false)
    };
    loop {
        if cancel.is_set() && !driver.run.writing {
            owner.say("Stopped by the owner.");
            return leave(&mut driver, owner);
        }
        let result = {
            let mut ctx = Ctx {
                store,
                work,
                tables,
                progress: &bar,
                cancel,
                log: &log,
                system_written: driver.run.wrote || driver.run.writing,
            };
            driver.step(&mut ctx, base, inputs).await
        };
        let next = match result {
            Ok(next) => next,
            Err(error) => {
                owner.say(&format!("Error: {}", explain(texts, &error)));
                log.info("error", &format!("{error:#}"));
                // Before writing a lost cable or a time out is mended by plugging in again.
                let mendable = !driver.run.writing
                    && matches!(
                        driver.run.step,
                        Step::Connect | Step::Backup | Step::Check | Step::Write
                    );
                if mendable && owner.again() {
                    driver.reconnect()?;
                    waiting_since = None;
                    continue;
                }
                return leave(&mut driver, owner);
            }
        };
        match next {
            Next::Go => {}
            Next::Connect => {
                if waiting_since.is_none() {
                    owner.say("Connect the mower in USB mode as the guide shows.");
                    for (key, photo) in mower.guide().steps {
                        owner.say(&format!("  {} ({photo})", text_of(texts, key)));
                    }
                    waiting_since = Some(Instant::now());
                }
                tokio::time::sleep(LOOK).await;
            }
            Next::Agree(warnings) => {
                owner.say("Before writing:");
                show(owner, texts, &warnings);
                if !owner.agree("Install MowBite OS anyway?") {
                    return leave(&mut driver, owner);
                }
                driver.agree()?;
            }
            Next::WriteOrBack => {
                let Some(back) = owner.back(driver.run.goal) else {
                    return leave(&mut driver, owner);
                };
                driver.choose(back)?;
                waiting_since = None;
            }
            Next::SwitchOffAndOn => owner.say(
                "Written and checked, but the mower did not restart by itself: unplug the cable, switch the mower \
                 off and on.",
            ),
            Next::Network => {
                owner.say("The mower starts MowBite OS. Looking for it on the network.");
                let since = tokio::time::Instant::now();
                let mut hinted = false;
                loop {
                    if driver.back_in_usb().await? {
                        owner.say("The mower is back in USB mode: its new system did not start.");
                        break;
                    }
                    let why = match first.confirm().await {
                        Ok(None) => {
                            driver.started()?;
                            break;
                        }
                        Ok(Some(why)) => why,
                        Err(error) => format!("{error:#}"),
                    };
                    log.info("first_start_wait", &why);
                    let waited = since.elapsed();
                    if !hinted && waited >= SETUP_HINT {
                        owner.say(&format!(
                            "Not found yet. Should the mower not reach your Wi-Fi, it opens its own: join it with \
                             this PC or a phone, name and key are in {}.",
                            root.join(crate::cli::SHEET).display()
                        ));
                        hinted = true;
                    }
                    if waited >= NETWORK_WAIT {
                        owner.say(if driver.run.left_usb {
                            "The mower was not confirmed from here: confirm its first start in the app. Should it \
                             come back in USB mode, start the installer again with this folder."
                        } else {
                            "The mower did not leave USB mode: unplug the cable, switch it off and on, then start \
                             the installer again with this folder."
                        });
                        return Ok(false);
                    }
                    tokio::time::sleep(LOOK).await;
                }
                if driver.run.step == Step::FirstStart {
                    // Back in USB mode: write again or go back.
                    let Some(back) = owner.back(Goal::Install) else {
                        return Ok(false);
                    };
                    driver.choose(back)?;
                    waiting_since = None;
                }
            }
            Next::Stopped(findings) => {
                owner.say("MowBite OS cannot be installed on this mower:");
                show(owner, texts, &findings);
                return leave(&mut driver, owner);
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
        agains: RefCell<Vec<bool>>,
    }

    impl Owner for Script {
        fn say(&self, text: &str) {
            self.said.borrow_mut().push(text.to_owned());
        }
        fn agree(&self, _: &str) -> bool {
            self.yes.get()
        }
        fn back(&self, _: Goal) -> Option<bool> {
            let mut backs = self.backs.borrow_mut();
            (!backs.is_empty()).then(|| backs.remove(0))
        }
        fn again(&self) -> bool {
            let mut agains = self.agains.borrow_mut();
            !agains.is_empty() && agains.remove(0)
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
        // Still listed right after the restart, then gone, then back after a failed start.
        *fake.seen.borrow_mut() = vec![true, false];
        *fake.usb_again.borrow_mut() = true;
        let owner = Script::default();
        owner.backs.borrow_mut().push(true);
        let not_yet = vec![Some("not found".to_string()); 5];
        assert!(go(&mut w, &fake, &owner, not_yet).await.unwrap());
        assert_eq!(fake.ran("back"), 1);
    }

    // Right after the restart the mower may still be listed: that is no failed start.
    #[tokio::test(start_paused = true)]
    async fn still_listed_right_after_the_restart() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.seen.borrow_mut() = vec![true, true];
        let owner = Script::default();
        assert!(
            go(
                &mut w,
                &fake,
                &owner,
                vec![Some("x".into()), Some("x".into())]
            )
            .await
            .unwrap()
        );
        assert_eq!(fake.ran("back"), 0);
    }

    // The trial period ends after 10 min and the mower comes back in USB mode: the frame sees it while it waits.
    #[tokio::test(start_paused = true)]
    async fn back_in_usb_while_waiting_for_the_network() {
        let mut w = World::new();
        let fake = Fake::default();
        let mut seen = vec![false; 300];
        seen.push(true);
        *fake.seen.borrow_mut() = seen;
        let owner = Script::default();
        owner.backs.borrow_mut().push(true);
        let never = vec![Some("not found".to_string()); 1000];
        assert!(go(&mut w, &fake, &owner, never).await.unwrap());
        assert_eq!(fake.ran("back"), 1);
        let said = owner.said.borrow().join("\n");
        assert!(said.contains("its own"), "{said}");
        assert!(said.contains(crate::cli::SHEET));
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_cable_before_writing_is_mended_by_plugging_in() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.write_error.borrow_mut() =
            Some(anyhow::anyhow!("USB timeout").context(crate::mower::NothingWritten));
        let owner = Script::default();
        owner.agains.borrow_mut().push(true);
        assert!(go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!(fake.ran("write"), 2);
        assert!(
            owner
                .said
                .borrow()
                .iter()
                .any(|s| s.contains("USB timeout"))
        );
    }

    // Cut off while writing: no retry, only "write again or back" at the next start, whatever goal it has.
    #[tokio::test(start_paused = true)]
    async fn a_half_written_device_is_never_called_untouched() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.write_error.borrow_mut() = Some(anyhow::anyhow!("cable off"));
        let owner = Script::default();
        owner.agains.borrow_mut().push(true);
        assert!(!go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!(fake.ran("write"), 1);
        assert!(
            owner
                .said
                .borrow()
                .iter()
                .any(|s| s.contains("not complete"))
        );
        assert!(
            !owner
                .said
                .borrow()
                .iter()
                .any(|s| s.contains("Nothing was written"))
        );
        // The owner starts "back" now: no question whether to touch an untouched mower, straight to the choice.
        owner.backs.borrow_mut().push(true);
        let first = Confirms(RefCell::new(vec![]));
        let back = run(
            &fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &w.inputs,
            Goal::Back,
            &first,
            &owner,
            &w.cancel,
        );
        assert!(back.await.unwrap());
        assert_eq!(fake.ran("back"), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn written_but_not_restarted() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.write_error.borrow_mut() =
            Some(anyhow::anyhow!("reset").context(crate::mower::WrittenNotRestarted));
        let owner = Script::default();
        assert!(go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert!(
            owner
                .said
                .borrow()
                .iter()
                .any(|s| s.contains("switch the mower off and on"))
        );
    }

    // Back from a folder that never wrote: the owner has to say yes.
    #[tokio::test(start_paused = true)]
    async fn back_on_an_untouched_mower_asks_first() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.findings.borrow_mut() = vec![Finding::Stop {
            code: "tron.table_unknown".into(),
            details: String::new(),
        }];
        let owner = Script::default();
        assert!(!go(&mut w, &fake, &owner, vec![]).await.unwrap());
        let first = Confirms(RefCell::new(vec![]));
        let back = run(
            &fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &w.inputs,
            Goal::Back,
            &first,
            &owner,
            &w.cancel,
        );
        assert!(!back.await.unwrap());
        assert_eq!(fake.ran("back"), 0);
        // A stop is checked again on the next install (newer tables may know the mower).
        fake.findings.borrow_mut().clear();
        assert!(go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!(fake.ran("check"), 2);
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
