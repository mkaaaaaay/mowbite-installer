//! The installer: without arguments on a page in the browser, else in text mode:
//! mowbite-installer install|check|back <backup folder> [<release folder>].

use anyhow::{Context, Result};
use mowbite_installer::cli::{self, Args};
use mowbite_installer::mower::{Mower, Progress, Tables, Wifi};
use mowbite_installer::run::Goal;
use mowbite_installer::run::Run;
use mowbite_installer::secret::Secret;
use mowbite_installer::sign::Signers;
use mowbite_installer::text::{Bar, Owner, explain, german, text_of, word};
use mowbite_installer::tron::module::{SPACE, Tron, before, check_wifi};
use mowbite_installer::tron::port::UsbPort;
use mowbite_installer::tron::{tables, texts::TEXTS};
use mowbite_installer::web::{Input, Note, PageOwner, Server, Shared, goal_id, goal_of};
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
    if args.download
        && let Err(error) = fetch::fetch(
            &fetch::Https::new(),
            signers,
            "tron",
            &args.release,
            progress,
        )
    {
        // Without a way to GitHub a release loaded before serves; each of its files is checked again below.
        if cli::tables(&args.release, signers, "tron").is_err() {
            return Err(error.context("frame.download_failed"));
        }
        progress.step("release.kept");
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
    anyhow::ensure!(
        cli::store_fits(&args.store),
        "{}",
        word("frame.store_foreign")
    );
    cli::check_space(&args, SPACE)?;
    let signers = Signers::builtin();
    let (tron, tables) = tron(&args, &signers, &Bar::new(TEXTS))?;
    cli::run(&tron, &args, &signers, &tables).await
}

/// Where the page suggests the backup folder: in the home folder.
fn default_store() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map_or_else(PathBuf::new, PathBuf::from);
    home.join(cli::store_name())
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

/// What the page sent to start: the goal and the backup folder, for install the Wi-Fi.
fn start_args(params: &Value) -> Result<(Args, Option<Wifi>), &'static str> {
    let goal = goal_of(params["goal"].as_str().unwrap_or_default()).ok_or("frame.store_missing")?;
    let store = PathBuf::from(params["store"].as_str().unwrap_or_default().trim());
    if store.as_os_str().is_empty() {
        return Err("frame.store_missing");
    }
    if !cli::store_fits(&store) {
        return Err("frame.store_foreign");
    }
    let wifi = match goal {
        Goal::Install => {
            let name = params["wifi_name"].as_str().unwrap_or_default();
            if name.is_empty() {
                return Err("frame.wifi_missing");
            }
            let key = params["wifi_key"].as_str().unwrap_or_default();
            Some(Wifi {
                name: name.to_owned(),
                key: Secret::new(key.to_owned()),
                hidden: params["wifi_hidden"] == true,
            })
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
        more: Vec::new(),
    };
    let (args, wifi) = start_args(params).map_err(|code| refused(word(code).to_owned()))?;
    // Before the download: a typo in the key costs no wait.
    if let Some(wifi) = &wifi {
        check_wifi(&wifi.name, wifi.key.expose())
            .map_err(|error| refused(explain(TEXTS, &error)))?;
    }
    cli::check_space(&args, SPACE).map_err(|error| refused(explain(TEXTS, &error)))?;
    let signers = Signers::builtin();
    let owner = PageOwner::new(shared.clone(), args.goal, TEXTS);
    let ended = |error: anyhow::Error| {
        owner.error(&explain(TEXTS, &error));
        let half_written = Run::load(&args.store)
            .ok()
            .flatten()
            .is_some_and(|run| run.writing);
        let text = if half_written {
            "frame.incomplete"
        } else {
            "frame.nothing_written"
        };
        owner.end(false, word(text), &[]);
        Ok(false)
    };
    let (tron, tables) = match tron(&args, &signers, &owner) {
        Ok(found) => found,
        Err(error) => return ended(error),
    };
    // The Wi-Fi typed at the start once; after a failed start the page asks for another one.
    let mut typed = wifi;
    let mut last = String::new();
    let mut take_wifi = || {
        if let Some(wifi) = typed.take() {
            tron.check_wifi(&wifi.name, wifi.key.expose())?;
            last = wifi.name.clone();
            return Ok(wifi);
        }
        let mut refused: Option<String> = None;
        loop {
            let wifi = owner.wifi(&last, refused.as_deref()).context("no Wi-Fi")?;
            last = wifi.name.clone();
            match tron.check_wifi(&wifi.name, wifi.key.expose()) {
                Ok(()) => return Ok(wifi),
                Err(error) => refused = Some(explain(TEXTS, &error)),
            }
        }
    };
    let mut prepared = match cli::prepare(&tron, &args, &signers, &owner, &owner, &mut take_wifi) {
        Ok(prepared) => prepared,
        Err(error) => {
            shared.update(|v| v.view = "start");
            return Err(refused(explain(TEXTS, &error)));
        }
    };
    cli::go(
        &tron,
        &mut prepared,
        &tables,
        args.goal,
        &owner,
        &owner,
        &shared.cancel,
        &mut take_wifi,
    )
    .await
    .or_else(ended)
}

async fn page_mode() -> Result<bool> {
    let shared = Arc::new(Shared::default());
    let server = Server::start(shared.clone())?;
    shared.update(|v| {
        v.lang = if german() { "de" } else { "en" };
        v.view = "start";
        for goal in [Goal::Install, Goal::Check, Goal::Back] {
            v.before
                .insert(goal_id(goal), text_of(TEXTS, before(goal)).to_owned());
        }
        v.store = default_store().display().to_string();
        v.wifi_now = mowbite_installer::net::current_wifi();
        v.space = SPACE;
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
            eprintln!("{}", explain(TEXTS, &error));
            ExitCode::from(2)
        }
    }
}
