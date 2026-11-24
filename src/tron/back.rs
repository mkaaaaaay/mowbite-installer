//! Back to the vendor system: prove that the backup belongs to this flash, then write the saved rootfs partition
//! into the rootfs window. misc is left out of the proof because the loader writes it on restarts.

use super::backup::{Area, areas, file_name, open_raw, raw_sha256, read_area};
use crate::disk::Disk;
use crate::mower::{Backup, Cancel, Part, WritePlan};
use crate::store::Store;
use anyhow::{Context, Result, ensure};

/// Areas MowBite OS never writes; their sha256 ties a backup to one device.
pub const FINGERPRINT: [&str; 4] = ["flash-start", "uboot", "boot", "recovery"];

/// The fingerprint of a finished backup: (area, sha256) from its file list.
pub fn fingerprint(files: &[(String, String)]) -> Result<Vec<(String, String)>> {
    FINGERPRINT
        .iter()
        .map(|name| {
            let file = file_name(name);
            files
                .iter()
                .find(|(f, _)| *f == file)
                .map(|(_, sum)| (name.to_string(), sum.clone()))
                .with_context(|| format!("the backup has no {file}"))
        })
        .collect()
}

/// Reads the fingerprint areas from the device and compares them and the partition table with the backup.
pub async fn belongs_here<D: Disk>(
    disk: &mut D,
    table_now: &[Part],
    backup: &Backup,
) -> Result<()> {
    ensure!(
        table_now == backup.table,
        "the partition table differs from the one in the backup"
    );
    let all = areas(table_now, disk.sectors())?;
    for (name, want) in &backup.fingerprint {
        let area: &Area = all
            .iter()
            .find(|a| a.name == *name)
            .with_context(|| format!("no area {name}"))?;
        let got = read_area(disk, area, &Cancel::default(), &mut |_| {}, &mut |_| Ok(())).await?;
        ensure!(
            got == *want,
            "{name} differs from the backup: this backup belongs to another mower"
        );
    }
    Ok(())
}

/// The plan that writes the saved rootfs back and its unpacked bytes; the file is checked against SHA256SUMS and
/// its raw sha256 against the backup before anything is written.
pub fn plan(store: &Store, backup: &Backup) -> Result<(WritePlan, impl std::io::Read + use<>)> {
    let part = backup
        .table
        .iter()
        .find(|p| p.name == "rootfs")
        .context("the backup has no rootfs partition")?;
    let file = file_name("rootfs");
    let (_, sha256) = backup
        .files
        .iter()
        .find(|(f, _)| *f == file)
        .with_context(|| format!("{file} is not in the backup"))?;
    ensure!(
        raw_sha256(store, &file)? == *sha256,
        "{file} does not unpack to the rootfs that was saved"
    );
    let sectors = part.last_sector - part.first_sector + 1;
    let plan = WritePlan {
        partition: part.name.clone(),
        first_sector: part.first_sector,
        sectors,
        partition_sectors: sectors,
        image: store.path(&file)?,
        sha256: sha256.clone(),
    };
    Ok((plan, open_raw(store, &file)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::FileDisk;
    use crate::mower::Progress;
    use crate::tron::backup::backup;
    use crate::tron::image::write_and_verify;

    struct Quiet;
    impl Progress for Quiet {
        fn step(&self, _: &'static str) {}
        fn bytes(&self, _: u64, _: u64) {}
    }

    fn table() -> Vec<Part> {
        let p = |n: &str, a, b| Part {
            name: n.into(),
            first_sector: a,
            last_sector: b,
        };
        vec![
            p("uboot", 16, 31),
            p("misc", 32, 39),
            p("boot", 40, 63),
            p("recovery", 64, 79),
            p("rootfs", 80, 199),
        ]
    }

    async fn saved(flash: &std::path::Path, store: &mut Store) -> Backup {
        let mut disk = FileDisk::open(flash, vec![]).unwrap();
        let files = backup(&mut disk, &table(), store, &Cancel::default(), &Quiet)
            .await
            .unwrap();
        Backup {
            table: table(),
            fingerprint: fingerprint(&files).unwrap(),
            files,
        }
    }

    #[tokio::test]
    async fn writes_the_saved_rootfs_back_onto_the_same_mower() {
        let dir = tempfile::tempdir().unwrap();
        let flash = dir.path().join("flash.img");
        let original: Vec<u8> = (0..256 * 512).map(|i| (i % 241) as u8).collect();
        std::fs::write(&flash, &original).unwrap();
        let mut store = Store::open(&dir.path().join("backup")).unwrap();
        let b = saved(&flash, &mut store).await;
        // MowBite OS in rootfs, and the loader wrote misc.
        let mut changed = original.clone();
        changed[80 * 512..200 * 512].fill(0x42);
        changed[32 * 512] ^= 1;
        std::fs::write(&flash, &changed).unwrap();

        let mut disk = FileDisk::open(&flash, vec![]).unwrap();
        belongs_here(&mut disk, &table(), &b).await.unwrap();
        let (plan, mut src) = plan(&store, &b).unwrap();
        write_and_verify(disk, &mut src, &plan, &Cancel::default(), &Quiet)
            .await
            .unwrap();
        let after = std::fs::read(&flash).unwrap();
        assert_eq!(&after[80 * 512..200 * 512], &original[80 * 512..200 * 512]);
        assert_eq!(after[32 * 512], changed[32 * 512]);
    }

    #[tokio::test]
    async fn another_mower_or_a_changed_backup_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let flash = dir.path().join("flash.img");
        std::fs::write(&flash, vec![7u8; 256 * 512]).unwrap();
        let mut store = Store::open(&dir.path().join("backup")).unwrap();
        let b = saved(&flash, &mut store).await;

        let mut other = vec![7u8; 256 * 512];
        other[45 * 512] = 8;
        std::fs::write(&flash, &other).unwrap();
        let mut disk = FileDisk::open(&flash, vec![]).unwrap();
        let err = belongs_here(&mut disk, &table(), &b).await.unwrap_err();
        assert!(err.to_string().contains("boot differs"), "{err}");
        let mut t = table();
        t[4].last_sector = 198;
        assert!(belongs_here(&mut disk, &t, &b).await.is_err());

        std::fs::write(store.path("rootfs.img.zst").unwrap(), vec![0u8; 120 * 512]).unwrap();
        let Err(err) = plan(&store, &b) else {
            panic!("a changed backup file passed");
        };
        assert!(
            err.to_string().contains("changed since the backup"),
            "{err}"
        );
    }
}
