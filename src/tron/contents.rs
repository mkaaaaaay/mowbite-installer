//! What goes into the pack of one Tron: the vendor parts from its backup, its name, the home Wi-Fi, the setup
//! access point and the signed bundles for the update inbox. Paths are relative to / on the Tron.

use super::checks::Known;
use super::pack::Archive;
use super::rootfs;
use crate::mower::Inputs;
use anyhow::{Context, Result, ensure};
use ext4_view::Ext4;
use sha2::{Digest, Sha256};

pub const CONNECTIONS: &str = "etc/NetworkManager/system-connections";
pub const INBOX: &str = "userdata/mowbite-update/inbox";
/// Random value of this install, 32 hex digits.
pub const INSTALL_ID: &str = "etc/mowbite/install-id";
/// Interface of the setup access point; wlan0 stays for the home Wi-Fi.
pub const SETUP_IFACE: &str = "wlan1";

/// An SSID as the byte list NetworkManager keyfiles accept for any name ("72;101;105;109;").
fn ssid(name: &str) -> String {
    name.bytes().map(|b| format!("{b};")).collect()
}

/// A WPA key as a keyfile string: a passphrase of 8 to 63 printable ASCII characters (spaces and backslashes
/// escaped) or the 64 hex digits of the key itself, as the standard has it. The frame checks typed keys with it too.
pub fn psk(key: &str) -> Result<String> {
    let phrase = (8..=63).contains(&key.len()) && key.bytes().all(|b| (0x20..0x7f).contains(&b));
    let raw = key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit());
    ensure!(phrase || raw, "tron.wifi_key_bad");
    Ok(key.replace('\\', "\\\\").replace(' ', "\\s"))
}

fn home_wifi(id: &str, name: &str, key: &str, hidden: bool) -> Result<String> {
    Ok(format!(
        "[connection]\nid={id}\ntype=wifi\ninterface-name=wlan0\nautoconnect=true\n\n\
         [wifi]\nmode=infrastructure\nssid={}\n{}\n\
         [wifi-security]\nkey-mgmt=wpa-psk\npsk={}\n\n\
         [ipv4]\nmethod=auto\n\n[ipv6]\nmethod=auto\n",
        ssid(name),
        if hidden { "hidden=true\n" } else { "" },
        psk(key)?
    ))
}

/// The access point for setup; it does not start by itself, the base system raises it when wlan0 has no network.
fn setup_ap(name: &str, key: &str) -> Result<String> {
    Ok(format!(
        "[connection]\nid=mowbite-setup\ntype=wifi\ninterface-name={SETUP_IFACE}\nautoconnect=false\n\n\
         [wifi]\nmode=ap\nband=bg\nssid={}\n\n\
         [wifi-security]\nkey-mgmt=wpa-psk\nproto=rsn\npairwise=ccmp\ngroup=ccmp\npsk={}\n\n\
         [ipv4]\nmethod=shared\n\n[ipv6]\nmethod=disabled\n",
        ssid(name),
        psk(key)?
    ))
}

fn hostname(name: &str) -> Result<String> {
    ensure!(
        (1..=63).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-'),
        "the device name {name:?} is no host name"
    );
    Ok(format!("{name}\n"))
}

/// The archive for one Tron. `vendor` is its saved rootfs, `mtime` the time stamp of every entry.
pub fn build(vendor: &Ext4, known: &Known, inputs: &Inputs, mtime: u64) -> Result<Vec<u8>> {
    let mut a = Archive::new(mtime);
    let parts = known.parts();
    for part in &known.required {
        a.vendor_part(vendor, &rootfs::take(vendor, part, &parts)?)?;
    }
    // As check: an optional part that cannot be taken whole stays out.
    for part in &known.optional {
        if let Ok(entries) = rootfs::take(vendor, part, &parts) {
            a.vendor_part(vendor, &entries)?;
        }
    }
    a.file(
        "etc/hostname",
        0o644,
        (0, 0),
        hostname(&inputs.device_name)?.as_bytes(),
    )?;
    // The host proves its first-start answers with it, so the PC knows it reached this Tron.
    let id = inputs.install_id.expose();
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "the install id has 32 hex digits"
    );
    a.file(INSTALL_ID, 0o600, (0, 0), format!("{id}\n").as_bytes())?;
    let tag = &format!("{:x}", Sha256::digest(inputs.wifi_name.as_bytes()))[..8];
    let id = format!("mowbite-wifi-{tag}");
    a.file(
        &format!("{CONNECTIONS}/{id}.nmconnection"),
        0o600,
        (0, 0),
        home_wifi(
            &id,
            &inputs.wifi_name,
            inputs.wifi_key.expose(),
            inputs.wifi_hidden,
        )?
        .as_bytes(),
    )?;
    a.file(
        &format!("{CONNECTIONS}/mowbite-setup.nmconnection"),
        0o600,
        (0, 0),
        setup_ap(&inputs.device_name, inputs.setup_wifi_key.expose())?.as_bytes(),
    )?;
    for bundle in inputs.bundles.iter().map(|b| &b.path) {
        let name = bundle
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("bundle without a plain name: {}", bundle.display()))?;
        let data = std::fs::read(bundle).with_context(|| format!("read {}", bundle.display()))?;
        a.file(&format!("{INBOX}/{name}"), 0o644, (0, 0), &data)?;
    }
    Ok(a.finish())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::secret::Secret;
    use crate::tron::rootfs::tests::{GOOD_VERMAGIC, ext4_from, vendor_tree};
    use std::process::Command;

    fn inputs(dir: &std::path::Path) -> Inputs {
        let bundle = dir.join("os-1.bundle");
        std::fs::write(&bundle, b"signed").unwrap();
        Inputs {
            wifi_name: "Heim; Netz".into(),
            wifi_key: Secret::new("ge heim\\123".into()),
            wifi_hidden: false,
            device_name: "mowbite-ab12".into(),
            setup_wifi_key: Secret::new("setup-key-1".into()),
            install_id: Secret::new("0123456789abcdef0123456789abcdef".into()),
            bundles: vec![crate::mower::Bundle {
                path: bundle,
                sha256: String::new(),
            }],
        }
    }

    #[test]
    fn the_pack_holds_parts_name_wifi_and_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        vendor_tree(&tree, GOOD_VERMAGIC);
        let img = dir.path().join("rootfs.img");
        ext4_from(&tree, &img);
        let fs = crate::tron::rootfs::open(&img, 0).unwrap();
        let known = Known {
            tables: vec![],
            required: vec!["/usr/lib/modules/5.10.209".into()],
            optional: vec!["/etc/iqfiles".into(), "/not/there".into()],
            vendor_versions: vec![],
        };
        let archive = build(&fs, &known, &inputs(dir.path()), 0).unwrap();
        let tar = dir.path().join("a.tar");
        std::fs::write(&tar, &archive).unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let ok = Command::new("tar")
            .args(["-xf"])
            .arg(&tar)
            .arg("-C")
            .arg(&out)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        assert!(out.join("usr/lib/modules/5.10.209/bcmdhd.ko").exists());
        assert!(out.join("etc/iqfiles/a.json").exists());
        assert_eq!(
            std::fs::read_to_string(out.join("etc/hostname")).unwrap(),
            "mowbite-ab12\n"
        );
        assert_eq!(
            std::fs::read(out.join(INBOX).join("os-1.bundle")).unwrap(),
            b"signed"
        );
        let wifi: Vec<_> = std::fs::read_dir(out.join(CONNECTIONS))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(wifi.len(), 2);
        let home = wifi
            .iter()
            .find(|n| n.starts_with("mowbite-wifi-"))
            .unwrap();
        assert_eq!(
            home.len(),
            "mowbite-wifi-".len() + 8 + ".nmconnection".len()
        );
        let text = std::fs::read_to_string(out.join(CONNECTIONS).join(home)).unwrap();
        assert!(text.contains(&format!("ssid={}\n", ssid("Heim; Netz"))));
        assert!(text.contains("psk=ge\\sheim\\\\123\n"));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join(CONNECTIONS).join(home))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let id = std::fs::read_to_string(out.join(INSTALL_ID)).unwrap();
        assert!(
            id.len() == 33 && id.ends_with('\n') && id[..32].bytes().all(|b| b.is_ascii_hexdigit())
        );
        let mode = std::fs::metadata(out.join(INSTALL_ID))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn an_optional_part_that_check_leaves_out_stays_out() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        vendor_tree(&tree, GOOD_VERMAGIC);
        std::os::unix::fs::symlink("/etc/shadow", tree.join("etc/iqfiles/x")).unwrap();
        let img = dir.path().join("rootfs.img");
        ext4_from(&tree, &img);
        let fs = crate::tron::rootfs::open(&img, 0).unwrap();
        let known = Known {
            tables: vec![],
            required: vec!["/usr/lib/modules/5.10.209".into()],
            optional: vec!["/etc/iqfiles".into()],
            vendor_versions: vec![],
        };
        let archive = build(&fs, &known, &inputs(dir.path()), 0).unwrap();
        assert!(
            !archive
                .windows(b"etc/iqfiles".len())
                .any(|w| w == b"etc/iqfiles")
        );
    }

    #[test]
    fn bad_names_and_keys_are_refused() {
        assert!(psk("short").is_err());
        assert!(psk("schlüssel123").is_err());
        assert!(psk(&"a1".repeat(32)).is_ok());
        assert!(psk(&"g1".repeat(32)).is_err());
        let shown = home_wifi("id", "Heim", "geheim123", false).unwrap();
        assert!(!shown.contains("hidden"));
        let hidden = home_wifi("id", "Heim", "geheim123", true).unwrap();
        assert!(hidden.contains(
            "[wifi]\nmode=infrastructure\nssid=72;101;105;109;\nhidden=true\n\n[wifi-security]"
        ));
        assert!(psk("tab\tinside1").is_err());
        assert!(psk(&"x".repeat(64)).is_err());
        assert!(hostname("MowBite").is_err());
        assert!(hostname("-a").is_err());
        assert!(hostname("a b").is_err());
        assert_eq!(ssid("Ab"), "65;98;");
    }
}
