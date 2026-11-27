//! The text mode of the frame: runs a Driver to its end and asks the owner in the terminal. The web page uses the
//! same Driver; this mode serves where no browser is wanted and in the first runs on our own mower.

use crate::awake::Awake;
use crate::mower::{
    BaseImage, Cancel, Ctx, Finding, Inputs, Log, Mower, Progress, Tables, Wifi, WorkDir,
};
use crate::run::{Driver, Goal, Next, Step};
use crate::secret::Secret;
use crate::store::Store;
use anyhow::Result;
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, Instant};

/// How often the frame looks for the mower in USB mode, and for its first start on the network.
const LOOK: Duration = Duration::from_secs(2);
/// Longer than the trial period of the mower (10 min after its start, then back into USB mode) with its boot.
const NETWORK_WAIT: Duration = Duration::from_secs(20 * 60);
/// A mower in its Wi-Fi answers within a minute of its restart (measured on a Tron). After this the owner hears what
/// to check, and of the setup Wi-Fi the mower opens 3 min after a start without its Wi-Fi.
const NOT_FOUND_HINT: Duration = Duration::from_secs(2 * 60);
/// The page of the mower on its setup Wi-Fi.
const SETUP_URL: &str = "http://10.42.0.1/";

/// A yes-or-no question before something is written: the findings, the question and what each answer does.
pub struct Ask {
    pub findings: Vec<String>,
    pub text: String,
    pub yes: &'static str,
    pub no: &'static str,
}

/// What the owner chose after a write that did not end or a start that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Back {
    Vendor,
    Again,
    /// Write again with another Wi-Fi for the mower.
    OtherWifi,
}

/// What the frame asks the owner beyond the answers of a question.
pub trait Typed {
    /// Another Wi-Fi for the mower, after a failed start.
    fn wifi(&mut self) -> Result<Wifi>;
    /// What an install needs beside a check (the Wi-Fi, name and keys of the mower), into inputs.
    fn for_install(&mut self, inputs: &mut Inputs) -> Result<()>;
}

/// What the owner needs while the mower is not found on the network.
pub struct NotFound<'a> {
    /// The Wi-Fi the mower was told to join.
    pub wifi: &'a str,
    /// Name of the mower on the network and of its setup Wi-Fi.
    pub name: &'a str,
    pub setup_key: &'a Secret<String>,
    pub sheet: &'a Path,
}

/// What the owner did on their own while the frame looks for the mower.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Help {
    /// The address of the mower, read off the router.
    Address(IpAddr),
    /// Confirmed the start in the app.
    Confirmed,
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
    /// why: what did not end or start. None when no answer can come; other_wifi: writing again with another Wi-Fi
    /// is on offer.
    fn back(&self, why: &str, other_wifi: bool) -> Option<Back>;
    /// true when the owner plugged the cable in again and wants to go on.
    fn again(&self) -> bool;
    /// What the run waits for now.
    fn waits(&self, text: &str) {
        self.say(text);
    }
    /// Some: the mower was not found on the network for a while, what to check and its setup Wi-Fi. None: it answers.
    fn not_found(&self, help: Option<&NotFound>) {
        if let Some(help) = help {
            for line in not_found_lines(help) {
                self.say(&line);
            }
        }
    }
    /// Asked while the frame looks for the mower.
    fn helped(&self) -> Option<Help> {
        None
    }
    /// The run goes on with another goal (install after a check).
    fn goal(&self, _goal: Goal) {}
    /// Where the app of the mower is, once it was found.
    fn app(&self, _link: &str) {}
    /// The run ended, reached (ok) or not; more: what is left to do by hand.
    fn end(&self, _ok: bool, text: &str, more: &[String]) {
        self.say(text);
        for line in more {
            self.say(line);
        }
    }
}

/// What to check while the mower is not found, in the words of the frame.
pub fn not_found_lines(help: &NotFound) -> Vec<String> {
    let fill = |code: &str| {
        word(code)
            .replace("{wifi}", help.wifi)
            .replace("{name}", help.name)
            .replace("{sheet}", &help.sheet.display().to_string())
            .replace("{setup}", SETUP_URL)
    };
    [
        "frame.not_found",
        "frame.not_found_same_net",
        "frame.not_found_phone",
        "frame.not_found_setup",
        "frame.not_found_falls_back",
    ]
    .iter()
    .map(|code| fill(code))
    .collect()
}

/// The network part of the first start: find the mower and confirm its trial period (host.confirm_start).
#[allow(async_fn_in_trait)]
pub trait FirstStart {
    /// Ok(None) once confirmed, Ok(Some(why)) while not yet.
    async fn confirm(&self) -> Result<Option<String>>;
    /// Where the mower answered the last confirm(); None when it did not.
    fn reached(&self) -> Option<IpAddr> {
        None
    }
    /// An address the owner read off the router, asked beside what the search finds.
    fn also_at(&self, _addr: IpAddr) {}
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

    fn back(&self, why: &str, other_wifi: bool) -> Option<Back> {
        loop {
            print!(
                "{why} {} ",
                word(if other_wifi {
                    "frame.write_wifi_or_back_keys"
                } else {
                    "frame.write_or_back_keys"
                })
            );
            let _ = std::io::stdout().flush();
            match line()?.as_str() {
                "w" => return Some(Back::Again),
                "b" => return Some(Back::Vendor),
                "n" if other_wifi => return Some(Back::OtherWifi),
                _ => {}
            }
        }
    }

    fn again(&self) -> bool {
        print!("{} ", word("frame.again_keys"));
        let _ = std::io::stdout().flush();
        line().is_some_and(|l| !l.eq_ignore_ascii_case("q"))
    }

    // The name always, the key only from the sheet (it would stay in the scrollback).
    fn not_found(&self, help: Option<&NotFound>) {
        let Some(help) = help else { return };
        for line in not_found_lines(help) {
            self.say(&line);
        }
        self.say(&format!("  {}: {}", word("frame.wifi_name"), help.name));
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
        "release.kept",
        "Keine Verbindung zu GitHub. Der Installer nimmt die Dateien, die er früher geladen und geprüft hat.",
        "No connection to GitHub. The installer takes the files it downloaded and checked before.",
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
        "frame.awake_lid",
        "Den Deckel dieses PCs bis zum Ende offen lassen: zugeklappt schläft er ein, auch wenn der Installer ihn \
         sonst wach hält.",
        "Keep the lid of this PC open until the end: closed it goes to sleep, although the installer keeps it awake \
         otherwise.",
    ),
    (
        "frame.other_mower",
        "Das ist ein anderer Mäher als der, dessen Sicherung in diesem Ordner liegt. Den richtigen Mäher anschließen, \
         oder für diesen Mäher den Installer mit einem neuen, leeren Sicherungsordner starten.",
        "This is another mower than the one whose backup is in this folder. Connect the right mower, or start the \
         installer with a new, empty backup folder for this one.",
    ),
    (
        "frame.no_backup",
        "In diesem Ordner liegt keine Sicherung dieses Mähers. Den Ordner wählen, in den der Installer ihn gesichert \
         hat.",
        "This folder holds no backup of this mower. Choose the folder the installer backed it up into.",
    ),
    (
        "frame.other_kind",
        "Die Sicherung in diesem Ordner gehört zu einem anderen Mähertyp.",
        "The backup in this folder belongs to another kind of mower.",
    ),
    (
        "frame.half_written_check",
        "Das System auf dem Mäher ist nicht vollständig. Mit diesem Sicherungsordner installieren oder das \
         Hersteller-System zurückschreiben.",
        "The system on the mower is not complete. Install with this backup folder or write the vendor system back.",
    ),
    (
        "frame.download_failed",
        "Die Dateien der Version ließen sich nicht von GitHub laden. Die Internetverbindung prüfen und den Installer \
         erneut starten.",
        "The files of the release could not be downloaded from GitHub. Check the internet connection and start the \
         installer again.",
    ),
    (
        "frame.ask_wifi_name",
        "Name deines WLANs:",
        "Name of your Wi-Fi:",
    ),
    (
        "frame.ask_wifi_key",
        "WLAN-Schlüssel (wird nicht angezeigt):",
        "Wi-Fi key (not shown):",
    ),
    (
        "frame.ask_wifi_hidden",
        "Ist das WLAN versteckt (sendet seinen Namen nicht aus)? [j/N]",
        "Is the Wi-Fi hidden (it does not send its name)? [y/N]",
    ),
    (
        "frame.install_now",
        "Jetzt gleich MowBite OS installieren? Der Mäher bleibt dafür angeschlossen, wie er ist. Vor dem Schreiben \
         wird noch einmal gefragt.",
        "Install MowBite OS right now? The mower stays connected as it is. You are asked once more before the write.",
    ),
    (
        "frame.answer_install_now",
        "Jetzt installieren",
        "Install now",
    ),
    ("frame.answer_later", "Später", "Later"),
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
        "Der Mäher startet MowBite OS, meist dauert das etwa eine Minute. Der Installer sucht ihn im Netz.",
        "The mower starts MowBite OS, mostly within a minute. Looking for it on the network.",
    ),
    (
        "frame.network_busy",
        "Gefunden unter {addr}. Der Mäher richtet MowBite OS, die App und den Planer ein, das dauert ein paar Minuten.",
        "Found at {addr}. The mower sets up MowBite OS, the app and the planner, this takes a few minutes.",
    ),
    (
        "frame.network_not_ok",
        "Gefunden unter {addr}, sein Start ist aber noch nicht in Ordnung ({why}). Der Installer fragt weiter.",
        "Found at {addr}, but its start is not good yet ({why}). The installer keeps asking.",
    ),
    (
        "frame.back_in_usb",
        "Der Mäher ist zurück im USB-Modus: sein Start wurde nicht bestätigt, weil der Installer ihn im Netz nicht \
         erreicht hat. Meist stimmt der Name oder der Schlüssel des WLANs nicht, oder dieser PC ist in einem anderen \
         Netz als der Mäher.",
        "The mower is back in USB mode: its start was not confirmed because the installer did not reach it on the \
         network. Mostly the name or the key of the Wi-Fi is wrong, or this PC is on another network than the mower.",
    ),
    (
        "frame.back_in_usb_reached",
        "Der Mäher ist zurück im USB-Modus: sein neues System startete nicht richtig ({why}).",
        "The mower is back in USB mode: its new system did not start well ({why}).",
    ),
    (
        "frame.not_found",
        "Noch nicht gefunden. Das kannst du prüfen:",
        "Not found yet. What you can check:",
    ),
    (
        "frame.not_found_same_net",
        "Ist dieser PC im selben Netz wie das WLAN „{wifi}“? Aus einem Gast-WLAN oder einem anderen Netz findet der \
         Installer den Mäher nicht.",
        "Is this PC on the same network as the Wi-Fi \"{wifi}\"? From a guest Wi-Fi or another network the installer \
         does not find the mower.",
    ),
    (
        "frame.not_found_phone",
        "Auf einem Handy in diesem WLAN http://{name}.local/ öffnen. Erscheint dort MowBite, in der App „Start \
         bestätigen“ drücken.",
        "Open http://{name}.local/ on a phone on that Wi-Fi. If MowBite shows up there, press \"Confirm the start\" \
         in the app.",
    ),
    (
        "frame.not_found_setup",
        "Erreicht der Mäher das WLAN nicht (Name oder Schlüssel falsch), öffnet er etwa 3 Minuten nach seinem Start \
         sein eigenes WLAN „{name}“; der Schlüssel steht in {sheet}. Diesen PC damit verbinden, dann findet der \
         Installer ihn. Hat der PC kein WLAN: ein Handy damit verbinden, {setup} öffnen und dort „Start bestätigen“ \
         drücken.",
        "Should the mower not reach the Wi-Fi (wrong name or key), it opens its own Wi-Fi \"{name}\" about 3 minutes \
         after its start; the key is in {sheet}. Join it with this PC, then the installer finds the mower. If the PC \
         has no Wi-Fi: join it with a phone, open {setup} and press \"Confirm the start\" there.",
    ),
    (
        "frame.not_found_falls_back",
        "Ohne Bestätigung geht der Mäher etwa 10 Minuten nach seinem Start von selbst zurück in den USB-Modus. Das \
         Kabel stecken lassen: der Installer bietet dann an, mit einem anderen WLAN neu zu schreiben oder das \
         Hersteller-System zurückzuschreiben.",
        "Without a confirmed start the mower goes back into USB mode by itself about 10 minutes after its start. \
         Leave the cable in: the installer then offers to write again with another Wi-Fi or to write the vendor \
         system back.",
    ),
    ("frame.wifi_name", "WLAN-Name", "Wi-Fi name"),
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
        "Zum Installieren den Installer erneut starten und denselben Sicherungsordner ({folder}) wählen: er macht mit \
         dieser Sicherung weiter.",
        "To install MowBite OS, start the installer again and choose the same backup folder ({folder}): it goes on \
         from this backup.",
    ),
    (
        "frame.done_install",
        "Fertig: MowBite OS läuft auf dem Mäher. Weiter geht es in MowBite, hier im Browser oder in der App auf dem \
         Handy.",
        "Done: MowBite OS runs on the mower. Go on in MowBite, here in the browser or in the app on your phone.",
    ),
    (
        "frame.done_install_setup",
        "Fertig: MowBite OS läuft auf dem Mäher. Er ist aber noch nicht in deinem WLAN: in seinem eigenen WLAN \
         {setup} öffnen und in MowBite unter Einstellungen › Der Mäher selbst › WLAN dein WLAN wählen.",
        "Done: MowBite OS runs on the mower. It is not on your Wi-Fi yet: on its own Wi-Fi open {setup} and choose \
         your Wi-Fi in MowBite under Settings › The mower itself › WLAN.",
    ),
    (
        "frame.app_at",
        "Adresse im Heimnetz: http://{addr}/ oder http://{name}.local/",
        "Address on your network: http://{addr}/ or http://{name}.local/",
    ),
    (
        "frame.app_at_name",
        "Adresse im Heimnetz: http://{name}.local/",
        "Address on your network: http://{name}.local/",
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
        "frame.write_wifi_or_back_keys",
        "MowBite OS neu schreiben (w), mit anderem WLAN neu schreiben (n) oder zurück zum Hersteller-System (b)?",
        "Write MowBite OS again (w), write it again with another Wi-Fi (n) or go back to the vendor system (b)?",
    ),
    (
        "frame.answer_other_wifi",
        "Mit anderem WLAN neu schreiben",
        "Write again with another Wi-Fi",
    ),
    (
        "frame.hint_other_wifi",
        "Bei einem Tippfehler im WLAN oder für ein anderes Netz. Die Sicherung bleibt.",
        "After a typo in the Wi-Fi or for another network. The backup stays.",
    ),
    (
        "frame.wifi_again",
        "Name und Schlüssel des WLANs, in das der Mäher soll. Dieser PC muss im selben Netz sein.",
        "Name and key of the Wi-Fi the mower is to join. This PC has to be on the same network.",
    ),
    (
        "frame.answer_use_wifi",
        "Mit diesem WLAN schreiben",
        "Write with this Wi-Fi",
    ),
    (
        "frame.checks_passed",
        "Die Sicherung ist fertig und geprüft. Die Prüfungen haben nichts gefunden, was gegen MowBite OS auf diesem \
         Mäher spricht.",
        "The backup is done and checked. The checks found nothing against MowBite OS on this mower.",
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

/// An error with a code in its chain ("tron.no_serial: ..." or the code alone) with the text of the code from there
/// on; codes of the mower module and of the frame.
pub fn explain(texts: &[(&str, &str, &str)], error: &anyhow::Error) -> String {
    let causes: Vec<String> = error.chain().map(ToString::to_string).collect();
    for (i, cause) in causes.iter().enumerate() {
        let (code, rest) = cause.split_once(": ").unwrap_or((cause, ""));
        if !texts.iter().chain(FRAME).any(|(key, _, _)| *key == code) {
            continue;
        }
        let text = match text_of(texts, code) {
            same if same == code => word(code),
            text => text,
        };
        let rest = std::iter::once(rest)
            .chain(causes[i + 1..].iter().map(String::as_str))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(": ");
        return if rest.is_empty() {
            text.to_owned()
        } else {
            format!("{text} ({rest})")
        };
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
    inputs: &mut Inputs,
    goal: Goal,
    first: &F,
    owner: &dyn Owner,
    progress: &dyn Progress,
    cancel: &Cancel,
    typed: &mut dyn Typed,
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
        owner.end(false, word("frame.nothing_written"), &[]);
        return Ok(false);
    }
    let _awake = match Awake::hold("MowBite installer reads or writes the mower") {
        Ok(awake) => {
            if !awake.lid {
                owner.say(word("frame.awake_lid"));
            }
            Some(awake)
        }
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
    // Where the mower answered its first start.
    let mut found: Option<IpAddr> = None;
    let leave = |driver: &mut Driver<'_, M>, owner: &dyn Owner| -> Result<bool> {
        let untouched = driver.leave()?;
        owner.end(
            false,
            word(if untouched {
                "frame.nothing_written"
            } else {
                "frame.incomplete"
            }),
            &[],
        );
        Ok(false)
    };
    loop {
        // Between two steps nothing is under way: leave() tells whether the system on the mower is whole.
        if cancel.is_set() {
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
                log.info("error", &format!("{error:#}"));
                if cancel.is_set() {
                    owner.say(word("frame.stopped"));
                    return leave(&mut driver, owner);
                }
                owner.error(&explain(texts, &error));
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
                    owner.waits(word("frame.connect"));
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
            Next::Agree(findings) => {
                let mut shown = lines(texts, &findings);
                if !findings.iter().any(|f| matches!(f, Finding::Warn { .. })) {
                    shown.insert(0, word("frame.checks_passed").to_owned());
                }
                if !owner.agree(&Ask {
                    findings: shown,
                    text: word("frame.write_ask").to_owned(),
                    yes: word("frame.answer_write_os"),
                    no: word("frame.answer_write_nothing"),
                }) {
                    return leave(&mut driver, owner);
                }
                driver.agree()?;
            }
            Next::WriteOrBack => {
                let goal = driver.run.goal;
                let chosen = owner.back(word(back_question(goal)), goal == Goal::Install);
                if !match chosen {
                    Some(back) => choose(&mut driver, inputs, back, typed)?,
                    None => false,
                } {
                    return leave(&mut driver, owner);
                }
                waiting_since = None;
            }
            Next::SwitchOffAndOn => owner.say(word("frame.switch_off_on")),
            Next::Network => {
                let mut shown = word("frame.network").to_owned();
                owner.waits(&shown);
                let since = tokio::time::Instant::now();
                let mut hinted = false;
                // The last refusal of a mower that answered.
                let mut refused: Option<String> = None;
                let confirmed = loop {
                    if driver.back_in_usb().await? {
                        break false;
                    }
                    match owner.helped() {
                        Some(Help::Address(addr)) => {
                            log.info("first_start_address", &addr.to_string());
                            first.also_at(addr);
                        }
                        Some(Help::Confirmed) => {
                            log.info("first_start", "confirmed in the app, says the owner");
                            break true;
                        }
                        None => {}
                    }
                    let why = match first.confirm().await {
                        Ok(None) => {
                            found = first.reached();
                            break true;
                        }
                        Ok(Some(why)) => why,
                        Err(error) => format!("{error:#}"),
                    };
                    log.info("first_start_wait", &why);
                    let text = match first.reached() {
                        None => word("frame.network").to_owned(),
                        Some(addr) => {
                            let code = if why.starts_with("BUSY") {
                                "frame.network_busy"
                            } else {
                                "frame.network_not_ok"
                            };
                            refused = Some(why.clone());
                            word(code)
                                .replace("{addr}", &addr.to_string())
                                .replace("{why}", &why)
                        }
                    };
                    if text != shown {
                        owner.waits(&text);
                        shown = text;
                    }
                    let waited = since.elapsed();
                    let lost = first.reached().is_none();
                    if lost != hinted && (!lost || waited >= NOT_FOUND_HINT) {
                        let sheet = root.join(crate::cli::SHEET);
                        let help = NotFound {
                            wifi: &inputs.wifi_name,
                            name: &inputs.device_name,
                            setup_key: &inputs.setup_wifi_key,
                            sheet: &sheet,
                        };
                        owner.not_found(lost.then_some(&help));
                        hinted = lost;
                    }
                    if waited >= NETWORK_WAIT {
                        owner.end(
                            false,
                            word(if driver.run.left_usb {
                                "frame.not_confirmed"
                            } else {
                                "frame.not_left_usb"
                            }),
                            &[],
                        );
                        return Ok(false);
                    }
                    tokio::time::sleep(LOOK).await;
                };
                owner.not_found(None);
                if confirmed {
                    driver.started()?;
                } else {
                    // Back in USB mode: write again, with another Wi-Fi, or go back.
                    let why = match refused {
                        Some(why) => word("frame.back_in_usb_reached").replace("{why}", &why),
                        None => word("frame.back_in_usb").to_owned(),
                    };
                    owner.say(&why);
                    if !match owner.back(&why, true) {
                        Some(back) => choose(&mut driver, inputs, back, typed)?,
                        None => false,
                    } {
                        return Ok(false);
                    }
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
                // Installing right away spares switching the mower off and on, after which the backup may not fit.
                if owner.agree(&Ask {
                    findings: vec![word("frame.checked").to_owned()],
                    text: word("frame.install_now").to_owned(),
                    yes: word("frame.answer_install_now"),
                    no: word("frame.answer_later"),
                }) {
                    match typed.for_install(inputs) {
                        Ok(()) => {
                            owner.goal(Goal::Install);
                            driver.install_now()?;
                            continue;
                        }
                        Err(error) => owner.error(&explain(texts, &error)),
                    }
                }
                let more: Vec<String> =
                    later(texts, mower.after(Goal::Check)).into_iter().collect();
                owner.end(
                    true,
                    &word("frame.checked_next").replace("{folder}", &root.display().to_string()),
                    &more,
                );
                return Ok(true);
            }
            Next::Done => {
                let goal = driver.run.goal;
                let name = &inputs.device_name;
                let mut more = Vec::new();
                let text = match (goal, found) {
                    (Goal::Back, _) => word("frame.done_back").to_owned(),
                    (_, Some(crate::net::SETUP_ADDR)) => {
                        owner.app(SETUP_URL);
                        more.extend(later(texts, mower.setup_wifi_later()));
                        word("frame.done_install_setup").replace("{setup}", SETUP_URL)
                    }
                    (_, Some(addr)) => {
                        log.info("first_start", &format!("confirmed at {addr}"));
                        owner.app(&format!("http://{addr}/"));
                        more.push(
                            word("frame.app_at")
                                .replace("{addr}", &addr.to_string())
                                .replace("{name}", name),
                        );
                        word("frame.done_install").to_owned()
                    }
                    (_, None) => {
                        owner.app(&format!("http://{name}.local/"));
                        more.push(word("frame.app_at_name").replace("{name}", name));
                        word("frame.done_install").to_owned()
                    }
                };
                more.extend(later(texts, mower.after(goal)));
                owner.end(true, &text, &more);
                return Ok(true);
            }
        }
    }
}

/// Applies what the owner chose after an unfinished write or a failed start; false when no Wi-Fi came.
fn choose<M: Mower>(
    driver: &mut Driver<'_, M>,
    inputs: &mut Inputs,
    back: Back,
    typed: &mut dyn Typed,
) -> Result<bool> {
    match back {
        Back::Vendor => driver.choose(true)?,
        Back::Again => driver.choose(false)?,
        Back::OtherWifi => {
            let Ok(wifi) = typed.wifi() else {
                return Ok(false);
            };
            inputs.wifi_name = wifi.name;
            inputs.wifi_key = wifi.key;
            inputs.wifi_hidden = wifi.hidden;
            driver.choose_changed()?;
        }
    }
    Ok(true)
}

/// The text of a code of the mower module, when it names one.
fn later(texts: &[(&str, &'static str, &'static str)], code: &'static str) -> Option<String> {
    (!code.is_empty()).then(|| text_of(texts, code).to_owned())
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
        backs: RefCell<Vec<Back>>,
        agains: RefCell<Vec<bool>>,
        /// What the owner does while the mower is looked for, once.
        help: Cell<Option<Help>>,
        ended: RefCell<Vec<String>>,
        app: RefCell<String>,
    }

    impl Script {
        fn agreeing() -> Self {
            let owner = Script::default();
            owner.yes.set(true);
            owner
        }
    }

    impl Owner for Script {
        fn say(&self, text: &str) {
            self.said.borrow_mut().push(text.to_owned());
        }
        fn agree(&self, ask: &Ask) -> bool {
            self.said.borrow_mut().extend(ask.findings.iter().cloned());
            self.yes.get()
        }
        fn back(&self, why: &str, _: bool) -> Option<Back> {
            self.say(why);
            let mut backs = self.backs.borrow_mut();
            (!backs.is_empty()).then(|| backs.remove(0))
        }
        fn helped(&self) -> Option<Help> {
            self.help.take()
        }
        fn app(&self, link: &str) {
            *self.app.borrow_mut() = link.to_owned();
        }
        fn end(&self, _: bool, text: &str, more: &[String]) {
            self.ended.borrow_mut().push(text.to_owned());
            self.ended.borrow_mut().extend(more.iter().cloned());
            self.say(text);
        }
        fn again(&self) -> bool {
            let mut agains = self.agains.borrow_mut();
            !agains.is_empty() && agains.remove(0)
        }
    }

    /// Answers of confirm in turn, then confirmed; at: where the mower answers.
    struct Confirms(RefCell<Vec<Option<String>>>, Cell<Option<IpAddr>>);

    impl Confirms {
        fn new(answers: Vec<Option<String>>) -> Self {
            Confirms(RefCell::new(answers), Cell::new(None))
        }
    }

    impl FirstStart for Confirms {
        async fn confirm(&self) -> Result<Option<String>> {
            let mut left = self.0.borrow_mut();
            Ok(if left.is_empty() {
                None
            } else {
                left.remove(0)
            })
        }
        fn reached(&self) -> Option<IpAddr> {
            self.1.get()
        }
        fn also_at(&self, addr: IpAddr) {
            self.1.set(Some(addr));
            self.0.borrow_mut().clear();
        }
    }

    /// Another Wi-Fi when asked; an install after a check goes on with the inputs of the world.
    struct Gast;

    impl Typed for Gast {
        fn wifi(&mut self) -> Result<Wifi> {
            Ok(Wifi {
                name: "Gast".into(),
                key: Secret::new("anderes123".into()),
                hidden: true,
            })
        }
        fn for_install(&mut self, _: &mut Inputs) -> Result<()> {
            Ok(())
        }
    }

    async fn go(
        w: &mut World,
        fake: &Fake,
        owner: &Script,
        confirms: Vec<Option<String>>,
    ) -> Result<bool> {
        let first = Confirms::new(confirms);
        run(
            fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &mut w.inputs,
            Goal::Install,
            &first,
            owner,
            &Bar::new(&[]),
            &w.cancel,
            &mut Gast,
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn installs_and_confirms_the_first_start() {
        let mut w = World::new();
        let fake = Fake::default();
        let owner = Script::agreeing();
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
        let first = Confirms::new(vec![]);
        let checked = run(
            &fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &mut w.inputs,
            Goal::Check,
            &first,
            &owner,
            &Bar::new(&[]),
            &w.cancel,
            &mut Gast,
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

    // Installing right after the check: the mower stays as it is, nothing is read or checked twice.
    #[tokio::test(start_paused = true)]
    async fn a_check_goes_on_to_install_on_a_yes() {
        let mut w = World::new();
        let fake = Fake::default();
        let owner = Script::agreeing();
        let first = Confirms::new(vec![]);
        let done = run(
            &fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &mut w.inputs,
            Goal::Check,
            &first,
            &owner,
            &Bar::new(&[]),
            &w.cancel,
            &mut Gast,
        )
        .await;
        assert!(done.unwrap());
        assert_eq!(
            *fake.calls.borrow(),
            ["open", "backup", "check", "prepare", "write"]
        );
        assert!(owner.ended.borrow()[0].contains(word("frame.done_install")));
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
    async fn the_write_needs_a_yes_also_without_findings() {
        let mut w = World::new();
        let fake = Fake::default();
        let owner = Script::default();
        assert!(!go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!((fake.ran("check"), fake.ran("prepare")), (1, 0));
        assert!(
            owner
                .said
                .borrow()
                .iter()
                .any(|s| s == word("frame.checks_passed"))
        );
        owner.yes.set(true);
        assert!(go(&mut w, &fake, &owner, vec![]).await.unwrap());
        assert_eq!(fake.ran("write"), 1);
    }

    // The mower fell back into USB mode: the owner gives another Wi-Fi, the pack is made again with it.
    #[tokio::test(start_paused = true)]
    async fn another_wifi_after_a_failed_start() {
        let mut w = World::new();
        let fake = Fake::default();
        *fake.seen.borrow_mut() = vec![true, false];
        *fake.usb_again.borrow_mut() = true;
        let owner = Script::agreeing();
        owner.backs.borrow_mut().push(Back::OtherWifi);
        let not_yet = vec![Some("not found".to_string()); 5];
        assert!(go(&mut w, &fake, &owner, not_yet).await.unwrap());
        assert_eq!(w.inputs.wifi_name, "Gast");
        assert_eq!(w.inputs.wifi_key.expose(), "anderes123");
        assert!(w.inputs.wifi_hidden);
        assert_eq!(
            (fake.ran("prepare"), fake.ran("write"), fake.ran("back")),
            (2, 2, 0)
        );
    }

    async fn found_at(at: Option<IpAddr>, help: Option<Help>) -> Script {
        let mut w = World::new();
        let fake = Fake::default();
        let owner = Script::agreeing();
        owner.help.set(help);
        let never = if help.is_some() { 1000 } else { 0 };
        let first = Confirms::new(vec![Some("not found".to_string()); never]);
        first.1.set(at);
        let done = run(
            &fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &mut w.inputs,
            Goal::Install,
            &first,
            &owner,
            &Bar::new(&[]),
            &w.cancel,
            &mut Gast,
        )
        .await;
        assert!(done.unwrap());
        owner
    }

    #[tokio::test(start_paused = true)]
    async fn the_end_names_where_the_mower_answered() {
        let home: IpAddr = "192.168.1.50".parse().unwrap();
        let owner = found_at(Some(home), None).await;
        assert_eq!(*owner.app.borrow(), "http://192.168.1.50/");
        let ended = owner.ended.borrow().join("\n");
        assert!(
            ended.contains("http://192.168.1.50/") && ended.contains("http://mowbite-ab12.local/"),
            "{ended}"
        );

        // On its setup Wi-Fi the mower is not on the Wi-Fi of the owner yet.
        let owner = found_at(Some(crate::net::SETUP_ADDR), None).await;
        assert_eq!(*owner.app.borrow(), SETUP_URL);
        assert!(owner.ended.borrow()[0].contains(SETUP_URL));
    }

    #[tokio::test(start_paused = true)]
    async fn the_owner_helps_the_search() {
        let given: IpAddr = "10.0.0.7".parse().unwrap();
        let owner = found_at(None, Some(Help::Address(given))).await;
        assert_eq!(*owner.app.borrow(), "http://10.0.0.7/");
        // Confirmed in the app: the frame knows only the name.
        let owner = found_at(None, Some(Help::Confirmed)).await;
        assert_eq!(*owner.app.borrow(), "http://mowbite-ab12.local/");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_start_goes_back_on_the_owners_word() {
        let mut w = World::new();
        let fake = Fake::default();
        // Still listed right after the restart, then gone, then back after a failed start.
        *fake.seen.borrow_mut() = vec![true, false];
        *fake.usb_again.borrow_mut() = true;
        let owner = Script::agreeing();
        owner.backs.borrow_mut().push(Back::Vendor);
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
        let owner = Script::agreeing();
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
        let owner = Script::agreeing();
        owner.backs.borrow_mut().push(Back::Vendor);
        let never = vec![Some("not found".to_string()); 1000];
        assert!(go(&mut w, &fake, &owner, never).await.unwrap());
        assert_eq!(fake.ran("back"), 1);
        let said = owner.said.borrow().join("\n");
        assert!(said.contains(&word("frame.not_found")[..20]), "{said}");
        assert!(said.contains(word("frame.back_in_usb")), "{said}");
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
        let owner = Script::agreeing();
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
        let owner = Script::agreeing();
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
        owner.backs.borrow_mut().push(Back::Vendor);
        let first = Confirms::new(vec![]);
        let back = run(
            &fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &mut w.inputs,
            Goal::Back,
            &first,
            &owner,
            &Bar::new(&[]),
            &w.cancel,
            &mut Gast,
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
        let owner = Script::agreeing();
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
        let first = Confirms::new(vec![]);
        let back = run(
            &fake,
            &mut w.store,
            &w.work,
            &w.tables,
            &w.base,
            &mut w.inputs,
            Goal::Back,
            &first,
            &owner,
            &Bar::new(&[]),
            &w.cancel,
            &mut Gast,
        )
        .await;
        assert!(!back.unwrap());
        assert_eq!(fake.ran("back"), 0);
        // A stop is checked again on the next install (newer tables may know the mower).
        fake.findings.borrow_mut().clear();
        owner.yes.set(true);
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
