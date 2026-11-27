//! The installer: without arguments on a page in the browser, else in text mode:
//! mowbite-installer install|check|back <backup folder> [<release folder>].

use anyhow::{Context, Result};
use mowbite_installer::cli::{self, Args};
use mowbite_installer::mower::{Mower, Progress, Tables};
use mowbite_installer::run::Goal;
use mowbite_installer::secret::Secret;
use mowbite_installer::sign::Signers;
use mowbite_installer::text::{Bar, Owner, german, text_of, word};
use mowbite_installer::tron::module::Tron;
use mowbite_installer::tron::port::UsbPort;
use mowbite_installer::tron::{tables, texts::TEXTS};
use mowbite_installer::web::{Input, Note, PageOwner, Server, Shared};
use mowbite_installer::{fetch, release};
use serde_json::Value;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

/// The page reads the end before the installer goes.
const LAST_LOOK: Duration = Duration::from_secs(3);

/// The release files (downloaded first when asked) and the Tron module over its loader.
fn tron(
    args: &Args,
    signers: &Signers,
    progress: &dyn Progress,
) -> Result<(Tron<UsbPort>, Tables)> {
    if args.download {
        fetch::fetch(
            &fetch::Https::new(),
            signers,
            "tron",
            &args.release,
            progress,
        )?;
    }
    let tables = cli::tables(&args.release, signers, "tron")?;
    let named = tables::loader(&tables.raw)?;
    let path = cli::release_file(&args.release, &named.file)?;
    let bytes = std::fs::read(&path).with_context(|| path.display().to_string())?;
    let loader = release::loader(&bytes, &cli::signature(&path)?, signers)?;
    Ok((
        Tron {
            port: UsbPort { loader },
        },
        tables,
    ))
}

async fn text_mode(list: &[String]) -> Result<bool> {
    let args = cli::parse(list)?;
    let signers = Signers::builtin();
    let (tron, tables) = tron(&args, &signers, &Bar::new(TEXTS))?;
    cli::run(&tron, &args, &signers, &tables).await
}

/// Where the page suggests the backup folder: in the home folder.
fn default_store() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map_or_else(PathBuf::new, PathBuf::from);
    home.join(if german() {
        "MowBite-Sicherung"
    } else {
        "MowBite-Backup"
    })
}

fn open_browser(url: &str) {
    #[cfg(windows)]
    let opened = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let opened = std::process::Command::new("xdg-open").arg(url).spawn();
    if opened.is_err() {
        println!("{}", word("frame.page").replace("{url}", url));
    }
}

type Wifi = (String, Secret<String>);

/// What the page sent to start: the goal and the backup folder, for install the Wi-Fi.
fn start_args(params: &Value) -> Result<(Args, Option<Wifi>), &'static str> {
    let goal = match params["goal"].as_str() {
        Some("install") => Goal::Install,
        Some("check") => Goal::Check,
        Some("back") => Goal::Back,
        _ => return Err("frame.store_missing"),
    };
    let store = PathBuf::from(params["store"].as_str().unwrap_or_default().trim());
    if store.as_os_str().is_empty() {
        return Err("frame.store_missing");
    }
    let wifi = match goal {
        Goal::Install => {
            let name = params["wifi_name"].as_str().unwrap_or_default();
            if name.is_empty() {
                return Err("frame.wifi_missing");
            }
            let key = params["wifi_key"].as_str().unwrap_or_default();
            Some((name.to_owned(), Secret::new(key.to_owned())))
        }
        Goal::Check | Goal::Back => None,
    };
    let release = store.join(cli::DOWNLOADED);
    Ok((
        Args {
            goal,
            work: store.join("work"),
            release,
            download: goal != Goal::Back,
            store,
        },
        wifi,
    ))
}

/// One start from the page. Err: back to the start with this note, nothing touched the mower.
async fn page_run(shared: &Arc<Shared>, params: &Value) -> Result<bool, Note> {
    let refused = |text: String| Note {
        tone: "bad",
        text,
        code: None,
        link: None,
        keep: false,
    };
    let (args, wifi) = start_args(params).map_err(|code| refused(word(code).to_owned()))?;
    let signers = Signers::builtin();
    let owner = PageOwner::new(shared.clone(), args.goal, TEXTS);
    let ended = |error: anyhow::Error| {
        owner.error(&format!("{error:#}"));
        owner.end(false, word("frame.nothing_written"));
        Ok(false)
    };
    let (tron, tables) = match tron(&args, &signers, &owner) {
        Ok(found) => found,
        Err(error) => return ended(error),
    };
    let mut wifi = wifi;
    let mut take_wifi = || {
        let (name, key) = wifi.take().context("no Wi-Fi")?;
        tron.check_wifi(&name, key.expose())?;
        Ok((name, key))
    };
    let mut prepared = match cli::prepare(&tron, &args, &signers, &owner, &owner, &mut take_wifi) {
        Ok(prepared) => prepared,
        Err(error) => {
            shared.update(|v| v.view = "start");
            return Err(refused(format!("{error:#}")));
        }
    };
    *owner.app.borrow_mut() = Some(format!("http://{}.local/", prepared.inputs.device_name));
    cli::go(&tron, &mut prepared, &tables, args.goal, &owner, &owner)
        .await
        .or_else(ended)
}

async fn page_mode() -> Result<bool> {
    let shared = Arc::new(Shared::default());
    let server = Server::start(shared.clone())?;
    shared.update(|v| {
        v.lang = if german() { "de" } else { "en" };
        v.view = "start";
        v.before = Some(text_of(TEXTS, "tron.before").to_owned());
        v.store = default_store().display().to_string();
    });
    let url = server.url();
    println!("{}", word("frame.page").replace("{url}", &url));
    open_browser(&url);
    loop {
        let Some(Input::Start(params)) = shared.next(None) else {
            continue;
        };
        match page_run(&shared, &params).await {
            Ok(done) => {
                shared.update(|v| v.closed = true);
                std::thread::sleep(LAST_LOOK);
                return Ok(done);
            }
            Err(note) => shared.update(|v| {
                v.view = "start";
                v.steps.clear();
                v.start_error = Some(note);
            }),
        }
    }
}

fn main() -> ExitCode {
    println!("mowbite-installer {}", env!("CARGO_PKG_VERSION"));
    let list: Vec<String> = std::env::args().skip(1).collect();
    let done = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("no runtime")
        .and_then(|rt| {
            rt.block_on(async {
                if list.is_empty() {
                    page_mode().await
                } else {
                    text_mode(&list).await
                }
            })
        });
    match done {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(2)
        }
    }
}
