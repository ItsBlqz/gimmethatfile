//! "Start with Windows": a `Run` entry for the current user that launches the
//! app with [`HIDDEN_ARG`], so it starts in the tray without showing a window.

use auto_launch::{AutoLaunch, AutoLaunchBuilder};

/// Command-line flag: start hidden in the tray.
pub const HIDDEN_ARG: &str = "--hidden";

/// Name of the startup entry (shown in Task Manager → Startup apps).
const ENTRY_NAME: &str = "Gimme That File";

fn launcher() -> Result<AutoLaunch, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = exe.display().to_string();
    // The registry value is a command line and the install folder has spaces.
    #[cfg(windows)]
    let path = format!("\"{path}\"");
    let mut builder = AutoLaunchBuilder::new();
    builder
        .set_app_name(ENTRY_NAME)
        .set_app_path(&path)
        .set_args(&[HIDDEN_ARG]);
    #[cfg(windows)]
    builder.set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser);
    builder.build().map_err(|e| e.to_string())
}

/// Whether the app is registered to start at sign-in (and not disabled in
/// Task Manager).
pub fn is_enabled() -> bool {
    launcher().and_then(|l| l.is_enabled().map_err(|e| e.to_string())) == Ok(true)
}

/// Register or remove the startup entry. Enabling again refreshes the path,
/// e.g. after the app was moved.
pub fn set(enabled: bool) -> Result<(), String> {
    let l = launcher()?;
    let res = if enabled { l.enable() } else { l.disable() };
    res.map_err(|e| format!("Couldn't change \"Start with Windows\": {e}"))?;
    #[cfg(windows)]
    if !enabled {
        forget_task_manager_state();
    }
    Ok(())
}

/// `auto-launch` marks the entry as enabled in Task Manager's list when
/// enabling, but leaves that value behind when disabling. Remove it too.
#[cfg(windows)]
fn forget_task_manager_state() {
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    if let Ok(key) = windows_registry::CURRENT_USER.options().write().open(KEY) {
        let _ = key.remove_value(ENTRY_NAME);
    }
}

/// Whether this launch should stay hidden in the tray.
pub fn launched_hidden<S: AsRef<str>>(args: impl IntoIterator<Item = S>) -> bool {
    args.into_iter().any(|a| a.as_ref() == HIDDEN_ARG)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_flag() {
        assert!(launched_hidden(["app.exe", "--hidden"]));
        assert!(!launched_hidden(["app.exe"]));
    }

    /// Touches the real startup entry of the current user, so it only runs on
    /// request: `cargo test -p gimmethatfile -- --ignored autostart`.
    /// Don't run it while an installed copy relies on "Start with Windows".
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn registers_and_removes_run_entry() {
        let query = || {
            std::process::Command::new("reg")
                .args([
                    "query",
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                    "/v",
                    ENTRY_NAME,
                ])
                .output()
                .unwrap()
        };
        let enabled = set(true).map(|_| (is_enabled(), query()));
        // Always clean up before asserting.
        set(false).unwrap();
        let (was_enabled, out) = enabled.unwrap();
        assert!(was_enabled);
        let value = String::from_utf8_lossy(&out.stdout).to_string();
        let exe = std::env::current_exe().unwrap().display().to_string();
        assert!(
            value.contains(&format!("\"{exe}\" {HIDDEN_ARG}")),
            "unexpected entry: {value}"
        );
        assert!(!is_enabled());
        assert!(!query().status.success(), "entry should be gone");
        let approved = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
                "/v",
                ENTRY_NAME,
            ])
            .output()
            .unwrap();
        assert!(
            !approved.status.success(),
            "Task Manager state should be gone"
        );
    }
}
