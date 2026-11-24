//! Free space for the backup and the work folder, checked before the mower is connected.

use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

/// The folder itself or the nearest one above it that exists.
fn existing(dir: &Path) -> PathBuf {
    let mut at = dir.to_path_buf();
    while !at.exists() {
        match at.parent() {
            Some(up) if !up.as_os_str().is_empty() => at = up.to_path_buf(),
            _ => return PathBuf::from("."),
        }
    }
    at
}

/// Bytes free for this user on the file system that holds dir.
pub fn free_bytes(dir: &Path) -> Result<u64> {
    let at = existing(dir);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(at.as_os_str().as_bytes())?;
        let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: path is a NUL-terminated string and st is space for one statvfs.
        let rc = unsafe { libc::statvfs(path.as_ptr(), st.as_mut_ptr()) };
        ensure!(rc == 0, "no free space known for {}", at.display());
        // SAFETY: statvfs filled it.
        let st = unsafe { st.assume_init() };
        // Narrower than u64 on some Unix systems.
        #[allow(clippy::useless_conversion)]
        let free = u64::from(st.f_bavail).saturating_mul(u64::from(st.f_frsize));
        Ok(free)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = at.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: wide is NUL-terminated, free is a valid u64, the other outputs are optional.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        ensure!(ok != 0, "no free space known for {}", at.display());
        Ok(free)
    }
}

/// Bytes the files below dir take already (a backup or work folder from an earlier run).
pub fn used_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok()?.metadata().ok())
        .map(|m| if m.is_file() { m.len() } else { 0 })
        .sum()
}

/// Whether the backup and the work folder have room for what is still to come; work inside the backup folder
/// shares its space.
pub fn check(store: &Path, backup_bytes: u64, work: &Path, work_bytes: u64) -> Result<()> {
    let backup_left = backup_bytes.saturating_sub(used_bytes(store));
    let work_left = work_bytes.saturating_sub(used_bytes(work));
    let gib = |b: u64| b.div_ceil(1 << 30);
    if work.starts_with(store) {
        let free = free_bytes(store)?;
        ensure!(
            free >= backup_left + work_left,
            "{} has {} GiB free, the backup and the work files need {} GiB",
            store.display(),
            gib(free),
            gib(backup_left + work_left)
        );
        return Ok(());
    }
    for (dir, need) in [(store, backup_left), (work, work_left)] {
        let free = free_bytes(dir)?;
        ensure!(
            free >= need,
            "{} has {} GiB free, it needs {} GiB",
            dir.display(),
            gib(free),
            gib(need)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_still_to_make_counts_on_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let free = free_bytes(dir.path()).unwrap();
        assert!(free > 0);
        assert!(free_bytes(&dir.path().join("a/b/c")).unwrap() > 0);
        assert!(check(&dir.path().join("b"), 1, &dir.path().join("b/work"), 1).is_ok());
        assert!(
            check(
                &dir.path().join("b"),
                free,
                &dir.path().join("b/work"),
                free
            )
            .is_err()
        );
    }

    #[test]
    fn files_already_there_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x"), vec![0u8; 1000]).unwrap();
        assert_eq!(used_bytes(dir.path()), 1000);
        assert_eq!(used_bytes(&dir.path().join("none")), 0);
    }
}
