//! The command line of the text mode: install or back, the backup folder and the folder with the release files.
//! Loads the release files against the built-in keys and asks the owner for the Wi-Fi.

use crate::mower::{BaseImage, Inputs, Mower, Tables, WorkDir};
use crate::net::Confirm;
use crate::release;
use crate::run::Goal;
use crate::secret::Secret;
use crate::sign::Signers;
use crate::store::Store;
use crate::text::{self, Terminal};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const USAGE: &str =
    "usage: mowbite-installer install|back <backup folder> <release folder> [--work <folder>]";

/// What a new system takes from its inbox: one bundle each.
const BUNDLES: [&str; 3] = ["os", "planner", "app"];
/// Name of the mower and key of its setup Wi-Fi, beside the backup.
pub const SHEET: &str = "setup-wifi.txt";
/// No 0/o, 1/l/i: the key is read off a sheet and typed on a phone.
const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
const KEY_LEN: usize = 16;

#[derive(Debug, PartialEq)]
pub struct Args {
    pub goal: Goal,
    pub store: PathBuf,
    pub release: PathBuf,
    /// Holds the work images with keys; emptied after the read back. Default: work in the backup folder.
    pub work: PathBuf,
}

pub fn parse(args: &[String]) -> Result<Args> {
    let mut plain = Vec::new();
    let mut work = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--work" => work = Some(PathBuf::from(it.next().context(USAGE)?)),
            other if other.starts_with('-') => bail!("unknown option {other}\n{USAGE}"),
            _ => plain.push(arg),
        }
    }
    let [goal, store, release] = plain.as_slice() else {
        bail!(USAGE);
    };
    let goal = match goal.as_str() {
        "install" => Goal::Install,
        "back" => Goal::Back,
        _ => bail!(USAGE),
    };
    let store = PathBuf::from(store);
    Ok(Args {
        goal,
        work: work.unwrap_or_else(|| store.join("work")),
        store,
        release: PathBuf::from(release),
    })
}

/// name in the release folder; the name may come from a file not yet verified, so only a plain one is taken.
pub fn release_file(release: &Path, name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\', ':']),
        "{name:?} is no plain file name"
    );
    Ok(release.join(name))
}

pub fn signature(path: &Path) -> Result<String> {
    let mut sig = path.as_os_str().to_owned();
    sig.push(".sig");
    std::fs::read_to_string(&sig).with_context(|| format!("{} has no .sig", path.display()))
}

pub fn tables(release: &Path, signers: &Signers, module: &str) -> Result<Tables> {
    let path = release.join("tables.json");
    let json = std::fs::read(&path).with_context(|| path.display().to_string())?;
    release::tables(&json, &signature(&path)?, signers, module)
}

pub fn base(release: &Path, signers: &Signers, platform: &str) -> Result<BaseImage> {
    #[derive(Deserialize)]
    struct Named {
        file: String,
    }
    let path = release.join("base.json");
    let sidecar = std::fs::read(&path).with_context(|| path.display().to_string())?;
    let named: Named = serde_json::from_slice(&sidecar).context("base.json")?;
    let image = release_file(release, &named.file)?;
    Ok(release::base(&sidecar, &signature(&path)?, signers, &image, platform)?.0)
}

/// The bundles for the inbox, each checked against its component's key; each comes with its .sig, which the
/// mower checks again.
pub fn bundles(release: &Path, signers: &Signers) -> Result<Vec<PathBuf>> {
    let mut names: Vec<String> = std::fs::read_dir(release)
        .with_context(|| release.display().to_string())?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".tar.gz"))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for component in BUNDLES {
        let prefix = format!("mowbite-{component}-");
        let found: Vec<&String> = names.iter().filter(|n| n.starts_with(&prefix)).collect();
        let [name] = found.as_slice() else {
            bail!(
                "the release folder needs one {prefix}*.tar.gz, it has {}",
                found.len()
            );
        };
        let path = release.join(name);
        let data = std::fs::read(&path).with_context(|| path.display().to_string())?;
        signers
            .verify(&data, &signature(&path)?, &format!("mowbite-{component}"))
            .with_context(|| name.to_string())?;
        let mut sig = path.clone().into_os_string();
        sig.push(".sig");
        out.push(path);
        out.push(sig.into());
    }
    Ok(out)
}

fn random(len: usize) -> Result<String> {
    let mut out = String::with_capacity(len);
    let mut byte = [0u8; 1];
    // Only bytes below a whole multiple of the alphabet, so every letter is as likely.
    let limit = 256 - 256 % ALPHABET.len();
    while out.len() < len {
        getrandom::getrandom(&mut byte)
            .map_err(|e| anyhow!("no randomness from the system: {e}"))?;
        if usize::from(byte[0]) < limit {
            out.push(char::from(ALPHABET[usize::from(byte[0]) % ALPHABET.len()]));
        }
    }
    Ok(out)
}

/// The mower's name and the key of its setup Wi-Fi: made once per backup folder, then read from the sheet, so a
/// written mower and its sheet always agree.
pub fn setup(store: &Path) -> Result<(String, Secret<String>)> {
    let path = store.join(SHEET);
    if !path.exists() {
        let text = format!(
            "MowBite setup Wi-Fi of this mower. Keep this sheet to yourself.\nname={}\nkey={}\n",
            format_args!("mowbite-{}", random(4)?),
            random(KEY_LEN)?
        );
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&path)
            .with_context(|| path.display().to_string())?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    let text = std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(str::to_owned)
            .with_context(|| format!("{} has no {key}", path.display()))
    };
    Ok((field("name=")?, Secret::new(field("key=")?)))
}

/// Wi-Fi name and key of the home network; the key is not shown while typed.
fn ask_wifi() -> Result<(String, Secret<String>)> {
    loop {
        print!("Name of your Wi-Fi: ");
        std::io::stdout().flush()?;
        let mut name = String::new();
        std::io::stdin().read_line(&mut name)?;
        let name = name.trim_end_matches(['\r', '\n']).to_owned();
        let key = Secret::new(rpassword::prompt_password("Wi-Fi key (not shown): ")?);
        let len = key.expose().len();
        if !name.is_empty() && name.len() <= 32 && (8..=63).contains(&len) {
            return Ok((name, key));
        }
        println!("A Wi-Fi name has 1 to 32 characters, a key 8 to 63. Please again.");
    }
}

/// Reads everything a goal needs, then runs it in the terminal. Ok(true) when the goal was reached.
pub async fn run<M: Mower>(
    mower: &M,
    args: &Args,
    signers: &Signers,
    tables: &Tables,
) -> Result<bool> {
    let mut store = Store::open(&args.store)?;
    let (base, inputs) = match args.goal {
        Goal::Install => {
            let base = base(&args.release, signers, mower.id())?;
            let bundles = bundles(&args.release, signers)?;
            let (device_name, setup_wifi_key) = setup(&args.store)?;
            println!(
                "The mower will be called {device_name}. Should it not find your Wi-Fi, it opens its own; its \
                 name and key are in {}.",
                args.store.join(SHEET).display()
            );
            let (wifi_name, wifi_key) = ask_wifi()?;
            let inputs = Inputs {
                wifi_name,
                wifi_key,
                device_name,
                setup_wifi_key,
                bundles,
            };
            (base, inputs)
        }
        // The way back writes the backup only; base and inputs are never used on it.
        Goal::Back => (
            BaseImage {
                path: PathBuf::new(),
                sha256: String::new(),
                pack: vec![],
            },
            Inputs {
                wifi_name: String::new(),
                wifi_key: Secret::new(String::new()),
                device_name: String::new(),
                setup_wifi_key: Secret::new(String::new()),
                bundles: Vec::new(),
            },
        ),
    };
    let work = WorkDir {
        path: args.work.clone(),
    };
    let first = Confirm::new(&inputs.device_name);
    text::run(
        mower,
        &mut store,
        &work,
        tables,
        &base,
        &inputs,
        args.goal,
        &first,
        &Terminal,
        &Default::default(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args> {
        parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn the_command_line() {
        let a = args(&["install", "b", "r"]).unwrap();
        assert_eq!(
            a,
            Args {
                goal: Goal::Install,
                store: "b".into(),
                release: "r".into(),
                work: Path::new("b").join("work"),
            }
        );
        let a = args(&["back", "b", "r", "--work", "w"]).unwrap();
        assert_eq!((a.goal, a.work), (Goal::Back, PathBuf::from("w")));
        for bad in [
            &["install", "b"][..],
            &["remove", "b", "r"],
            &["install", "b", "r", "--yes"],
            &["install", "b", "r", "--work"],
        ] {
            assert!(args(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn release_names_stay_in_the_folder() {
        assert!(release_file(Path::new("r"), "base-2.img").is_ok());
        for bad in ["", "../x", "a/b", "a\\b", ".hidden", "c:x"] {
            assert!(release_file(Path::new("r"), bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_sheet_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (name, key) = setup(dir.path()).unwrap();
        assert!(name.starts_with("mowbite-") && name.len() == 12);
        assert_eq!(key.expose().len(), KEY_LEN);
        assert!(key.expose().bytes().all(|b| ALPHABET.contains(&b)));
        let (again, key_again) = setup(dir.path()).unwrap();
        assert_eq!((again, key_again.expose()), (name, key.expose()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(SHEET))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let other = tempfile::tempdir().unwrap();
        assert_ne!(setup(other.path()).unwrap().1.expose(), key.expose());
    }

    #[cfg(unix)]
    #[test]
    fn bundles_need_one_good_signature_each() {
        use crate::testing::signed;
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        std::fs::create_dir(&keys).unwrap();
        let release = dir.path().join("release");
        std::fs::create_dir(&release).unwrap();
        let mut public = String::new();
        for component in BUNDLES {
            let name = format!("mowbite-{component}-abc.tar.gz");
            let data = format!("{component} bundle");
            let Some((p, sig)) =
                signed(&keys, "k", data.as_bytes(), &format!("mowbite-{component}"))
            else {
                return;
            };
            public = p;
            std::fs::write(release.join(&name), &data).unwrap();
            std::fs::write(release.join(format!("{name}.sig")), sig).unwrap();
        }
        let signers = Signers::parse(&format!(
            "mowbite namespaces=\"mowbite-os,mowbite-planner,mowbite-app\" {public}\n"
        ))
        .unwrap();
        let found = bundles(&release, &signers).unwrap();
        let names: Vec<_> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "mowbite-os-abc.tar.gz",
                "mowbite-os-abc.tar.gz.sig",
                "mowbite-planner-abc.tar.gz",
                "mowbite-planner-abc.tar.gz.sig",
                "mowbite-app-abc.tar.gz",
                "mowbite-app-abc.tar.gz.sig"
            ]
        );

        // An app bundle signed as os is refused.
        let (_, as_os) = signed(&keys, "k", b"app bundle", "mowbite-os").unwrap();
        std::fs::write(release.join("mowbite-app-abc.tar.gz.sig"), as_os).unwrap();
        assert!(bundles(&release, &signers).is_err());

        // Two os bundles are refused.
        let (_, sig) = signed(&keys, "k", b"app bundle", "mowbite-app").unwrap();
        std::fs::write(release.join("mowbite-app-abc.tar.gz.sig"), sig).unwrap();
        std::fs::write(release.join("mowbite-os-def.tar.gz"), b"x").unwrap();
        assert!(bundles(&release, &signers).is_err());
    }
}
