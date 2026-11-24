//! The checks before anything is written, run on the backup. Each failed technical check is a Stop: the run ends
//! before writing and the backup stays.

use super::fit::{self, Boot};
use super::gpt::{self, Entry};
use super::identity;
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
    /// Vendor versions (folder names under /userdata/RobotData/upgrade) the parts were checked on.
    pub vendor_versions: Vec<String>,
}

impl Known {
    /// Required and optional parts together.
    pub fn parts(&self) -> Vec<String> {
        self.required
            .iter()
            .chain(&self.optional)
            .cloned()
            .collect()
    }
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
    fn warn(&mut self, code: &str, details: impl Into<String>) {
        self.findings.push(Finding::Warn {
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
    if fs.symlink_metadata("/workspace").is_err() {
        return c.stop("tron.no_vendor_system", "no /workspace");
    }
    match identity::vendor_version(&fs) {
        Some(v) if known.vendor_versions.contains(&v) => {}
        other => c.warn("tron.vendor_unknown", other.unwrap_or_default()),
    }
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
    let parts = known.parts();
    for (part, required) in known
        .required
        .iter()
        .map(|p| (p, true))
        .chain(known.optional.iter().map(|p| (p, false)))
    {
        let entries = match rootfs::take(&fs, part, &parts) {
            Ok(e) => e,
            Err(e) if rootfs::is_outside_link(&e) => {
                if required {
                    c.stop("tron.part_link", format!("{e:#}"));
                } else {
                    c.info("tron.optional_left_out", format!("{e:#}"));
                }
                continue;
            }
            Err(e) if rootfs::is_unwritten(&e) => {
                if required {
                    c.stop("tron.part_unwritten", format!("{e:#}"));
                } else {
                    c.info("tron.optional_unwritten", format!("{e:#}"));
                }
                continue;
            }
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
    use crate::tron::rootfs::tests::{GOOD_VERMAGIC, ext4_from, fallocate, vendor_tree};

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
            vendor_versions: vec!["v1.1.1".into()],
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
    fn an_unknown_vendor_version_only_warns() {
        let mut k = known();
        k.vendor_versions = vec!["v1.3.32-tron.rc.8".into()];
        let c = run(
            &table(),
            &boot_partition(KERNEL, BOOTARGS, false),
            GOOD_VERMAGIC,
            &k,
        );
        assert!(!c.stopped(), "{:?}", c.findings);
        assert!(c.findings.contains(&Finding::Warn {
            code: "tron.vendor_unknown".into(),
            details: "v1.1.1".into()
        }));
    }

    #[test]
    fn unwritten_blocks_in_a_part_stop_only_when_required() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        vendor_tree(&tree, GOOD_VERMAGIC);
        let img = dir.path().join("rootfs.img");
        ext4_from(&tree, &img);
        fallocate(&img, "/etc/iqfiles/a.json", 7);
        let boot = boot_partition(KERNEL, BOOTARGS, false);
        let c = check(&table(), &boot, &img, &known());
        assert!(
            codes(&c).contains(&"stop tron.part_unwritten".to_string()),
            "{:?}",
            c.findings
        );
        let mut k = known();
        k.required.retain(|p| p != "/etc/iqfiles");
        k.optional.push("/etc/iqfiles".into());
        let c = check(&table(), &boot, &img, &k);
        assert!(!c.stopped(), "{:?}", c.findings);
        assert!(codes(&c).contains(&"info tron.optional_unwritten".to_string()));
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

    fn run_tree(tree_fix: impl FnOnce(&Path), known: &Known) -> Checked {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        vendor_tree(&tree, GOOD_VERMAGIC);
        tree_fix(&tree);
        let img = dir.path().join("rootfs.img");
        ext4_from(&tree, &img);
        check(
            &table(),
            &boot_partition(KERNEL, BOOTARGS, false),
            &img,
            known,
        )
    }

    #[test]
    fn a_system_without_workspace_is_no_vendor_system() {
        let c = run_tree(
            |t| std::fs::remove_file(t.join("workspace")).unwrap(),
            &known(),
        );
        assert!(
            codes(&c).contains(&"stop tron.no_vendor_system".to_string()),
            "{:?}",
            c.findings
        );
    }

    #[test]
    fn links_out_of_the_vendor_parts_stop_or_leave_the_part_out() {
        let outside = |t: &Path| {
            std::os::unix::fs::symlink("../../../../root", t.join("usr/lib/modules/5.10.209/build"))
                .unwrap()
        };
        assert!(codes(&run_tree(outside, &known())).contains(&"stop tron.part_link".to_string()));
        let absolute = |t: &Path| {
            std::fs::create_dir_all(t.join("usr/lib/kernel/driver")).unwrap();
            std::os::unix::fs::symlink("/etc/shadow", t.join("usr/lib/kernel/driver/x")).unwrap()
        };
        let c = run_tree(absolute, &known());
        assert!(!c.stopped(), "{:?}", c.findings);
        assert!(codes(&c).contains(&"info tron.optional_left_out".to_string()));
    }
}
