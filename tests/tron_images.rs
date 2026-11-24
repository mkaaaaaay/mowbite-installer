//! The Tron checks against real backups, which cannot be shipped. Run with
//! TRON_START=<flash image or its first sectors> TRON_BOOT=<boot partition> TRON_ROOTFS=<rootfs partition>
//! [TRON_LISTING=<expected listing of the vendor parts>] cargo test --test tron_images -- --ignored

use mowbite_installer::mower::Part;
use mowbite_installer::tron::checks::{Known, check};
use mowbite_installer::tron::gpt::{PRIMARY_SECTORS, SECTOR};
use mowbite_installer::tron::rootfs;
use std::io::Read;
use std::path::PathBuf;

fn env(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("{name} not set"))
        .into()
}

/// The partition table measured on the Tron (eMMC 122159104 sectors).
fn tron_table() -> Vec<Part> {
    [
        ("uboot", 16384, 24575),
        ("misc", 24576, 32767),
        ("boot", 32768, 557055),
        ("recovery", 557056, 1081343),
        ("backup", 1081344, 13664255),
        ("rootfs", 13664256, 47218687),
        ("oem", 47218688, 114327551),
        ("userdata", 114327552, 114589695),
    ]
    .into_iter()
    .map(|(n, a, b)| Part {
        name: n.into(),
        first_sector: a,
        last_sector: b,
    })
    .collect()
}

const REQUIRED: [&str; 4] = [
    "/usr/lib/modules/5.10.209",
    "/usr/lib/kernel/driver",
    "/vendor/etc/firmware",
    "/usr/metoak/metoak/mo_asicrw",
];
const OPTIONAL: [&str; 5] = [
    "/etc/iqfiles",
    "/usr/lib/librkaiq.so",
    "/usr/lib/aarch64-linux-gnu/librga.so",
    "/usr/lib/aarch64-linux-gnu/librga.so.2",
    "/usr/lib/aarch64-linux-gnu/librga.so.2.1.0",
];

#[test]
#[ignore = "needs backups of a Tron"]
fn a_real_tron_backup_passes() {
    let mut start = vec![0u8; PRIMARY_SECTORS * SECTOR];
    std::fs::File::open(env("TRON_START"))
        .unwrap()
        .read_exact(&mut start)
        .unwrap();
    let boot = std::fs::read(env("TRON_BOOT")).unwrap();
    let known = Known {
        tables: vec![tron_table()],
        required: REQUIRED.map(String::from).to_vec(),
        optional: OPTIONAL.map(String::from).to_vec(),
    };
    let c = check(&start, &boot, &env("TRON_ROOTFS"), &known);
    for f in &c.findings {
        eprintln!("{f:?}");
    }
    assert!(!c.stopped());
    let b = c.boot.unwrap();
    eprintln!("{}\n{}", b.kernel_banner, b.bootargs);

    if let Some(listing) = std::env::var_os("TRON_LISTING") {
        let fs = rootfs::open(&env("TRON_ROOTFS"), 0).unwrap();
        let mut got: Vec<String> = Vec::new();
        for part in REQUIRED.iter().chain(&OPTIONAL) {
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
