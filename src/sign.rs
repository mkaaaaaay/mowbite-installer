//! Signatures of ssh-keygen -Y sign on what the installer loads (base system, sidecars, bundles, tables), checked
//! against the keys built into the program: the same allowed_signers as on the mower.

use anyhow::{Context, Result, bail};
use ssh_key::{PublicKey, SshSig};

const BUILTIN: &str = include_str!("../keys/allowed_signers");

/// Larger files are signed through a sidecar that names their sha256.
pub const MAX_SIGNED: usize = 256 << 20;

struct Signer {
    key: PublicKey,
    namespaces: Vec<String>,
}

pub struct Signers(Vec<Signer>);

impl Signers {
    pub fn builtin() -> Self {
        Self::parse(BUILTIN).expect("the built-in allowed_signers parse")
    }

    /// Lines "principal namespaces=\"a,b\" ssh-ed25519 AAAA..."; a line without namespaces would allow every one
    /// and is refused, as is any other option.
    pub fn parse(text: &str) -> Result<Self> {
        let mut signers = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let (_principal, options, kind, data) =
                match (fields.next(), fields.next(), fields.next(), fields.next()) {
                    (Some(p), Some(o), Some(k), Some(d)) => (p, o, k, d),
                    _ => bail!(
                        "allowed_signers line {}: principal, namespaces, key expected",
                        n + 1
                    ),
                };
            let Some(list) = options
                .strip_prefix("namespaces=\"")
                .and_then(|o| o.strip_suffix('"'))
            else {
                bail!(
                    "allowed_signers line {}: only namespaces=\"...\" is allowed as option",
                    n + 1
                );
            };
            let key = PublicKey::from_openssh(&format!("{kind} {data}"))
                .with_context(|| format!("allowed_signers line {}", n + 1))?;
            let namespaces = list.split(',').map(str::to_owned).collect();
            signers.push(Signer { key, namespaces });
        }
        if signers.is_empty() {
            bail!("allowed_signers holds no key");
        }
        Ok(Signers(signers))
    }

    /// Ok when one key allowed for namespace signed data in namespace.
    pub fn verify(&self, data: &[u8], signature: &str, namespace: &str) -> Result<()> {
        if data.len() > MAX_SIGNED {
            bail!(
                "{} bytes are too many to check at once; sign a sidecar with their sha256",
                data.len()
            );
        }
        let sig = SshSig::from_pem(signature).context("the signature cannot be read")?;
        for signer in &self.0 {
            if signer.namespaces.iter().any(|n| n == namespace)
                && signer.key.key_data() == sig.public_key()
            {
                return signer
                    .key
                    .verify(namespace, data, &sig)
                    .context("the signature does not match");
            }
        }
        bail!("no key allowed for {namespace} made this signature")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    // A key of its own and a signature of data, made by ssh-keygen; None where ssh-keygen is missing.
    fn signed(
        dir: &std::path::Path,
        name: &str,
        data: &[u8],
        namespace: &str,
    ) -> Option<(String, String)> {
        let key = dir.join(name);
        if !key.exists() {
            let made = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
                .arg(&key)
                .status()
                .ok()?;
            assert!(made.success());
        }
        let file = dir.join(format!("{name}-{namespace}.data"));
        std::fs::write(&file, data).unwrap();
        let _ = std::fs::remove_file(file.with_extension("data.sig"));
        let done = Command::new("ssh-keygen")
            .args(["-q", "-Y", "sign", "-n", namespace, "-f"])
            .arg(&key)
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
        let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        let sig = std::fs::read_to_string(file.with_extension("data.sig")).unwrap();
        Some((
            public
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" "),
            sig,
        ))
    }

    #[test]
    fn the_built_in_keys_parse() {
        assert_eq!(Signers::builtin().0.len(), 2);
    }

    #[test]
    fn only_an_allowed_key_in_its_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let Some((public, sig)) = signed(dir.path(), "a", b"base", "mowbite-base") else {
            eprintln!("no ssh-keygen: skipped");
            return;
        };
        let signers = Signers::parse(&format!(
            "mowbite namespaces=\"mowbite-base,mowbite-index\" {public}\n"
        ))
        .unwrap();
        signers.verify(b"base", &sig, "mowbite-base").unwrap();
        assert!(signers.verify(b"bass", &sig, "mowbite-base").is_err());
        assert!(signers.verify(b"base", &sig, "mowbite-index").is_err());
        let (_, other_ns) = signed(dir.path(), "a", b"base", "mowbite-os").unwrap();
        assert!(signers.verify(b"base", &other_ns, "mowbite-os").is_err());
        let (_, stranger) = signed(dir.path(), "b", b"base", "mowbite-base").unwrap();
        assert!(signers.verify(b"base", &stranger, "mowbite-base").is_err());
        assert!(
            Signers::builtin()
                .verify(b"base", &sig, "mowbite-base")
                .is_err()
        );
    }

    #[test]
    fn lines_without_namespaces_or_with_other_options_are_refused() {
        let line = BUILTIN.lines().find(|l| l.contains("ssh-ed25519")).unwrap();
        let key = line
            .split_whitespace()
            .skip(2)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(Signers::parse(&format!("p {key}")).is_err());
        assert!(Signers::parse(&format!("p cert-authority {key}")).is_err());
        assert!(Signers::parse(&format!("p namespaces=\"x\" {key}")).is_ok());
        assert!(Signers::parse("# only a comment\n").is_err());
        assert!(Signers::parse("p namespaces=\"x\" ssh-ed25519 notbase64").is_err());
    }
}
