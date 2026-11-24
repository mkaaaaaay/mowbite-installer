//! The installer in text mode: mowbite-installer install|back <backup folder> <release folder>.

use anyhow::{Context, Result};
use mowbite_installer::sign::Signers;
use mowbite_installer::tron::module::Tron;
use mowbite_installer::tron::port::UsbPort;
use mowbite_installer::tron::tables;
use mowbite_installer::{cli, release};
use std::process::ExitCode;

async fn run(list: &[String]) -> Result<bool> {
    let args = cli::parse(list)?;
    let signers = Signers::builtin();
    let tables = cli::tables(&args.release, &signers, "tron")?;
    let named = tables::loader(&tables.raw)?;
    let path = cli::release_file(&args.release, &named.file)?;
    let bytes = std::fs::read(&path).with_context(|| path.display().to_string())?;
    let loader = release::loader(&bytes, &cli::signature(&path)?, &signers)?;
    let tron = Tron {
        port: UsbPort { loader },
    };
    cli::run(&tron, &args, &signers, &tables).await
}

fn main() -> ExitCode {
    println!("mowbite-installer {}", env!("CARGO_PKG_VERSION"));
    let list: Vec<String> = std::env::args().skip(1).collect();
    let done = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("no runtime")
        .and_then(|rt| rt.block_on(run(&list)));
    match done {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(2)
        }
    }
}
