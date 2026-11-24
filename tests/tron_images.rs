//! The Tron checks against real backups, which cannot be shipped. Run with
//! TRON_START=<flash image or its first sectors> TRON_BOOT=<boot partition> TRON_ROOTFS=<rootfs partition>
//! [TRON_LISTING=<expected listing of the vendor parts>] [TRON_META=<partition userdata>] cargo test --test tron_images -- --ignored

use mowbite_installer::tron::checks::check;
use mowbite_installer::tron::gpt::{PRIMARY_SECTORS, SECTOR};
use mowbite_installer::tron::{identity, rootfs, tables};
use std::io::Read;
use std::path::PathBuf;

fn env(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("{name} not set"))
        .into()
}

#[test]
#[ignore = "needs backups of a Tron"]
fn a_real_tron_backup_passes() {
    let mut start = vec![0u8; PRIMARY_SECTORS * SECTOR];
    std::fs::File::open(env("TRON_START"))
        .unwrap()
        .read_exact(&mut start)
        .unwrap();
    let boot = std::fs::read(env("TRON_BOOT")).unwrap();
    let known = tables::parse(tables::SHIPPED).unwrap();
    let c = check(&start, &boot, &env("TRON_ROOTFS"), &known);
    for f in &c.findings {
        eprintln!("{f:?}");
    }
    assert!(!c.stopped());
    let b = c.boot.unwrap();
    eprintln!("{}\n{}", b.kernel_banner, b.bootargs);

    let fs = rootfs::open(&env("TRON_ROOTFS"), 0).unwrap();
    eprintln!("vendor version {:?}", identity::vendor_version(&fs));
    if let Some(meta) = std::env::var_os("TRON_META") {
        let serial = identity::serial(&rootfs::open(meta.as_ref(), 0).unwrap()).unwrap();
        assert!(serial.starts_with("sn:"));
        eprintln!("serial hash {}", identity::serial_hash(&serial));
    }

    if let Some(listing) = std::env::var_os("TRON_LISTING") {
        let mut got: Vec<String> = Vec::new();
        for part in known.required.iter().chain(&known.optional) {
            got.extend(
                rootfs::collect(&fs, part)
                    .unwrap()
                    .iter()
                    .map(rootfs::Entry::line),
            );
        }
        got.sort();
        let mut want: Vec<String> = std::fs::read_to_string(listing)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        want.sort();
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g, w);
        }
    }
}
