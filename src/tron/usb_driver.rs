//! WinUSB for the Tron on Windows: mowbite-usb-driver.exe next to the installer installs it with libwdi (LGPL-3, a
//! program of its own with its DLL, so the installer never links it). Windows asks the owner for permission (UAC).
//! On Linux a udev rule lets the owner at the desk use the Tron; pkexec asks for the password for one fixed script.

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

pub const ACCESS_DECLINED: &str = "tron.usb_access_declined";
pub const ACCESS_FAILED: &str = "tron.usb_access_failed";
pub const ACCESS_MANUAL: &str = "tron.usb_access_manual";
/// The rule: read and write for the user at the desk (uaccess), for the Tron in every USB mode.
const RULE: &str = r#"SUBSYSTEM=="usb", ATTR{idVendor}=="2207", ATTR{idProduct}=="350b", MODE="0660", TAG+="uaccess""#;
const RULE_FILE: &str = "/etc/udev/rules.d/70-mowbite-tron.rules";

/// The only thing run as root: write the rule, load it and apply it to the Tron already on USB. Fixed text, nothing
/// from outside goes in.
fn rule_script() -> String {
    format!(
        "umask 022 && printf '%s\\n' '{RULE}' > {RULE_FILE} && udevadm control --reload-rules && \
         udevadm trigger --subsystem-match=usb --attr-match=idVendor=2207 && udevadm settle"
    )
}

/// Whether this user may read and write the device file of the Tron at bus/address.
#[cfg(target_os = "linux")]
pub fn may_use(bus: u8, address: u8) -> bool {
    let path = std::ffi::CString::new(format!("/dev/bus/usb/{bus:03}/{address:03}"))
        .expect("no NUL in a formatted path");
    // SAFETY: path is a NUL-terminated string; access only reads it.
    unsafe { libc::access(path.as_ptr(), libc::R_OK | libc::W_OK) == 0 }
}

#[cfg(target_os = "linux")]
pub async fn install() -> Result<()> {
    tokio::task::spawn_blocking(|| {
        let ran = std::process::Command::new("pkexec")
            .args(["/bin/sh", "-c", &rule_script()])
            .status();
        pkexec_outcome(ran.map(|s| s.code()))
    })
    .await?
}

/// What pkexec's end means: 126 and 127 are its own for a password not given or not accepted.
fn pkexec_outcome(ran: std::io::Result<Option<i32>>) -> Result<()> {
    match ran {
        Ok(Some(0)) => Ok(()),
        Ok(Some(126 | 127)) => bail!("{ACCESS_DECLINED}: pkexec"),
        Ok(code) => bail!("{ACCESS_FAILED}: pkexec ended with {code:?}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "{ACCESS_MANUAL}: sudo sh -c \"{}\"",
                rule_script().replace('"', "\\\"")
            )
        }
        Err(e) => bail!("{ACCESS_FAILED}: {e}"),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub async fn install() -> Result<()> {
    bail!("{MISSING}: this system needs no USB driver")
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
    fn the_rule_script_writes_one_fixed_rule() {
        let script = rule_script();
        assert!(
            script.starts_with(
                "umask 022 && printf '%s\\n' 'SUBSYSTEM==\"usb\", ATTR{idVendor}==\"2207\""
            ) && script.contains("> /etc/udev/rules.d/70-mowbite-tron.rules && ")
                && script.ends_with("--attr-match=idVendor=2207 && udevadm settle"),
            "{script}"
        );
        // The rule sits in single quotes: none inside it.
        assert!(!RULE.contains('\''));
    }

    #[test]
    fn exit_codes_of_pkexec() {
        assert!(pkexec_outcome(Ok(Some(0))).is_ok());
        for code in [126, 127] {
            let e = pkexec_outcome(Ok(Some(code))).unwrap_err().to_string();
            assert!(e.starts_with(ACCESS_DECLINED), "{e}");
        }
        let e = pkexec_outcome(Ok(Some(1))).unwrap_err().to_string();
        assert!(e.starts_with(ACCESS_FAILED), "{e}");
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let e = pkexec_outcome(Err(missing)).unwrap_err().to_string();
        assert!(
            e.starts_with(ACCESS_MANUAL)
                && e.contains("sudo sh -c \"umask 022")
                && e.contains("\\\"usb\\\""),
            "{e}"
        );
    }

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
