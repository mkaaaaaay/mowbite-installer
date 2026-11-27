//! The release files from the release repo, chosen by its signed index.json (mowbite-index): the newest OS for the
//! platform, the planner and app its bundle.json names as bundled, and the highest base system it fits. The index
//! only chooses and bounds; each file is checked against its own signature afterwards, as from a release folder.

use crate::mower::{Progress, Tables};
use crate::release;
use crate::sign::Signers;
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const RELEASES: &str = "https://github.com/mkaaaaaay/mowbite-releases/releases/download/";
const INDEX: &str = "index/index.json";
const SCHEMA: u64 = 1;
const MAX_INDEX: u64 = 256 << 10;
const MAX_SIG: u64 = 64 << 10;
const MAX_SIDECAR: u64 = 1 << 20;
const MAX_LOADER: u64 = 4 << 20;
const MAX_BUNDLE: u64 = 512 << 20;
const MAX_IMAGE: u64 = 4 << 30;

/// Where the files come from: a path below RELEASES, at most max bytes into out.
pub trait Source {
    fn get(&self, path: &str, max: u64, out: &mut dyn Write, progress: &dyn Progress)
    -> Result<()>;
}

pub struct Https {
    agent: ureq::Agent,
}

impl Https {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .user_agent(concat!("mowbite-installer/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .build();
        Https {
            agent: config.into(),
        }
    }
}

impl Default for Https {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for Https {
    fn get(
        &self,
        path: &str,
        max: u64,
        out: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<()> {
        let url = format!("{RELEASES}{path}");
        let response = self.agent.get(&url).call().with_context(|| url.clone())?;
        let total = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
        if let Some(total) = total {
            ensure!(total <= max, "{url} has {total} bytes, more than {max}");
        }
        let mut reader = response.into_body().into_with_config().limit(max).reader();
        copy(&mut reader, out, total, progress).with_context(|| url.clone())
    }
}

fn copy(
    from: &mut dyn Read,
    out: &mut dyn Write,
    total: Option<u64>,
    progress: &dyn Progress,
) -> Result<()> {
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(total) = total.filter(|&t| t > MAX_SIDECAR) {
            progress.bytes(done, total);
        }
    }
}

#[derive(Deserialize)]
struct Index {
    schema: u64,
    bundles: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
struct Entry {
    component: String,
    revision: String,
    serial: u64,
    size: u64,
    sha256: String,
    file: String,
    url: String,
    #[serde(default)]
    platforms: Vec<String>,
    /// Of a base system: its number.
    #[serde(default)]
    base: Option<u32>,
}

impl Entry {
    /// The release folder below RELEASES the file lies in.
    fn release(&self) -> Result<&str> {
        let (release, file) = self
            .url
            .split_once('/')
            .with_context(|| format!("index.json: {} is no release/file", self.url))?;
        ensure!(
            plain(release) && file == self.file && plain(file),
            "index.json: {} is no release/{}",
            self.url,
            self.file
        );
        Ok(release)
    }
}

/// A name that stays in its folder.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// What the OS bundle needs beside itself, from its bundle.json.
#[derive(Default, Deserialize)]
pub struct Needs {
    pub base: Option<u32>,
    #[serde(default)]
    bundled: std::collections::BTreeMap<String, Bundled>,
}

#[derive(Deserialize)]
struct Bundled {
    revision: String,
}

/// The files of one release for platform into folder; files of an earlier download that are no longer chosen go.
pub fn fetch(
    source: &dyn Source,
    signers: &Signers,
    platform: &str,
    folder: &Path,
    progress: &dyn Progress,
) -> Result<()> {
    std::fs::create_dir_all(folder).with_context(|| folder.display().to_string())?;
    progress.step("release.download");
    let index = small(source, INDEX, MAX_INDEX)?;
    let sig = small(source, &format!("{INDEX}.sig"), MAX_SIG)?;
    signers
        .verify(
            &index,
            &String::from_utf8(sig).context("index.json.sig")?,
            "mowbite-index",
        )
        .context("index.json")?;
    let index: Index = serde_json::from_slice(&index).context("index.json")?;
    ensure!(
        index.schema == SCHEMA,
        "index.json is not of schema {SCHEMA}"
    );
    let mut kept = Vec::new();

    let os = newest(&index, |e| {
        e.component == "os" && e.platforms.iter().any(|p| p == platform)
    })
    .with_context(|| format!("index.json names no MowBite OS for {platform}"))?;
    let os_path = bundle(source, folder, os, progress, &mut kept)?;
    let needs = needs(&os_path)?;
    for component in ["planner", "app"] {
        let revision = &needs
            .bundled
            .get(component)
            .with_context(|| format!("the OS bundle {} names no {component}", os.revision))?
            .revision;
        let entry = index
            .bundles
            .iter()
            .find(|e| e.component == component && &e.revision == revision)
            .with_context(|| {
                format!("index.json has no {component} {revision}, which the OS bundle needs")
            })?;
        bundle(source, folder, entry, progress, &mut kept)?;
    }

    let kind = format!("{platform}-base");
    let least = needs.base.unwrap_or(0);
    let base = index
        .bundles
        .iter()
        .filter(|e| e.component == kind && e.base.is_some_and(|b| b >= least))
        .max_by_key(|e| (e.base, e.serial))
        .with_context(|| {
            format!("index.json has no base system {least} or newer for {platform}")
        })?;
    let release = base.release()?;
    let sidecar = file(
        source,
        folder,
        release,
        &base.file,
        MAX_SIDECAR,
        Some(&base.sha256),
        progress,
        &mut kept,
    )?;
    signed(source, folder, release, &base.file, progress, &mut kept)?;
    let image = sidecar_file(&std::fs::read(&sidecar)?)?;
    file(
        source, folder, release, &image, MAX_IMAGE, None, progress, &mut kept,
    )?;
    let tables = file(
        source,
        folder,
        release,
        "tables.json",
        MAX_SIDECAR,
        None,
        progress,
        &mut kept,
    )?;
    let tables_sig = signed(source, folder, release, "tables.json", progress, &mut kept)?;
    let tables = release::tables(
        &std::fs::read(&tables)?,
        &std::fs::read_to_string(&tables_sig)?,
        signers,
        platform,
    )?;
    let loader = loader(platform, &tables)?;
    file(
        source,
        folder,
        release,
        &loader.0,
        MAX_LOADER,
        Some(&loader.1),
        progress,
        &mut kept,
    )?;
    signed(source, folder, release, &loader.0, progress, &mut kept)?;

    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_file() && !kept.contains(&path) {
            std::fs::remove_file(&path).with_context(|| path.display().to_string())?;
        }
    }
    Ok(())
}

fn newest(index: &Index, pick: impl Fn(&Entry) -> bool) -> Option<&Entry> {
    index
        .bundles
        .iter()
        .filter(|e| pick(e))
        .max_by_key(|e| e.serial)
}

fn small(source: &dyn Source, path: &str, max: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    source.get(path, max, &mut out, &NoProgress)?;
    Ok(out)
}

struct NoProgress;

impl Progress for NoProgress {
    fn step(&self, _: &'static str) {}
    fn bytes(&self, _: u64, _: u64) {}
}

/// A bundle of the index and its .sig; sha256 and size as the index says.
fn bundle(
    source: &dyn Source,
    folder: &Path,
    entry: &Entry,
    progress: &dyn Progress,
    kept: &mut Vec<PathBuf>,
) -> Result<PathBuf> {
    ensure!(
        entry.size <= MAX_BUNDLE,
        "index.json: {} has {} bytes, more than a bundle may",
        entry.file,
        entry.size
    );
    let release = entry.release()?;
    let path = file(
        source,
        folder,
        release,
        &entry.file,
        entry.size,
        Some(&entry.sha256),
        progress,
        kept,
    )?;
    signed(source, folder, release, &entry.file, progress, kept)?;
    Ok(path)
}

fn signed(
    source: &dyn Source,
    folder: &Path,
    release: &str,
    name: &str,
    progress: &dyn Progress,
    kept: &mut Vec<PathBuf>,
) -> Result<PathBuf> {
    file(
        source,
        folder,
        release,
        &format!("{name}.sig"),
        MAX_SIG,
        None,
        progress,
        kept,
    )
}

/// release/name into folder/name, kept when it is there with the sha256 the index names; through name.part, so an
/// interrupted download never counts.
#[allow(clippy::too_many_arguments)]
fn file(
    source: &dyn Source,
    folder: &Path,
    release: &str,
    name: &str,
    max: u64,
    sha256: Option<&str>,
    progress: &dyn Progress,
    kept: &mut Vec<PathBuf>,
) -> Result<PathBuf> {
    ensure!(plain(name), "{name:?} is no plain file name");
    let path = folder.join(name);
    kept.push(path.clone());
    if let Some(want) = sha256
        && path.is_file()
        && crate::store::sha256_file(&path)? == want
    {
        return Ok(path);
    }
    if max > MAX_SIDECAR {
        println!("Downloading {name}");
    }
    let part = folder.join(format!("{name}.part"));
    let mut out = HashingFile {
        file: std::fs::File::create(&part).with_context(|| part.display().to_string())?,
        hash: Sha256::new(),
    };
    source.get(&format!("{release}/{name}"), max, &mut out, progress)?;
    out.file.sync_all()?;
    let got = crate::store::hex(&out.hash.finalize());
    if let Some(want) = sha256
        && got != want
    {
        let _ = std::fs::remove_file(&part);
        bail!("{name} does not have the sha256 index.json names");
    }
    std::fs::rename(&part, &path).with_context(|| path.display().to_string())?;
    Ok(path)
}

struct HashingFile {
    file: std::fs::File,
    hash: Sha256,
}

impl Write for HashingFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// What an OS bundle needs, from its bundle.json.
pub fn needs(bundle: &Path) -> Result<Needs> {
    let file = std::fs::File::open(bundle).with_context(|| bundle.display().to_string())?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.components().count() == 2 && path.file_name() == Some("bundle.json".as_ref()) {
            return serde_json::from_reader(&mut entry)
                .with_context(|| format!("bundle.json of {}", bundle.display()));
        }
    }
    bail!("{} has no bundle.json", bundle.display())
}

/// The image base.json names; base.json itself is checked with its signature later.
fn sidecar_file(sidecar: &[u8]) -> Result<String> {
    #[derive(Deserialize)]
    struct Named {
        file: String,
    }
    let named: Named = serde_json::from_slice(sidecar).context("base.json")?;
    Ok(named.file)
}

/// The loader file and its sha256, from the section of the module.
fn loader(platform: &str, tables: &Tables) -> Result<(String, String)> {
    match platform {
        "tron" => {
            let l = crate::tron::tables::loader(&tables.raw)?;
            Ok((l.file, l.sha256))
        }
        other => bail!("no loader known for {other}"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::signed;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Files {
        files: RefCell<HashMap<String, Vec<u8>>>,
        asked: RefCell<Vec<String>>,
    }

    impl Files {
        fn put(&self, path: &str, data: impl Into<Vec<u8>>) {
            self.files.borrow_mut().insert(path.to_owned(), data.into());
        }
        fn asked(&self, path: &str) -> usize {
            self.asked.borrow().iter().filter(|p| *p == path).count()
        }
    }

    impl Source for Files {
        fn get(&self, path: &str, max: u64, out: &mut dyn Write, _: &dyn Progress) -> Result<()> {
            self.asked.borrow_mut().push(path.to_owned());
            let data = self
                .files
                .borrow()
                .get(path)
                .cloned()
                .with_context(|| format!("404 {path}"))?;
            ensure!(data.len() as u64 <= max, "{path} too large");
            out.write_all(&data)?;
            Ok(())
        }
    }

    fn targz(meta: &str) -> Vec<u8> {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, data) in [("x/install/a", "1"), ("x/bundle.json", meta)] {
            let mut header = tar::Header::new_ustar();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, data.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn sha(data: &[u8]) -> String {
        crate::store::hex(&Sha256::digest(data))
    }

    struct Release {
        files: Files,
        signers: Signers,
        entries: Vec<serde_json::Value>,
        keys: tempfile::TempDir,
    }

    impl Release {
        fn sign(&self, data: &[u8], namespace: &str) -> String {
            signed(self.keys.path(), "k", data, namespace).unwrap().1
        }

        /// A bundle in its release, named in the index.
        fn bundle(
            &mut self,
            component: &str,
            revision: &str,
            serial: u64,
            extra: serde_json::Value,
            data: &[u8],
        ) {
            let file = format!("mowbite-{component}-{revision}.tar.gz");
            let url = format!("{component}-{revision}/{file}");
            self.files.put(&url, data);
            self.files.put(&format!("{url}.sig"), "sig");
            let mut entry = serde_json::json!({"component": component, "revision": revision, "serial": serial,
                "date": serial, "size": data.len(), "sha256": sha(data), "file": file, "url": url});
            entry
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            self.entries.push(entry);
        }

        fn index(&self) {
            let index =
                serde_json::to_vec(&serde_json::json!({"schema": 1, "serial": 9, "date": 9,
                "bundles": self.entries}))
                .unwrap();
            self.files.put("index/index.json", index.clone());
            self.files
                .put("index/index.json.sig", self.sign(&index, "mowbite-index"));
        }
    }

    /// Two OS for the Tron and one for another platform, two planners, an app, two base systems; signed index,
    /// tables, loader and base.json. None without ssh-keygen.
    fn release() -> Option<Release> {
        let keys = tempfile::tempdir().unwrap();
        let (public, _) = signed(keys.path(), "k", b"x", "mowbite-index")?;
        let signers = Signers::parse(&format!(
            "mowbite namespaces=\"mowbite-index,mowbite-tables,mowbite-base\" {public}\n"
        ))
        .unwrap();
        let mut r = Release {
            files: Files::default(),
            signers,
            entries: Vec::new(),
            keys,
        };
        let os = |planner: &str| {
            targz(&format!(
                r#"{{"component": "os", "base": 2, "bundled": {{"planner": {{"revision": "{planner}"}},
                "app": {{"revision": "a1"}}}}}}"#
            ))
        };
        let tron = serde_json::json!({"platforms": ["tron"]});
        r.bundle("os", "o1", 1, tron.clone(), &os("p1"));
        r.bundle("os", "o2", 2, tron, &os("p1"));
        r.bundle(
            "os",
            "o3",
            3,
            serde_json::json!({"platforms": ["yf"]}),
            &os("p2"),
        );
        r.bundle("planner", "p1", 1, serde_json::json!({}), b"planner 1");
        r.bundle("planner", "p2", 2, serde_json::json!({}), b"planner 2");
        r.bundle("app", "a1", 1, serde_json::json!({}), b"app");

        let loader = b"loader".to_vec();
        let mut section: serde_json::Value =
            serde_json::from_slice(crate::tron::tables::SHIPPED).unwrap();
        section["loader"] = serde_json::json!({"file": "loader.bin", "sha256": sha(&loader)});
        let tables =
            serde_json::to_vec(&serde_json::json!({"schema": 1, "tron": section})).unwrap();
        let base = br#"{"file": "base-2.img.zst"}"#.to_vec();
        for (release, number, serial) in [
            ("tron-base-2", 2, 5),
            ("tron-base-1", 1, 9),
            ("tron-base-3", 3, 1),
        ] {
            r.files.put(&format!("{release}/base.json"), base.clone());
            r.files.put(&format!("{release}/base.json.sig"), "sig");
            r.files.put(&format!("{release}/base-2.img.zst"), "image");
            r.files
                .put(&format!("{release}/tables.json"), tables.clone());
            r.files.put(
                &format!("{release}/tables.json.sig"),
                r.sign(&tables, "mowbite-tables"),
            );
            r.files
                .put(&format!("{release}/loader.bin"), loader.clone());
            r.files.put(&format!("{release}/loader.bin.sig"), "sig");
            r.entries.push(serde_json::json!({"component": "tron-base", "revision": release, "serial": serial,
                "size": base.len(), "sha256": sha(&base), "file": "base.json", "url": format!("{release}/base.json"),
                "base": number}));
        }
        r.index();
        Some(r)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_newest_os_of_the_platform_with_what_it_bundles_and_a_base_it_fits() {
        let Some(r) = release() else { return };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mowbite-os-o1.tar.gz"), "from before").unwrap();
        fetch(&r.files, &r.signers, "tron", dir.path(), &NoProgress).unwrap();
        assert_eq!(
            names(dir.path()),
            [
                "base-2.img.zst",
                "base.json",
                "base.json.sig",
                "loader.bin",
                "loader.bin.sig",
                "mowbite-app-a1.tar.gz",
                "mowbite-app-a1.tar.gz.sig",
                "mowbite-os-o2.tar.gz",
                "mowbite-os-o2.tar.gz.sig",
                "mowbite-planner-p1.tar.gz",
                "mowbite-planner-p1.tar.gz.sig",
                "tables.json",
                "tables.json.sig",
            ]
        );
        assert_eq!(r.files.asked("tron-base-3/base.json"), 1);
        assert_eq!(
            r.files.asked("tron-base-1/base.json") + r.files.asked("tron-base-2/base.json"),
            0
        );
        // Again: the bundles with the right sha256 stay, nothing else is left over.
        fetch(&r.files, &r.signers, "tron", dir.path(), &NoProgress).unwrap();
        assert_eq!(r.files.asked("os-o2/mowbite-os-o2.tar.gz"), 1);
        assert_eq!(names(dir.path()).len(), 13);
    }

    #[test]
    fn a_wrong_index_signature_or_file_stops_the_download() {
        let Some(mut r) = release() else { return };
        let dir = tempfile::tempdir().unwrap();
        r.files
            .put("index/index.json.sig", r.sign(b"other", "mowbite-index"));
        assert!(fetch(&r.files, &r.signers, "tron", dir.path(), &NoProgress).is_err());

        r.index();
        r.files.put("app-a1/mowbite-app-a1.tar.gz", "ppa");
        let error = fetch(&r.files, &r.signers, "tron", dir.path(), &NoProgress).unwrap_err();
        assert!(format!("{error:#}").contains("sha256"), "{error:#}");
        assert!(!dir.path().join("mowbite-app-a1.tar.gz").exists());
        assert!(!dir.path().join("mowbite-app-a1.tar.gz.part").exists());

        r.entries.retain(|e| e["component"] != "app");
        r.index();
        let error = fetch(&r.files, &r.signers, "tron", dir.path(), &NoProgress).unwrap_err();
        assert!(format!("{error:#}").contains("no app a1"), "{error:#}");
        assert!(fetch(&r.files, &r.signers, "nx60i", dir.path(), &NoProgress).is_err());
    }

    #[test]
    fn index_urls_stay_in_their_release() {
        let entry = |url: &str, file: &str| Entry {
            component: "os".into(),
            revision: "r".into(),
            serial: 1,
            size: 1,
            sha256: String::new(),
            file: file.into(),
            url: url.into(),
            platforms: vec![],
            base: None,
        };
        assert_eq!(
            entry("os-r/f.tar.gz", "f.tar.gz").release().unwrap(),
            "os-r"
        );
        for (url, file) in [
            ("../f", "f"),
            ("os-r/../f", "f"),
            ("os-r/f", "g"),
            ("f", "f"),
            ("https://x/f", "f"),
            ("/os-r/f", "f"),
            ("os-r/.f", ".f"),
        ] {
            assert!(entry(url, file).release().is_err(), "{url}");
        }
    }
}
