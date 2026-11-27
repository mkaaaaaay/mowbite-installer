//! The text mode of the frame: runs a Driver to its end and asks the owner in the terminal. The web page uses the
//! same Driver; this mode serves where no browser is wanted and in the first runs on our own mower.

use crate::awake::Awake;
use crate::mower::{
    BaseImage, Cancel, Ctx, Finding, Inputs, Log, Mower, Progress, Tables, WorkDir,
};
use crate::run::{Driver, Goal, Next, Step};
use crate::secret::Secret;
use crate::store::Store;
use anyhow::Result;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// How often the frame looks for the mower in USB mode, and for its first start on the network.
const LOOK: Duration = Duration::from_secs(2);
/// Longer than the trial period of the mower (10 min after its start, then back into USB mode) with its boot.
const NETWORK_WAIT: Duration = Duration::from_secs(20 * 60);
/// After this the owner hears of the setup Wi-Fi, which the mower opens 3 min after a start without its Wi-Fi.
const SETUP_HINT: Duration = Duration::from_secs(4 * 60);

/// A yes-or-no question before something is written: the findings, the question and what each answer does.
pub struct Ask {
    pub findings: Vec<String>,
    pub text: String,
    pub yes: &'static str,
    pub no: &'static str,
}

/// The owner at the terminal or on the page; tests give their own answers. Every text comes in the language of the
/// installer.
pub trait Owner {
    fn say(&self, text: &str);
    /// The run is at this step now.
    fn at(&self, _step: Step) {}
    /// How to bring the mower into USB mode, while the frame waits for it.
    fn guide(&self, lines: &[String]) {
        for line in lines {
            self.say(&format!("  {line}"));
        }
    }
    /// What went wrong; again() or the end follows.
    fn error(&self, text: &str) {
        self.say(&format!("{}: {text}", word("frame.error")));
    }
    /// true only on an explicit yes.
    fn agree(&self, ask: &Ask) -> bool;
    /// Some(true) for "back", Some(false) for "write again", None when no answer can come.
    fn back(&self, goal: Goal) -> Option<bool>;
    /// true when the owner plugged the cable in again and wants to go on.
    fn again(&self) -> bool;
    /// The mower was not found on the network for a while: it may have opened its own Wi-Fi.
    fn setup_wifi(&self, _name: &str, _key: &Secret<String>, sheet: &Path) {
        self.say(&word("frame.setup_hint").replace("{sheet}", &sheet.display().to_string()));
    }
    /// The run ended, reached (ok) or not.
    fn end(&self, _ok: bool, text: &str) {
        self.say(text);
    }
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

    fn agree(&self, ask: &Ask) -> bool {
        for finding in &ask.findings {
            println!("  {finding}");
        }
        print!("{} {} ", ask.text, word("frame.yes_no"));
        let _ = std::io::stdout().flush();
        line().is_some_and(|l| l.eq_ignore_ascii_case("yes") || l.eq_ignore_ascii_case("ja"))
    }

    fn back(&self, goal: Goal) -> Option<bool> {
        loop {
            print!(
                "{} {} ",
                word(back_question(goal)),
                word("frame.write_or_back_keys")
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
        print!("{} ", word("frame.again_keys"));
        let _ = std::io::stdout().flush();
        line().is_some_and(|l| !l.eq_ignore_ascii_case("q"))
    }

    // The name always, the key only when the owner asks for it here (it would stay in the scrollback).
    fn setup_wifi(&self, name: &str, key: &Secret<String>, sheet: &Path) {
        self.say(&word("frame.setup_hint").replace("{sheet}", &sheet.display().to_string()));
        self.say(&format!("  {}: {name}", word("frame.wifi_name")));
        print!("{} ", word("frame.show_key"));
        let _ = std::io::stdout().flush();
        if line().is_some_and(|l| ["j", "y", "ja", "yes"].contains(&l.to_lowercase().as_str())) {
            self.say(&format!("  {}: {}", word("frame.wifi_key"), key.expose()));
        }
    }
}

/// The question after a run that did not end: write again or go back.
pub fn back_question(goal: Goal) -> &'static str {
    match goal {
        Goal::Install | Goal::Check => "frame.not_complete",
        Goal::Back => "frame.back_not_finished",
    }
}

/// Progress on stderr: the step with its text, then percent of the step, at most once a second.
pub struct Bar {
    last: std::cell::Cell<Option<Instant>>,
    texts: &'static [(&'static str, &'static str, &'static str)],
}

impl Bar {
    /// texts of the mower module; the frame's own come on top.
    pub fn new(texts: &'static [(&'static str, &'static str, &'static str)]) -> Self {
        Bar {
            last: std::cell::Cell::new(None),
            texts,
        }
    }
}

impl Progress for Bar {
    fn step(&self, code: &'static str) {
        let text = step_text(self.texts, code);
        if text == code {
            eprintln!("== {code}");
        } else {
            eprintln!("== {code}\n   {text}");
        }
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
        eprintln!("   {}%", done * 100 / total.max(1));
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

/// Texts in German when the system speaks it, else English: the locale variables, on Windows its display language.
pub fn german() -> bool {
    let set = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    match set {
        Some(v) => v.starts_with("de"),
        None => system_german(),
    }
}

#[cfg(windows)]
fn system_german() -> bool {
    // SAFETY: no arguments, returns a language id.
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    id & 0x3ff == 0x07
}

#[cfg(not(windows))]
fn system_german() -> bool {
    false
}

/// Steps of the frame: (key, de, en).
const STEPS: &[(&str, &str, &str)] = &[
    (
        "release.download",
        "Der Installer lädt die Dateien der neuesten Version von GitHub (etwa 220 MB) und prüft ihre Signaturen.",
        "The installer downloads the files of the newest release from GitHub (about 220 MB) and checks their \
         signatures.",
    ),
    (
        "release.check_base",
        "Der Installer prüft das Grundsystem, ein paar Minuten.",
        "The installer checks the base system, a few minutes.",
    ),
];

/// The texts of the frame: (key, de, en).
const FRAME: &[(&str, &str, &str)] = &[
    ("frame.error", "Fehler", "Error"),
    ("frame.yes_no", "[ja/nein]", "[yes/no]"),
    (
        "frame.nothing_written",
        "Nichts wurde geschrieben. Die Sicherung bleibt.",
        "Nothing was written. The backup stays.",
    ),
    (
        "frame.incomplete",
        "Das System auf dem Mäher ist nicht vollständig. Den Installer mit diesem Sicherungsordner neu starten: er \
         bietet an, erneut zu schreiben oder zurückzugehen.",
        "The system on the mower is not complete. Start the installer again with this backup folder: it offers to \
         write again or to go back.",
    ),
    (
        "frame.back_unwritten",
        "Mit diesem Sicherungsordner wurde nie auf den Mäher geschrieben: er läuft mit dem System, aus dem die \
         Sicherung stammt, oder mit einem anderswo geschriebenen. Die Sicherung trotzdem zurückschreiben?",
        "This backup folder never wrote to the mower: it runs the system the backup was made from, or one written \
         from elsewhere. Write the backup back anyway?",
    ),
    (
        "frame.answer_write_backup",
        "Sicherung zurückschreiben",
        "Write the backup back",
    ),
    (
        "frame.answer_write_nothing",
        "Nichts schreiben",
        "Write nothing",
    ),
    (
        "frame.awake_failed",
        "Diesen PC bis zum Ende wach halten, Deckel offen: der Installer konnte den Ruhezustand nicht sperren.",
        "Keep this PC awake until the end, lid open: it could not be kept from sleeping.",
    ),
    (
        "frame.awake_failed_ssh",
        "Über SSH gestartet: dort darf der Installer den Ruhezustand dieses PCs nicht sperren. Den PC bis zum Ende \
         wach halten oder den Installer direkt am PC starten.",
        "Started over SSH: there the installer may not keep this PC from sleeping. Keep the PC awake until the \
         end, or start the installer at the PC itself.",
    ),
    (
        "frame.stopped",
        "Vom Besitzer beendet.",
        "Stopped by the owner.",
    ),
    (
        "frame.connect",
        "Den Mäher wie in der Anleitung im USB-Modus anschließen.",
        "Connect the mower in USB mode as the guide shows.",
    ),
    (
        "frame.usb_driver",
        "Windows hat noch keinen USB-Treiber für den Mäher. Der Installer richtet den Treiber von Windows dafür ein \
         (WinUSB), Windows fragt nach der Erlaubnis.",
        "Windows has no USB driver for the mower yet. The installer sets up the Windows driver for it (WinUSB), \
         Windows asks for permission.",
    ),
    (
        "frame.usb_access",
        "Damit der Installer die Tron über USB erreicht, legt er eine Regel für Linux an. Dafür fragt das System \
         einmal nach deinem Passwort.",
        "So that the installer reaches the Tron over USB, it sets up a rule for Linux. The system asks once for your \
         password for it.",
    ),
    (
        "frame.usb_access_ask",
        "Die Regel jetzt anlegen?",
        "Set up the rule now?",
    ),
    (
        "frame.answer_usb_access",
        "Regel anlegen",
        "Set up the rule",
    ),
    (
        "frame.usb_driver_ask",
        "Den USB-Treiber jetzt einrichten?",
        "Set up the USB driver now?",
    ),
    (
        "frame.answer_usb_driver",
        "USB-Treiber einrichten",
        "Set up the USB driver",
    ),
    ("frame.answer_stop", "Beenden", "Stop"),
    (
        "frame.write_ask",
        "Schreiben ersetzt das Hersteller-System auf dem Mäher. Die Sicherung im Ordner bleibt, mit ihr geht es \
         zurück. MowBite OS schreiben?",
        "Writing replaces the vendor system on the mower. The backup in the folder stays, it is the way back. \
         Write MowBite OS?",
    ),
    (
        "frame.answer_write_os",
        "MowBite OS schreiben",
        "Write MowBite OS",
    ),
    (
        "frame.switch_off_on",
        "Geschrieben und geprüft, aber der Mäher startete nicht von selbst neu: das Kabel abziehen, den Mäher aus- \
         und wieder einschalten.",
        "Written and checked, but the mower did not restart by itself: unplug the cable, switch the mower off and \
         on.",
    ),
    (
        "frame.network",
        "Der Mäher startet MowBite OS. Der Installer sucht ihn im Netz.",
        "The mower starts MowBite OS. Looking for it on the network.",
    ),
    (
        "frame.back_in_usb",
        "Der Mäher ist wieder im USB-Modus: sein neues System startete nicht.",
        "The mower is back in USB mode: its new system did not start.",
    ),
    (
        "frame.setup_hint",
        "Noch nicht gefunden. Erreicht der Mäher dein WLAN nicht, öffnet er sein eigenes: mit diesem PC oder dem \
         Handy verbinden, Name und Schlüssel stehen in {sheet}.",
        "Not found yet. Should the mower not reach your Wi-Fi, it opens its own: join it with this PC or a phone, \
         name and key are in {sheet}.",
    ),
    ("frame.wifi_name", "WLAN-Name", "Wi-Fi name"),
    ("frame.wifi_key", "WLAN-Schlüssel", "Wi-Fi key"),
    (
        "frame.show_key",
        "Den Schlüssel hier anzeigen? [j/n]",
        "Show the key here? [y/n]",
    ),
    (
        "frame.not_confirmed",
        "Der Start des Mähers wurde von hier nicht bestätigt: den ersten Start in der App bestätigen. Kommt er \
         wieder im USB-Modus hoch, den Installer mit diesem Ordner neu starten.",
        "The mower was not confirmed from here: confirm its first start in the app. Should it come back in USB \
         mode, start the installer again with this folder.",
    ),
    (
        "frame.not_left_usb",
        "Der Mäher hat den USB-Modus nicht verlassen: das Kabel abziehen, ihn aus- und einschalten, dann den \
         Installer mit diesem Ordner neu starten.",
        "The mower did not leave USB mode: unplug the cable, switch it off and on, then start the installer again \
         with this folder.",
    ),
    (
        "frame.cannot_install",
        "MowBite OS lässt sich auf diesem Mäher nicht installieren:",
        "MowBite OS cannot be installed on this mower:",
    ),
    (
        "frame.checked",
        "Sicherung und Prüfungen sind fertig, auf den Mäher wurde nichts geschrieben.",
        "Backup and checks are done, nothing was written to the mower.",
    ),
    (
        "frame.checked_next",
        "Zum Installieren den Installer mit install und diesem Sicherungsordner ({folder}) starten: er macht mit \
         dieser Sicherung weiter.",
        "To install MowBite OS, start the installer with install and this backup folder ({folder}): it goes on \
         from this backup.",
    ),
    (
        "frame.done_install",
        "Fertig: MowBite OS läuft auf dem Mäher. Weiter geht es in MowBite, hier im Browser oder in der App auf dem \
         Handy.",
        "Done: MowBite OS runs on the mower. Go on in MowBite, here in the browser or in the app on your phone.",
    ),
    (
        "frame.done_back",
        "Fertig: das Hersteller-System ist zurück auf dem Mäher.",
        "Done: the vendor system is back on the mower.",
    ),
    (
        "frame.not_complete",
        "Das System auf dem Mäher ist nicht vollständig oder startete nicht.",
        "The system on the mower is not complete or did not start.",
    ),
    (
        "frame.back_not_finished",
        "Das Zurückschreiben des Hersteller-Systems wurde nicht fertig.",
        "Writing the vendor system back did not finish.",
    ),
    (
        "frame.write_or_back_keys",
        "MowBite OS neu schreiben (w) oder zurück zum Hersteller-System (b)?",
        "Write MowBite OS again (w) or go back to the vendor system (b)?",
    ),
    (
        "frame.answer_write_again",
        "MowBite OS neu schreiben",
        "Write MowBite OS again",
    ),
    (
        "frame.answer_back",
        "Hersteller-System zurückschreiben",
        "Write the vendor system back",
    ),
    (
        "frame.hint_back",
        "Der Mäher läuft danach wie vorher. Die Sicherung bleibt.",
        "The mower runs as before then. The backup stays.",
    ),
    (
        "frame.hint_write_again",
        "Noch ein Versuch mit derselben Version. Die Sicherung bleibt.",
        "One more try with the same release. The backup stays.",
    ),
    (
        "frame.again_keys",
        "Das Kabel wieder einstecken und Enter drücken, oder q und Enter zum Beenden:",
        "Plug the cable in again and press Enter to go on, or q and Enter to stop:",
    ),
    (
        "frame.again",
        "Das Kabel wieder einstecken, dann geht es an derselben Stelle weiter.",
        "Plug the cable in again, then it goes on where it stopped.",
    ),
    (
        "frame.answer_again",
        "Kabel steckt, weiter",
        "Cable is in, go on",
    ),
    (
        "frame.keep_backup",
        "Die Sicherung in {folder} enthält den ganzen Speicher des Mähers mit seinen Schlüsseln und dem WLAN: für \
         dich behalten, nicht weitergeben.",
        "The backup in {folder} holds the whole flash of the mower with its keys and Wi-Fi: keep it to yourself, \
         do not share it.",
    ),
    (
        "frame.mower_name",
        "Der Mäher heißt {name}. Findet er dein WLAN nicht, öffnet er sein eigenes; Name und Schlüssel stehen in \
         {sheet}.",
        "The mower will be called {name}. Should it not find your Wi-Fi, it opens its own; its name and key are in \
         {sheet}.",
    ),
    (
        "frame.page",
        "Der Installer läuft im Browser: {url} . Dieses Fenster offen lassen, es zeigt dasselbe Protokoll.",
        "The installer runs in the browser: {url} . Keep this window open, it shows the same log.",
    ),
    (
        "frame.store_missing",
        "Bitte einen Sicherungsordner angeben.",
        "Please give a backup folder.",
    ),
    (
        "frame.store_foreign",
        "In diesem Ordner liegen schon andere Dateien. Bitte einen neuen oder leeren Ordner angeben: der Installer \
         macht den Sicherungsordner nur für dich lesbar.",
        "This folder holds other files already. Please give a new or empty folder: the installer makes the backup \
         folder readable for you only.",
    ),
    (
        "frame.space_low",
        "Auf dem Laufwerk von {folder} sind {free} GB frei, gebraucht werden {need} GB. Bitte einen Ordner auf einem \
         Laufwerk mit mehr Platz wählen, zum Beispiel einer externen Festplatte.",
        "The drive of {folder} has {free} GB free, {need} GB are needed. Please choose a folder on a drive with more \
         room, for example an external disk.",
    ),
    (
        "frame.backup_size",
        "Die Sicherung ist fertig und belegt {gb} GB.",
        "The backup is done and takes {gb} GB.",
    ),
    (
        "frame.pick_store",
        "Sicherungsordner wählen",
        "Choose the backup folder",
    ),
    (
        "frame.wifi_missing",
        "Bitte den Namen deines WLANs angeben.",
        "Please give the name of your Wi-Fi.",
    ),
    ("frame.stop", "Stopp", "Stop"),
    ("frame.warn", "Achtung", "Warning"),
    ("frame.note", "Hinweis", "Note"),
];

/// The text of a step of the frame or of the mower module.
pub fn step_text<'a>(texts: &[(&str, &'a str, &'a str)], code: &'a str) -> &'a str {
    text_of(STEPS, text_of(texts, code))
}

/// A text of the frame in the language of the installer.
pub fn word(code: &str) -> &str {
    text_of(FRAME, code)
}

/// The text of a code, or the code when the module has none.
pub fn text_of<'a>(texts: &[(&str, &'a str, &'a str)], code: &'a str) -> &'a str {
    texts
        .iter()
        .find(|(key, _, _)| *key == code)
        .map_or(code, |&(_, de, en)| if german() { de } else { en })
}

/// An error with a code in its chain ("tron.no_serial: ...") with the text of the code from there on.
fn explain(texts: &[(&str, &str, &str)], error: &anyhow::Error) -> String {
    let causes: Vec<String> = error.chain().map(ToString::to_string).collect();
    for (i, cause) in causes.iter().enumerate() {
        if let Some((code, rest)) = cause.split_once(": ")
            && texts.iter().any(|(key, _, _)| *key == code)
        {
            let rest = std::iter::once(rest)
                .chain(causes[i + 1..].iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(": ");
            return format!("{} ({rest})", text_of(texts, code));
        }
    }
    causes.join(": ")
}

/// The findings as lines: kind, text, details.
fn lines(texts: &[(&str, &str, &str)], findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .map(|finding| {
            let (kind, code, details) = match finding {
                Finding::Stop { code, details } => ("frame.stop", code, details),
                Finding::Warn { code, details } => ("frame.warn", code, details),
                Finding::Info { code, details } => ("frame.note", code, details),
            };
            let text = text_of(texts, code);
            if details.is_empty() {
                format!("{}: {text}", word(kind))
            } else {
                format!("{}: {text} ({details})", word(kind))
            }
        })
        .collect()
}

fn show(owner: &dyn Owner, texts: &[(&str, &str, &str)], findings: &[Finding]) {
    for line in lines(texts, findings) {
        owner.say(&format!("  {line}"));
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
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<bool> {
    let root = store.root().to_path_buf();
    let texts = mower.texts();
    let mut driver = Driver::open(mower, &root, goal)?;
    if goal == Goal::Back
        && !driver.run.wrote
        && !driver.run.writing
        && !owner.agree(&Ask {
            findings: vec![],
            text: word("frame.back_unwritten").to_owned(),
            yes: word("frame.answer_write_backup"),
            no: word("frame.answer_write_nothing"),
        })
    {
        owner.end(false, word("frame.nothing_written"));
        return Ok(false);
    }
    let _awake = match Awake::hold("MowBite installer reads or writes the mower") {
        Ok(awake) => Some(awake),
        Err(_) => {
            // Over SSH polkit refuses the lock without a password: say why.
            let ssh = std::env::var_os("SSH_CONNECTION").is_some();
            owner.say(word(if ssh {
                "frame.awake_failed_ssh"
            } else {
                "frame.awake_failed"
            }));
            None
        }
    };
    let log = FileLog::new(store);
    let mut waiting_since: Option<Instant> = None;
    let leave = |driver: &mut Driver<'_, M>, owner: &dyn Owner| -> Result<bool> {
        let untouched = driver.leave()?;
        owner.end(
            false,
            word(if untouched {
                "frame.nothing_written"
            } else {
                "frame.incomplete"
            }),
        );
        Ok(false)
    };
    loop {
        if cancel.is_set() && !driver.run.writing {
            owner.say(word("frame.stopped"));
            return leave(&mut driver, owner);
        }
        let was = driver.run.step;
        let result = {
            let mut ctx = Ctx {
                store,
                work,
                tables,
                progress,
                cancel,
                log: &log,
                system_written: driver.run.wrote || driver.run.writing,
            };
            owner.at(driver.run.step);
            driver.step(&mut ctx, base, inputs).await
        };
        if was == Step::Backup && driver.run.step != Step::Backup {
            let gb = crate::space::used_bytes(store.root()) as f64 / 1e9;
            owner.say(&word("frame.backup_size").replace("{gb}", &format!("{gb:.1}")));
        }
        let next = match result {
            Ok(next) => next,
            Err(error) => {
                owner.error(&explain(texts, &error));
                log.info("error", &format!("{error:#}"));
                if driver.mendable() && owner.again() {
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
                    owner.say(word("frame.connect"));
                    let guide: Vec<String> = mower
                        .guide()
                        .steps
                        .iter()
                        .map(|(key, photo)| match *photo {
                            "" => text_of(texts, key).to_owned(),
                            p => format!("{} ({p})", text_of(texts, key)),
                        })
                        .collect();
                    owner.guide(&guide);
                    waiting_since = Some(Instant::now());
                }
                tokio::time::sleep(LOOK).await;
            }
            Next::UsbDriver => {
                log.info("usb_driver", "asking to set up USB access");
                let [what, ask, yes] = if cfg!(windows) {
                    [
                        "frame.usb_driver",
                        "frame.usb_driver_ask",
                        "frame.answer_usb_driver",
                    ]
                } else {
                    [
                        "frame.usb_access",
                        "frame.usb_access_ask",
                        "frame.answer_usb_access",
                    ]
                };
                if !owner.agree(&Ask {
                    findings: vec![word(what).to_owned()],
                    text: word(ask).to_owned(),
                    yes: word(yes),
                    no: word("frame.answer_stop"),
                }) {
                    return leave(&mut driver, owner);
                }
                if let Err(error) = mower.install_usb_driver().await {
                    owner.error(&explain(texts, &error));
                    log.info("error", &format!("{error:#}"));
                    return leave(&mut driver, owner);
                }
                log.info("usb_driver", "installed");
            }
            Next::Agree(warnings) => {
                if !owner.agree(&Ask {
                    findings: lines(texts, &warnings),
                    text: word("frame.write_ask").to_owned(),
                    yes: word("frame.answer_write_os"),
                    no: word("frame.answer_write_nothing"),
                }) {
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
            Next::SwitchOffAndOn => owner.say(word("frame.switch_off_on")),
            Next::Network => {
                owner.say(word("frame.network"));
                let since = tokio::time::Instant::now();
                let mut hinted = false;
                loop {
                    if driver.back_in_usb().await? {
                        owner.say(word("frame.back_in_usb"));
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
                        owner.setup_wifi(
                            &inputs.device_name,
                            &inputs.setup_wifi_key,
                            &root.join(crate::cli::SHEET),
                        );
                        hinted = true;
                    }
                    if waited >= NETWORK_WAIT {
                        owner.end(
                            false,
                            word(if driver.run.left_usb {
                                "frame.not_confirmed"
                            } else {
                                "frame.not_left_usb"
                            }),
                        );
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
                owner.say(word("frame.cannot_install"));
                show(owner, texts, &findings);
                return leave(&mut driver, owner);
            }
            Next::Checked(findings) => {
                owner.say(word("frame.checked"));
                show(owner, texts, &findings);
                owner.end(
                    true,
                    &word("frame.checked_next").replace("{folder}", &root.display().to_string()),
                );
                return Ok(true);
            }
            Next::Done => {
                owner.end(
                    true,
                    word(match driver.run.goal {
                        Goal::Install | Goal::Check => "frame.done_install",
                        Goal::Back => "frame.done_back",
                    }),
                );
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

    #[test]
    fn a_code_under_nothing_written_still_gets_its_text() {
        let texts = [("tron.table_changed", "Tabelle", "Table")];
        let error = anyhow::anyhow!("tron.table_changed: rootfs moved")
            .context(crate::mower::NothingWritten);
        let shown = explain(&texts, &error);
        assert!(
            shown.ends_with(" (rootfs moved)") && !shown.contains("tron."),
            "{shown}"
        );
        let plain = anyhow::anyhow!("USB timeout").context("reading");
        assert_eq!(explain(&texts, &plain), "reading: USB timeout");
    }

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
        fn agree(&self, ask: &Ask) -> bool {
            self.said.borrow_mut().extend(ask.findings.iter().cloned());
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
            &Bar::new(&[]),
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
    async fn check_saves_and_checks_without_writing_and_install_goes_on_from_its_backup() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.findings.borrow_mut() = vec![Finding::Warn {
            code: "tron.vendor_unknown".into(),
            details: "1.3.40".into(),
        }];
        let owner = Script::default();
        let first = Confirms(RefCell::new(vec![]));
        let checked = run(
            &fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &w.inputs,
            Goal::Check,
            &first,
            &owner,
            &Bar::new(&[]),
            &w.cancel,
        )
        .await
        .unwrap();
        assert!(checked);
        assert_eq!((fake.ran("backup"), fake.ran("check")), (1, 1));
        assert_eq!((fake.ran("prepare"), fake.ran("write")), (0, 0));
        assert!(
            owner
                .said
                .borrow()
                .iter()
                .any(|s| s.contains(word("frame.checked")))
        );
        owner.yes.set(true);
        assert!(go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!((fake.ran("backup"), fake.ran("check")), (1, 2));
        assert_eq!(fake.ran("write"), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_missing_usb_driver_is_set_up_after_a_yes() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.no_driver.borrow_mut() = true;
        let owner = Script::default();
        assert!(!go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!((fake.ran("install_usb_driver"), fake.ran("open")), (0, 0));
        owner.yes.set(true);
        assert!(go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!(fake.ran("install_usb_driver"), 1);
        assert_eq!(fake.ran("write"), 1);
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
        assert!(said.contains(&word("frame.setup_hint")[..20]), "{said}");
        assert!(said.contains(crate::cli::SHEET));
        let size = word("frame.backup_size").split('{').next().unwrap();
        assert_eq!(said.matches(size).count(), 1, "{said}");
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
                .any(|s| s.contains(word("frame.incomplete")))
        );
        assert!(
            !owner
                .said
                .borrow()
                .iter()
                .any(|s| s.contains(word("frame.nothing_written")))
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
            &Bar::new(&[]),
            &w.cancel,
        )
        .await;
        assert!(back.unwrap());
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
                .any(|s| s.contains(word("frame.switch_off_on")))
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
            &Bar::new(&[]),
            &w.cancel,
        )
        .await;
        assert!(!back.unwrap());
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
