//! What the installer loads from the releases, each checked against the built-in keys before use: tables.json
//! (mowbite-tables) for the modules, and base.json (mowbite-base), the sidecar that names size and sha256 of the base
//! system and where its pack file lies.

use crate::mower::{BaseImage, Tables};
use crate::sign::Signers;
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

const SCHEMA: u64 = 1;
/// The pack file of the base system starts on a block of the file system.
const PACK_ALIGN: u64 = 4096;

/// The section of module from a signed tables.json; the module reads its own format.
pub fn tables(json: &[u8], sig: &str, signers: &Signers, module: &str) -> Result<Tables> {
    signers
        .verify(json, sig, "mowbite-tables")
        .context("tables.json")?;
    let all: Value = serde_json::from_slice(json).context("tables.json")?;
    ensure!(
        all.get("schema").and_then(Value::as_u64) == Some(SCHEMA),
        "tables.json is not of schema {SCHEMA}"
    );
    let Some(section) = all.get(module) else {
        bail!("tables.json has nothing for {module}");
    };
    Ok(Tables {
        raw: serde_json::to_vec(section)?,
    })
}

/// Largest loader the installer takes; the one of the Tron has about 450 KiB.
const MAX_LOADER: usize = 4 << 20;

/// A loader of the SoC vendor from the release, signed like the tables; the module checks its sha256 against them.
pub fn loader(bytes: &[u8], sig: &str, signers: &Signers) -> Result<Vec<u8>> {
    ensure!(
        bytes.len() <= MAX_LOADER,
        "the loader has {} bytes, too many for a loader",
        bytes.len()
    );
    signers
        .verify(bytes, sig, "mowbite-tables")
        .context("loader")?;
    Ok(bytes.to_vec())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sidecar {
    schema: u64,
    platform: String,
    base: u32,
    file: String,
    size: u64,
    sha256: String,
    pack_offset: u64,
    pack_len: u64,
}

/// The base system at image, once its signed sidecar names this platform, this file, its size and its sha256.
pub fn base(
    sidecar: &[u8],
    sig: &str,
    signers: &Signers,
    image: &Path,
    platform: &str,
) -> Result<(BaseImage, u32)> {
    signers
        .verify(sidecar, sig, "mowbite-base")
        .context("base.json")?;
    let s: Sidecar = serde_json::from_slice(sidecar).context("base.json")?;
    ensure!(s.schema == SCHEMA, "base.json is not of schema {SCHEMA}");
    ensure!(
        s.platform == platform,
        "the base system is for {}, not for {platform}",
        s.platform
    );
    let name = image
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    ensure!(s.file == name, "base.json names {}, not {name}", s.file);
    ensure!(
        s.pack_len > 0
            && s.pack_offset % PACK_ALIGN == 0
            && s.pack_offset
                .checked_add(s.pack_len)
                .is_some_and(|end| end <= s.size),
        "the pack file of base.json does not lie inside the image"
    );
    let mut file = std::fs::File::open(image).with_context(|| image.display().to_string())?;
    let size = file.metadata()?.len();
    ensure!(
        size == s.size,
        "{name} has {size} bytes, base.json says {}",
        s.size
    );
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    ensure!(
        got == s.sha256,
        "{name} is not the signed base system (sha256 {got})"
    );
    let image = BaseImage {
        path: image.to_path_buf(),
        sha256: got,
        pack_offset: s.pack_offset,
        pack_len: s.pack_len,
    };
    Ok((image, s.base))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::signed;
    use serde_json::json;

    fn signers(public: &str) -> Signers {
        Signers::parse(&format!(
            "mowbite namespaces=\"mowbite-base,mowbite-tables\" {public}\n"
        ))
        .unwrap()
    }

    #[test]
    fn tables_give_the_section_of_the_module() {
        let dir = tempfile::tempdir().unwrap();
        let text = json!({"schema": 1, "tron": {"parts": ["a"]}}).to_string();
        let Some((public, sig)) = signed(dir.path(), "k", text.as_bytes(), "mowbite-tables") else {
            return;
        };
        let keys = signers(&public);
        let t = tables(text.as_bytes(), &sig, &keys, "tron").unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&t.raw).unwrap(),
            json!({"parts": ["a"]})
        );
        assert!(tables(text.as_bytes(), &sig, &keys, "nx60i").is_err());
        assert!(tables(text.replace('a', "b").as_bytes(), &sig, &keys, "tron").is_err());
        let (_, base_sig) = signed(dir.path(), "k", text.as_bytes(), "mowbite-base").unwrap();
        assert!(tables(text.as_bytes(), &base_sig, &keys, "tron").is_err());
        let old = json!({"schema": 2, "tron": {}}).to_string();
        let (_, old_sig) = signed(dir.path(), "k", old.as_bytes(), "mowbite-tables").unwrap();
        assert!(tables(old.as_bytes(), &old_sig, &keys, "tron").is_err());
    }

    #[test]
    fn the_base_system_has_to_be_the_signed_one() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("base-9.img");
        let data = vec![7u8; 3 * 4096];
        std::fs::write(&image, &data).unwrap();
        let sha: String = Sha256::digest(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let sidecar = |changes: Value| {
            let mut s = json!({"schema": 1, "platform": "tron", "base": 2, "file": "base-9.img", "size": data.len(),
                "sha256": sha, "pack_offset": 4096, "pack_len": 4096});
            for (k, v) in changes.as_object().unwrap() {
                s[k] = v.clone();
            }
            s.to_string()
        };
        let good = sidecar(json!({}));
        let Some((public, sig)) = signed(dir.path(), "k", good.as_bytes(), "mowbite-base") else {
            return;
        };
        let keys = signers(&public);
        let (got, number) = base(good.as_bytes(), &sig, &keys, &image, "tron").unwrap();
        assert_eq!(
            (got.pack_offset, got.pack_len, got.sha256.as_str(), number),
            (4096, 4096, sha.as_str(), 2)
        );
        assert!(base(good.as_bytes(), &sig, &keys, &image, "nx60i").is_err());
        for bad in [
            json!({"sha256": "0".repeat(64)}),
            json!({"size": 4096}),
            json!({"file": "other.img"}),
            json!({"pack_offset": 100}),
            json!({"pack_offset": 8192, "pack_len": 8192}),
            json!({"pack_len": 0}),
            json!({"extra": 1}),
        ] {
            let text = sidecar(bad.clone());
            let (_, s) = signed(dir.path(), "k", text.as_bytes(), "mowbite-base").unwrap();
            assert!(
                base(text.as_bytes(), &s, &keys, &image, "tron").is_err(),
                "{bad}"
            );
        }
        std::fs::write(&image, vec![8u8; 3 * 4096]).unwrap();
        assert!(base(good.as_bytes(), &sig, &keys, &image, "tron").is_err());
    }

    #[test]
    fn a_loader_has_to_be_signed_and_small() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![1u8; 1000];
        let Some((public, sig)) = signed(dir.path(), "k", &bytes, "mowbite-tables") else {
            return;
        };
        let keys = signers(&public);
        assert_eq!(loader(&bytes, &sig, &keys).unwrap(), bytes);
        assert!(loader(&[2u8; 1000], &sig, &keys).is_err());
        let big = vec![0u8; MAX_LOADER + 1];
        assert!(loader(&big, &sig, &keys).is_err());
    }
}
