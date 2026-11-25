//! WinUSB for the Tron on Windows: mowbite-usb-driver.exe next to the installer installs it with libwdi (LGPL-3, a
//! program of its own with its DLL, so the installer never links it). Windows asks the owner for permission (UAC).

use anyhow::{Result, bail};

/// The helper next to the installer.
pub const HELPER: &str = "mowbite-usb-driver.exe";
pub const DECLINED: &str = "tron.usb_driver_declined";
pub const FAILED: &str = "tron.usb_driver_failed";
pub const MISSING: &str = "tron.usb_driver_missing";

/// What the exit code of the helper means, with the line it wrote.
pub fn outcome(code: u32, line: &str) -> Result<()> {
    match code {
        0 => Ok(()),
        2 => bail!("{FAILED}: the Tron is no longer on USB"),
        _ => bail!(
            "{FAILED}: {}",
            if line.is_empty() {
                "no reason given"
            } else {
                line
            }
        ),
    }
}

#[cfg(windows)]
pub async fn install() -> Result<()> {
    tokio::task::spawn_blocking(run).await?
}

#[cfg(not(windows))]
pub async fn install() -> Result<()> {
    bail!("{MISSING}: only Windows needs a USB driver")
}

/// Starts the helper elevated and waits for it; it writes one line on what it did into a file of ours.
#[cfg(windows)]
fn run() -> Result<()> {
    use anyhow::Context;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let helper = std::env::current_exe()?.with_file_name(HELPER);
    if !helper.exists() {
        bail!("{MISSING}: {} is missing", helper.display());
    }
    let result =
        std::env::temp_dir().join(format!("mowbite-usb-driver-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&result);
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide("runas".as_ref());
    let file = wide(helper.as_os_str());
    let mut quoted = std::ffi::OsString::from("\"");
    quoted.push(result.as_os_str());
    quoted.push("\"");
    let params = wide(&quoted);
    // SAFETY: the struct is plain data; every pointer in it outlives the call.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: info is filled as ShellExecuteExW expects.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        // SAFETY: no arguments.
        let error = unsafe { GetLastError() };
        if error == ERROR_CANCELLED {
            bail!("{DECLINED}: Windows asked for permission and it was not given");
        }
        bail!("{FAILED}: the helper did not start (Windows error {error})");
    }
    let mut code = 1u32;
    // SAFETY: hProcess is the handle of the started helper, closed once here.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    let line = std::fs::read_to_string(&result).unwrap_or_default();
    let _ = std::fs::remove_file(&result);
    outcome(code, line.trim()).context("the USB driver of the Tron")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_of_the_helper() {
        assert!(outcome(0, "WinUSB installed").is_ok());
        let gone = outcome(2, "").unwrap_err().to_string();
        assert!(
            gone.starts_with(FAILED) && gone.contains("no longer on USB"),
            "{gone}"
        );
        let failed = outcome(4, "installing: timeout").unwrap_err().to_string();
        assert!(failed.ends_with("installing: timeout"), "{failed}");
        assert!(
            outcome(3, "")
                .unwrap_err()
                .to_string()
                .ends_with("no reason given")
        );
    }
}
