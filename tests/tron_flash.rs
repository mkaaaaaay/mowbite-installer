//! Open, back up and check a whole saved Tron flash, opened read only, as the installer does over USB. Run with
//! TRON_FLASH=<flash image> TRON_OS_BUNDLE=<mowbite-os-….tar.gz of the release>
//! TRON_STORE=<empty folder for the backup> TRON_WORK=<work folder>
//! cargo test --release --test tron_flash -- --ignored --nocapture

use mowbite_installer::mower::{
    Bundle, Cancel, Ctx, Finding, Inputs, Log, Mower, Progress, Tables, WorkDir,
};
use mowbite_installer::secret::Secret;
use mowbite_installer::store::Store;
use mowbite_installer::tron::module::Tron;
use mowbite_installer::tron::port::{FilePort, Port};
use mowbite_installer::tron::tables;
use std::cell::Cell;
use std::path::PathBuf;
use std::time::Instant;

struct Print {
    start: Instant,
    last: Cell<u64>,
}

impl Progress for Print {
    fn step(&self, code: &'static str) {
        eprintln!("step {code}");
    }
    fn bytes(&self, done: u64, total: u64) {
        let pct = done * 100 / total.max(1);
        if pct >= self.last.get() + 5 {
            self.last.set(pct);
            eprintln!("{pct} % after {} s", self.start.elapsed().as_secs());
        }
    }
}

impl Log for Print {
    fn info(&self, code: &'static str, details: &str) {
        eprintln!("log {code} {details}");
    }
}

/// The release's MowBite OS bundle (TRON_OS_BUNDLE): the checks read its board firmware lists.
fn os_bundle() -> Inputs {
    Inputs {
        wifi_name: String::new(),
        wifi_key: Secret::new(String::new()),
        device_name: String::new(),
        setup_wifi_key: Secret::new(String::new()),
        install_id: Secret::new(String::new()),
        bundles: vec![Bundle {
            path: env("TRON_OS_BUNDLE"),
            sha256: String::new(),
        }],
    }
}

fn env(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("{name} not set"))
        .into()
}

#[tokio::test]
#[ignore = "needs a saved Tron flash and room for its backup"]
async fn a_saved_tron_flash_backs_up_and_passes() {
    let tron = Tron {
        port: FilePort {
            path: env("TRON_FLASH"),
            read_only: true,
            started: Default::default(),
        },
    };
    let mut store = Store::open(&env("TRON_STORE")).unwrap();
    let work = WorkDir {
        path: env("TRON_WORK"),
    };
    let tables = Tables {
        raw: tables::SHIPPED.to_vec(),
    };
    let print = Print {
        start: Instant::now(),
        last: Cell::new(0),
    };
    let cancel = Cancel::default();
    let mut ctx = Ctx {
        store: &mut store,
        work: &work,
        tables: &tables,
        progress: &print,
        cancel: &cancel,
        log: &print,
        system_written: false,
    };
    let found = tron.port.find().await.unwrap().unwrap();
    let (mut dev, id) = tron.open(&found, &mut ctx).await.unwrap();
    eprintln!(
        "open: {} sectors, {} partitions, serial hash {}",
        id.flash_sectors,
        id.table.len(),
        id.serial_hash
    );
    let backup = tron.backup(&mut dev, &mut ctx).await.unwrap();
    eprintln!("backup done after {} s", print.start.elapsed().as_secs());
    for (file, sum) in &backup.files {
        eprintln!("{file} {sum}");
    }
    let findings = tron
        .check(&mut dev, &backup, &os_bundle(), &mut ctx)
        .await
        .unwrap();
    for f in &findings {
        eprintln!("{f:?}");
    }
    assert!(!findings.iter().any(|f| matches!(f, Finding::Stop { .. })));
    eprintln!("checks done after {} s", print.start.elapsed().as_secs());
}
