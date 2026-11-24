//! The checks before anything is written, run on the backup. Each failed technical check is a Stop: the run ends
//! before writing and the backup stays.

use super::fit::{self, Boot};
use super::gpt::{self, Entry};
use super::rootfs::{self, Kind};
use crate::mower::{Finding, Part};
use std::path::Path;

/// What the signed tables say about known devices.
pub struct Known {
    /// Partition tables seen on Trons; the device must match one exactly.
    pub tables: Vec<Vec<Part>>,
    /// Vendor parts the new system cannot run without (paths from /).
    pub required: Vec<String>,
    /// Vendor parts taken along when present.
    pub optional: Vec<String>,
}

#[derive(Default)]
pub struct Checked {
    pub table: Vec<Entry>,
    pub boot: Option<Boot>,
    pub findings: Vec<Finding>,
}

impl Checked {
    fn stop(&mut self, code: &str, details: impl Into<String>) {
        self.findings.push(Finding::Stop {
            code: code.into(),
            details: details.into(),
        });
    }
    fn info(&mut self, code: &str, details: impl Into<String>) {
        self.findings.push(Finding::Info {
            code: code.into(),
            details: details.into(),
        });
    }
    pub fn stopped(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f, Finding::Stop { .. }))
    }
}

/// `start`: the first sectors of the flash (at least gpt::PRIMARY_SECTORS), `boot`: the whole boot partition,
/// `rootfs`: the saved rootfs partition.
pub fn check(start: &[u8], boot: &[u8], rootfs: &Path, known: &Known) -> Checked {
    let mut c = Checked::default();
    check_table(&mut c, start, known);
    match fit::read_boot(boot) {
        Ok(b) => {
            check_boot(&mut c, &b);
            c.boot = Some(b);
        }
        Err(e) => c.stop("tron.boot_unreadable", format!("{e:#}")),
    }
    if let Some(release) = c.boot.as_ref().map(|b| b.kernel_release.clone()) {
        check_rootfs(&mut c, rootfs, &release, known);
    }
    c
}

fn check_table(c: &mut Checked, start: &[u8], known: &Known) {
    match gpt::parse_primary(start) {
        Ok(t) => c.table = t,
        Err(e) => return c.stop("tron.table_unreadable", format!("{e:#}")),
    }
    let parts: Vec<Part> = c.table.iter().map(|e| e.part.clone()).collect();
    if !known.tables.contains(&parts) {
        let seen: Vec<String> = parts
            .iter()
            .map(|p| format!("{} {}-{}", p.name, p.first_sector, p.last_sector))
            .collect();
        c.stop("tron.table_unknown", seen.join(", "));
    }
}

fn check_boot(c: &mut Checked, b: &Boot) {
    if b.signed {
        c.stop(
            "tron.boot_signed",
            "the loader checks signatures; a changed rootfs may not start",
        );
    }
    let args: Vec<&str> = b.bootargs.split_whitespace().collect();
    if args.contains(&"ro") {
        c.stop("tron.root_read_only", b.bootargs.clone());
    }
    if args
        .iter()
        .any(|a| a.contains("verity") || a.starts_with("dm-mod.") || a.starts_with("dm="))
    {
        c.stop("tron.root_verified", b.bootargs.clone());
    }
    // Linux matches a PARTUUID given in part by its start; it must name the rootfs partition and only that.
    let root = args
        .iter()
        .find_map(|a| a.strip_prefix("root=PARTUUID="))
        .map(str::to_ascii_lowercase);
    let hits: Vec<&str> = match &root {
        Some(r) if !r.is_empty() => c
            .table
            .iter()
            .filter(|e| e.uuid.starts_with(r.as_str()))
            .map(|e| e.part.name.as_str())
            .collect(),
        _ => Vec::new(),
    };
    if hits != ["rootfs"] {
        c.stop("tron.root_elsewhere", format!("{} -> {hits:?}", b.bootargs));
    }
}

fn check_rootfs(c: &mut Checked, image: &Path, release: &str, known: &Known) {
    let fs = match rootfs::open(image, 0) {
        Ok(fs) => fs,
        Err(e) => return c.stop("tron.rootfs_unreadable", format!("{e:#}")),
    };
    match rootfs::module_dirs(&fs) {
        Ok(dirs) => {
            if !dirs.iter().any(|d| d == release) {
                c.stop(
                    "tron.modules_missing",
                    format!("no {}/{release}, found {dirs:?}", rootfs::MODULES),
                );
            }
            for other in dirs.iter().filter(|d| *d != release) {
                c.info(
                    "tron.modules_ignored",
                    format!("{}/{other}", rootfs::MODULES),
                );
            }
        }
        Err(e) => c.stop("tron.modules_missing", format!("{e:#}")),
    }
    let want = format!("{release} ");
    for (part, required) in known
        .required
        .iter()
        .map(|p| (p, true))
        .chain(known.optional.iter().map(|p| (p, false)))
    {
        let entries = match rootfs::collect(&fs, part) {
            Ok(e) => e,
            Err(e) if required => {
                c.stop("tron.part_missing", format!("{e:#}"));
                continue;
            }
            Err(_) => {
                c.info("tron.optional_missing", part.clone());
                continue;
            }
        };
        for e in entries
            .iter()
            .filter(|e| e.path.ends_with(".ko") && matches!(e.kind, Kind::File { .. }))
        {
            let magic = rootfs::read(&fs, &e.path)
                .ok()
                .and_then(|d| rootfs::vermagic(&d));
            if !magic.as_deref().is_some_and(|m| m.starts_with(&want)) {
                c.stop(
                    "tron.module_other_kernel",
                    format!("{}: {magic:?}, kernel {release}", e.path),
                );
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tron::fit::tests::{KERNEL, boot_partition};
    use crate::tron::gpt::tests::{ROOTFS_UUID, image};
    use crate::tron::rootfs::tests::{GOOD_VERMAGIC, ext4_from, vendor_tree};

    const BOOTARGS: &str = "console=ttyFIQ0 root=PARTUUID=614e0000-0000 rw rootwait";

    fn table() -> Vec<u8> {
        image(&[
            ("boot", 32768, 557055, [7; 16]),
            ("rootfs", 13664256, 47218687, ROOTFS_UUID),
        ])
    }

    fn known() -> Known {
        Known {
            tables: vec![vec![
                Part {
                    name: "boot".into(),
                    first_sector: 32768,
                    last_sector: 557055,
                },
                Part {
                    name: "rootfs".into(),
                    first_sector: 13664256,
                    last_sector: 47218687,
                },
            ]],
            required: vec!["/usr/lib/modules/5.10.209".into(), "/etc/iqfiles".into()],
            optional: vec!["/usr/lib/kernel/driver".into()],
        }
    }

    fn codes(c: &Checked) -> Vec<String> {
        c.findings
            .iter()
            .map(|f| match f {
                Finding::Stop { code, .. } => format!("stop {code}"),
                Finding::Warn { code, .. } => format!("warn {code}"),
                Finding::Info { code, .. } => format!("info {code}"),
            })
            .collect()
    }

    /// Runs the checks against a rootfs built from vendor_tree.
    fn run(start: &[u8], boot: &[u8], vermagic: &str, known: &Known) -> Checked {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        vendor_tree(&tree, vermagic);
        let img = dir.path().join("rootfs.img");
        ext4_from(&tree, &img);
        check(start, boot, &img, known)
    }

    #[test]
    fn a_device_like_ours_passes() {
        let c = run(
            &table(),
            &boot_partition(KERNEL, BOOTARGS, false),
            GOOD_VERMAGIC,
            &known(),
        );
        assert_eq!(
            codes(&c),
            ["info tron.modules_ignored", "info tron.optional_missing"],
            "{:?}",
            c.findings
        );
        assert!(!c.stopped());
        assert_eq!(c.boot.unwrap().kernel_release, "5.10.209");
    }

    #[test]
    fn each_technical_problem_stops() {
        let boot = boot_partition(KERNEL, BOOTARGS, false);
        let cases: Vec<(&str, Vec<u8>, Vec<u8>, &str)> = vec![
            (
                "tron.table_unknown",
                image(&[("rootfs", 13664256, 47218688, ROOTFS_UUID)]),
                boot.clone(),
                GOOD_VERMAGIC,
            ),
            (
                "tron.boot_signed",
                table(),
                boot_partition(KERNEL, BOOTARGS, true),
                GOOD_VERMAGIC,
            ),
            (
                "tron.root_read_only",
                table(),
                boot_partition(KERNEL, &BOOTARGS.replace(" rw", " ro"), false),
                GOOD_VERMAGIC,
            ),
            (
                "tron.root_verified",
                table(),
                boot_partition(KERNEL, &format!("{BOOTARGS} dm-mod.create=x"), false),
                GOOD_VERMAGIC,
            ),
            (
                "tron.root_elsewhere",
                table(),
                boot_partition(KERNEL, "root=PARTUUID=0707", false),
                GOOD_VERMAGIC,
            ),
            (
                "tron.root_elsewhere",
                table(),
                boot_partition(KERNEL, "root=/dev/mmcblk0p6", false),
                GOOD_VERMAGIC,
            ),
            (
                "tron.root_elsewhere",
                table(),
                boot_partition(KERNEL, "root=PARTUUID=", false),
                GOOD_VERMAGIC,
            ),
            (
                "tron.boot_unreadable",
                table(),
                vec![0; 8192],
                GOOD_VERMAGIC,
            ),
            (
                "tron.module_other_kernel",
                table(),
                boot.clone(),
                "5.10.160 SMP mod_unload aarch64",
            ),
            (
                "tron.module_other_kernel",
                table(),
                boot.clone(),
                "5.10.2090 SMP",
            ),
            (
                "tron.modules_missing",
                table(),
                boot_partition(
                    &KERNEL
                        .to_vec()
                        .into_iter()
                        .map(|b| if b == b'9' { b'8' } else { b })
                        .collect::<Vec<_>>(),
                    BOOTARGS,
                    false,
                ),
                GOOD_VERMAGIC,
            ),
        ];
        for (code, start, boot, vermagic) in cases {
            let c = run(&start, &boot, vermagic, &known());
            assert!(
                codes(&c).contains(&format!("stop {code}")),
                "{code}: {:?}",
                c.findings
            );
        }
        let mut k = known();
        k.required.push("/usr/metoak/metoak/mo_asicrw".into());
        let c = run(&table(), &boot, GOOD_VERMAGIC, &k);
        assert!(
            codes(&c).contains(&"stop tron.part_missing".to_string()),
            "{:?}",
            c.findings
        );
    }
}
