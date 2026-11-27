//! The command line of the text mode: install, check or back, the backup folder and the folder with the release
//! files, which the installer downloads into the backup folder when none is given. Loads the release files against
//! the built-in keys and asks the owner for the Wi-Fi.

use crate::mower::{BaseImage, Bundle, Inputs, Mower, Progress, Tables, WorkDir};
use crate::net::Confirm;
use crate::release;
use crate::run::Goal;
use crate::secret::Secret;
use crate::sign::Signers;
use crate::store::Store;
use crate::text::{self, Bar, Owner, Terminal};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use sha2::Digest;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: mowbite-installer install|check|back <backup folder> [<release folder>] [--work <folder>]\n\
     check saves the mower and checks it, it writes nothing. Without a release folder the installer downloads the \
     newest release into the backup folder.";
/// The release folder inside the backup folder when the installer downloads it.
pub const DOWNLOADED: &str = "release";

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
    /// The release folder is DOWNLOADED in the backup folder, filled from the release repo.
    pub download: bool,
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
    let (goal, store, release) = match plain.as_slice() {
        [goal, store] => (goal, PathBuf::from(store), None),
        [goal, store, release] => (goal, PathBuf::from(store), Some(PathBuf::from(release))),
        _ => bail!(USAGE),
    };
    let goal = match goal.as_str() {
        "install" => Goal::Install,
        "check" => Goal::Check,
        "back" => Goal::Back,
        _ => bail!(USAGE),
    };
    Ok(Args {
        goal,
        work: work.unwrap_or_else(|| store.join("work")),
        download: release.is_none(),
        release: release.unwrap_or_else(|| store.join(DOWNLOADED)),
        store,
    })
}

/// The name of a new backup folder.
pub fn store_name() -> &'static str {
    if text::german() {
        "MowBite-Sicherung"
    } else {
        "MowBite-Backup"
    }
}

/// Whether dir can be the backup folder: missing, empty or made by the installer. The installer limits its rights to
/// the owner, so a folder of other files (Documents, a USB stick) is never taken.
pub fn store_fits(dir: &Path) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return e.kind() == std::io::ErrorKind::NotFound,
    };
    let mut entries = entries.flatten().peekable();
    entries.peek().is_none()
        || entries.any(|e| {
            [crate::store::SUMS, DOWNLOADED, "work", SHEET]
                .contains(&&*e.file_name().to_string_lossy())
        })
}

/// The backup folder for a folder the owner picked: the folder itself when it fits, else a new one inside it.
pub fn store_in(picked: &Path) -> PathBuf {
    if store_fits(picked) {
        picked.to_path_buf()
    } else {
        picked.join(store_name())
    }
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

/// The base image and its number.
pub fn base(
    release: &Path,
    signers: &Signers,
    platform: &str,
    progress: &dyn Progress,
) -> Result<(BaseImage, u32)> {
    #[derive(Deserialize)]
    struct Named {
        file: String,
    }
    let path = release.join("base.json");
    let sidecar = std::fs::read(&path).with_context(|| path.display().to_string())?;
    let named: Named = serde_json::from_slice(&sidecar).context("base.json")?;
    let image = release_file(release, &named.file)?;
    release::base(
        &sidecar,
        &signature(&path)?,
        signers,
        &image,
        platform,
        progress,
    )
}

/// The base number an OS bundle needs, from its bundle.json; None when it names none.
pub fn os_needs_base(bundle: &Path) -> Result<Option<u32>> {
    Ok(crate::fetch::needs(bundle)?.base)
}

/// The bundles for the inbox, each checked against its component's key; each comes with its .sig, which the
/// mower checks again. With the sha256 of each file as checked.
pub fn bundles(release: &Path, signers: &Signers) -> Result<Vec<Bundle>> {
    let mut names: Vec<String> = std::fs::read_dir(release)
        .with_context(|| release.display().to_string())?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".tar.gz"))
        .collect();
    names.sort();
    let mut out = Vec::new();
    let hex = |data: &[u8]| crate::store::hex(&sha2::Sha256::digest(data));
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
        let sig_text = signature(&path)?;
        signers
            .verify(&data, &sig_text, &format!("mowbite-{component}"))
            .with_context(|| name.to_string())?;
        let mut sig = path.clone().into_os_string();
        sig.push(".sig");
        out.push(Bundle {
            path,
            sha256: hex(&data),
        });
        out.push(Bundle {
            path: sig.into(),
            sha256: hex(sig_text.as_bytes()),
        });
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

/// What the sheet beside the backup holds for one mower.
pub struct Setup {
    pub name: String,
    pub key: Secret<String>,
    /// The mower proves its answers on the network with it.
    pub install_id: Secret<String>,
}

/// The mower's name, the key of its setup Wi-Fi and its install id: made once per backup folder, then read from the
/// sheet, so a written mower and its sheet always agree.
pub fn setup(store: &Path) -> Result<Setup> {
    let path = store.join(SHEET);
    if !path.exists() {
        let text = format!(
            "MowBite setup Wi-Fi of this mower. Keep this sheet to yourself.\nname={}\nkey={}\nid={}\n",
            format_args!("mowbite-{}", random(4)?),
            random(KEY_LEN)?,
            crate::secret::random_hex(16)?
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
    Ok(Setup {
        name: field("name=")?,
        key: Secret::new(field("key=")?),
        install_id: Secret::new(field("id=")?),
    })
}

/// Wi-Fi name and key of the home network, checked by the rules of the mower; the key is not shown while typed.
fn ask_wifi<M: Mower>(mower: &M) -> Result<(String, Secret<String>)> {
    loop {
        print!("Name of your Wi-Fi: ");
        std::io::stdout().flush()?;
        let mut name = String::new();
        ensure!(
            std::io::stdin().read_line(&mut name)? > 0,
            "no input: the Wi-Fi is needed"
        );
        let name = name.trim_end_matches(['\r', '\n']).to_owned();
        if name.is_empty() {
            println!("Please type the name of your Wi-Fi.");
            continue;
        }
        let key = Secret::new(rpassword::prompt_password("Wi-Fi key (not shown): ")?);
        match mower.check_wifi(&name, key.expose()) {
            Ok(()) => return Ok((name, key)),
            Err(e) => println!("{e:#}. Please again."),
        }
    }
}

/// What a run needs besides the mower: read and checked before anything touches the mower.
pub struct Prepared {
    pub store: Store,
    pub work: WorkDir,
    pub base: BaseImage,
    pub inputs: Inputs,
}

/// Opens the backup folder, checks space and the release, and takes the Wi-Fi of the owner (install only).
pub fn prepare<M: Mower>(
    mower: &M,
    args: &Args,
    signers: &Signers,
    owner: &dyn Owner,
    progress: &dyn Progress,
    wifi: &mut dyn FnMut() -> Result<(String, Secret<String>)>,
) -> Result<Prepared> {
    let store = Store::open(&args.store)?;
    crate::store::private_dir(&args.work)?;
    let space = mower.space_needed();
    let work_bytes = match args.goal {
        Goal::Install => space.work_bytes,
        Goal::Check | Goal::Back => 0,
    };
    if args.goal != Goal::Back {
        crate::space::check(&args.store, space.backup_bytes, &args.work, work_bytes)?;
    }
    owner.say(
        &text::word("frame.keep_backup").replace("{folder}", &args.store.display().to_string()),
    );
    let (base, inputs) = match args.goal {
        Goal::Install | Goal::Check => {
            let (base, base_number) = base(&args.release, signers, mower.id(), progress)?;
            let bundles = bundles(&args.release, signers)?;
            if let Some(os) = bundles.first()
                && let Some(needs) = os_needs_base(&os.path)?
            {
                ensure!(
                    base_number >= needs,
                    "the OS bundle needs base system {needs}, the release folder has {base_number}"
                );
            }
            let inputs = if args.goal == Goal::Check {
                Inputs {
                    bundles,
                    ..Inputs::none()
                }
            } else {
                let setup = setup(&args.store)?;
                owner.say(
                    &text::word("frame.mower_name")
                        .replace("{name}", &setup.name)
                        .replace("{sheet}", &args.store.join(SHEET).display().to_string()),
                );
                let (wifi_name, wifi_key) = wifi()?;
                Inputs {
                    wifi_name,
                    wifi_key,
                    device_name: setup.name,
                    setup_wifi_key: setup.key,
                    install_id: setup.install_id,
                    bundles,
                }
            };
            mower.check_inputs(&base, &inputs)?;
            (base, inputs)
        }
        // The way back writes the backup only; base and inputs are never used on it.
        Goal::Back => (
            BaseImage {
                path: PathBuf::new(),
                sha256: String::new(),
                pack: vec![],
            },
            Inputs::none(),
        ),
    };
    Ok(Prepared {
        store,
        work: WorkDir {
            path: args.work.clone(),
        },
        base,
        inputs,
    })
}

/// Runs a prepared goal with this owner. Ok(true) when the goal was reached.
pub async fn go<M: Mower>(
    mower: &M,
    prepared: &mut Prepared,
    tables: &Tables,
    goal: Goal,
    owner: &dyn Owner,
    progress: &dyn Progress,
) -> Result<bool> {
    let first = Confirm::new(
        &prepared.inputs.device_name,
        Secret::new(prepared.inputs.install_id.expose().clone()),
    );
    text::run(
        mower,
        &mut prepared.store,
        &prepared.work,
        tables,
        &prepared.base,
        &prepared.inputs,
        goal,
        &first,
        owner,
        progress,
        &Default::default(),
    )
    .await
}

/// Reads everything a goal needs, then runs it in the terminal. Ok(true) when the goal was reached.
pub async fn run<M: Mower>(
    mower: &M,
    args: &Args,
    signers: &Signers,
    tables: &Tables,
) -> Result<bool> {
    // Started without any terminal (a double click): stop at once, not after minutes at the first question.
    ensure!(
        std::io::stdin().is_terminal() || std::io::stdout().is_terminal(),
        "the installer needs a terminal: it asks there for your Wi-Fi and what to do next. Open a terminal and \
         start it there.\n{USAGE}"
    );
    let before = mower.before(args.goal);
    if !before.is_empty() {
        println!("{}", text::text_of(mower.texts(), before));
    }
    let bar = Bar::new(mower.texts());
    let mut prepared = prepare(mower, args, signers, &Terminal, &bar, &mut || {
        ask_wifi(mower)
    })?;
    go(mower, &mut prepared, tables, args.goal, &Terminal, &bar).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args> {
        parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_backup_folder_is_new_empty_or_made_by_the_installer() {
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name);
        assert!(store_fits(&at("new")));
        std::fs::create_dir(at("empty")).unwrap();
        assert!(store_fits(&at("empty")));
        for ours in [crate::store::SUMS, DOWNLOADED, "work", SHEET] {
            let folder = at(&format!("with-{ours}"));
            std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join("other.txt"), "").unwrap();
            std::fs::write(folder.join(ours), "").unwrap();
            assert!(store_fits(&folder), "{ours}");
            assert_eq!(store_in(&folder), folder);
        }
        std::fs::create_dir(at("documents")).unwrap();
        std::fs::write(at("documents").join("letter.txt"), "").unwrap();
        assert!(!store_fits(&at("documents")));
        assert_eq!(
            store_in(&at("documents")),
            at("documents").join(store_name())
        );
        assert!(!store_fits(&at("documents").join("letter.txt")));
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
                download: false,
                work: Path::new("b").join("work"),
            }
        );
        let a = args(&["back", "b", "r", "--work", "w"]).unwrap();
        assert_eq!((a.goal, a.work), (Goal::Back, PathBuf::from("w")));
        let a = args(&["check", "b"]).unwrap();
        assert_eq!(
            (a.goal, a.release, a.download),
            (Goal::Check, Path::new("b").join(DOWNLOADED), true)
        );
        for bad in [
            &["install"][..],
            &["install", "b", "r", "x"],
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

    fn bundle_with(dir: &Path, meta: &str) -> PathBuf {
        let path = dir.join("mowbite-os-abc.tar.gz");
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        for (name, data) in [("abc/install/x", "1"), ("abc/bundle.json", meta)] {
            let mut header = tar::Header::new_ustar();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, data.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn the_base_an_os_bundle_needs() {
        let dir = tempfile::tempdir().unwrap();
        let path = bundle_with(dir.path(), r#"{"component": "os", "base": 2}"#);
        assert_eq!(os_needs_base(&path).unwrap(), Some(2));
        let path = bundle_with(dir.path(), r#"{"component": "os"}"#);
        assert_eq!(os_needs_base(&path).unwrap(), None);
    }

    #[test]
    fn a_bundle_changed_after_its_check_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b");
        std::fs::write(&path, b"signed").unwrap();
        let mut inputs = crate::testing::World::new().inputs;
        inputs.bundles = vec![Bundle {
            path: path.clone(),
            sha256: format!("{:x}", sha2::Sha256::digest(b"signed")),
        }];
        assert!(inputs.unchanged().is_ok());
        std::fs::write(&path, b"swapped").unwrap();
        assert!(inputs.unchanged().is_err());
    }

    #[test]
    fn the_sheet_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let Setup {
            name,
            key,
            install_id,
        } = setup(dir.path()).unwrap();
        assert!(name.starts_with("mowbite-") && name.len() == 12);
        assert_eq!(key.expose().len(), KEY_LEN);
        assert!(key.expose().bytes().all(|b| ALPHABET.contains(&b)));
        let id = install_id.expose();
        assert!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()));
        let again = setup(dir.path()).unwrap();
        assert_eq!(
            (again.name, again.key.expose(), again.install_id.expose()),
            (name, key.expose(), id)
        );
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
        assert_ne!(setup(other.path()).unwrap().key.expose(), key.expose());
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
            .map(|b| b.path.file_name().unwrap().to_str().unwrap())
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
