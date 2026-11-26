//! Keeps the PC from sleeping while a run reads or writes the mower: systemd-inhibit on Linux, the execution state
//! of the thread on Windows. Without either the run goes on and the frame tells the owner to keep the PC awake.

use anyhow::Result;

pub struct Awake {
    #[cfg(unix)]
    child: std::process::Child,
}

impl Awake {
    #[cfg(unix)]
    pub fn hold(why: &str) -> Result<Awake> {
        use std::process::{Command, Stdio};
        let child = Command::new("systemd-inhibit")
            .args([
                "--what=sleep:idle",
                "--who=mowbite-installer",
                "--mode=block",
                // A refusal is fine: the frame tells the owner to keep the PC awake instead of a password prompt.
                "--no-ask-password",
            ])
            .arg(format!("--why={why}"))
            // Ends with this process, also when it is killed.
            .args([
                "tail",
                &format!("--pid={}", std::process::id()),
                "-f",
                "/dev/null",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut awake = Awake { child };
        // logind refusing the lock ends systemd-inhibit at once.
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Some(status) = awake.child.try_wait()? {
            anyhow::bail!("systemd-inhibit ended: {status}");
        }
        Ok(awake)
    }

    #[cfg(windows)]
    pub fn hold(_why: &str) -> Result<Awake> {
        use windows_sys::Win32::System::Power::{
            ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
        };
        // SAFETY: a plain call without pointers; 0 means it failed.
        let before = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
        anyhow::ensure!(
            before != 0,
            "Windows did not take the request to stay awake"
        );
        Ok(Awake {})
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Power::{ES_CONTINUOUS, SetThreadExecutionState};
            // SAFETY: as in hold().
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_hold_ends_with_its_value() {
        let Ok(awake) = Awake::hold("test") else {
            eprintln!("no systemd-inhibit: skipped");
            return;
        };
        let pid = awake.child.id();
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        drop(awake);
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        assert!(state.is_empty() || state.contains(") Z"), "{state}");
    }
}
