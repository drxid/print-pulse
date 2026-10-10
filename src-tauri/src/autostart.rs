//! Launch at login through the per-user `Run` registry key.
//!
//! This replaces tauri-plugin-autostart, which had three problems on Windows:
//! - it writes the exe path unquoted, while installs live in a folder with a
//!   space (`…\Local\Print Pulse\`);
//! - the NSIS uninstaller deletes the entry on every manual reinstall, so
//!   autostart silently turned itself off;
//! - enabling it from a dev build registered `target\debug\…exe`, which at
//!   login opens a window pointing at a dev server that isn't running.
//!
//! So the app remembers the user's choice itself and re-registers on every
//! start, always quoted and always for the exe that is actually running.

use std::path::Path;

use serde::Serialize;

pub const ARG: &str = "--autostarted";
#[cfg(windows)]
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Task Manager / Settings → Startup apps store the user's on/off override.
#[cfg(windows)]
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
#[cfg(windows)]
const APPROVED_ON: [u8; 12] = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// What Windows currently has for this app.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Exe the `Run` value points at, if there is one.
    pub path: Option<String>,
    /// Switched off in Task Manager / Settings → Startup apps.
    pub blocked: bool,
}

impl Entry {
    pub fn points_to(&self, exe: &Path) -> bool {
        self.path.as_deref().is_some_and(|p| same_path(p, exe))
    }
}

/// The value we write: quoted path, then the flag that tells the app it was
/// started at login.
pub fn command_line(exe: &Path) -> String {
    format!("\"{}\" {ARG}", exe.display())
}

/// The exe path from a `Run` value, quoted (ours) or not (the old plugin's).
pub fn exe_of(cmd: &str) -> String {
    let cmd = cmd.trim();
    match cmd.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default().to_string(),
        None => match cmd.find(" --") {
            Some(i) => cmd[..i].to_string(),
            None => cmd.to_string(),
        },
    }
}

/// A build straight out of `cargo` (`…\target\debug\…`), as opposed to an install.
pub fn is_build_output(path: &str) -> bool {
    let p = path.to_lowercase().replace('/', "\\");
    p.contains("\\target\\debug\\") || p.contains("\\target\\release\\")
}

fn same_path(a: &str, b: &Path) -> bool {
    a.trim().eq_ignore_ascii_case(b.to_string_lossy().trim())
}

/// Task Manager writes 02… for enabled and 03… (plus a timestamp) for disabled.
fn approved_blocked(bytes: &[u8]) -> bool {
    bytes.first().is_some_and(|b| b & 1 == 1)
}

#[cfg(windows)]
pub fn read(name: &str) -> Entry {
    use windows_registry::CURRENT_USER;
    let path = CURRENT_USER
        .open(RUN)
        .and_then(|k| k.get_string(name))
        .ok()
        .map(|cmd| exe_of(&cmd));
    let blocked = CURRENT_USER
        .open(APPROVED)
        .and_then(|k| k.get_value(name))
        .is_ok_and(|v| approved_blocked(&v));
    Entry { path, blocked }
}

/// Register `exe` to start at login and clear a Task Manager "disabled" flag —
/// turning it on in the app is an explicit request.
#[cfg(windows)]
pub fn register(name: &str, exe: &Path) -> Result<(), String> {
    use windows_registry::{Type, CURRENT_USER};
    CURRENT_USER
        .create(RUN)
        .and_then(|k| k.set_string(name, command_line(exe)))
        .map_err(|e| format!("Run: {}", e.message()))?;
    if let Ok(k) = CURRENT_USER.options().write().open(APPROVED) {
        let _ = k.set_bytes(name, Type::Bytes, &APPROVED_ON);
    }
    Ok(())
}

#[cfg(windows)]
pub fn unregister(name: &str) -> Result<(), String> {
    use windows_registry::CURRENT_USER;
    match CURRENT_USER.options().write().open(RUN).and_then(|k| k.remove_value(name)) {
        Ok(()) => Ok(()),
        // Already gone.
        Err(e) if e.code() == windows_result::HRESULT::from_win32(2) => Ok(()),
        Err(e) => Err(format!("Run: {}", e.message())),
    }
}

#[cfg(not(windows))]
pub fn read(_: &str) -> Entry {
    Entry::default()
}
#[cfg(not(windows))]
pub fn register(_: &str, _: &Path) -> Result<(), String> {
    Err("unsupported platform".into())
}
#[cfg(not(windows))]
pub fn unregister(_: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn quotes_paths_with_spaces() {
        let exe = PathBuf::from(r"C:\Users\PC\AppData\Local\Print Pulse\printpulse.exe");
        let cmd = command_line(&exe);
        assert_eq!(cmd, r#""C:\Users\PC\AppData\Local\Print Pulse\printpulse.exe" --autostarted"#);
        assert_eq!(exe_of(&cmd), exe.to_string_lossy());
    }

    #[test]
    fn reads_old_unquoted_entries() {
        assert_eq!(
            exe_of(r"C:\Users\PC\AppData\Local\Cursor Jiggler\cursor-jiggler.exe --autostarted"),
            r"C:\Users\PC\AppData\Local\Cursor Jiggler\cursor-jiggler.exe"
        );
        assert_eq!(exe_of(r"D:\app.exe"), r"D:\app.exe");
    }

    #[test]
    fn spots_dev_builds() {
        assert!(is_build_output(r"D:\dev\printpulse\src-tauri\target\debug\printpulse.exe"));
        assert!(!is_build_output(r"C:\Users\PC\AppData\Local\Print Pulse\printpulse.exe"));
    }

    #[test]
    fn task_manager_flag() {
        assert!(!approved_blocked(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(approved_blocked(&[0x03, 0, 0, 0, 0xa7, 0x15, 0x08, 0xd1, 0x66, 0x2d, 0xdc, 0x01]));
        assert!(!approved_blocked(&[]));
    }

    /// End to end against the real registry and process launcher: register an
    /// exe that lives in a folder with a space under a throwaway name, read the
    /// `Run` value back, and start it from that exact command line the way
    /// Windows does at login (CreateProcess with no application name).
    /// Run with `cargo test autostart -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn registry_roundtrip_and_launch() {
        use windows::core::PWSTR;
        use windows::Win32::System::Threading::*;

        let name = "Print Pulse (autostart test)";
        let dir = std::env::temp_dir().join("pp autostart test");
        std::fs::create_dir_all(&dir).unwrap();
        // Any small console exe will do; `whoami` exits right away.
        let exe = dir.join("probe app.exe");
        std::fs::copy(r"C:\Windows\System32\whoami.exe", &exe).unwrap();

        register(name, &exe).unwrap();
        let entry = read(name);
        let cmd = windows_registry::CURRENT_USER.open(RUN).unwrap().get_string(name).unwrap();
        unregister(name).unwrap();
        if let Ok(k) = windows_registry::CURRENT_USER.options().write().open(APPROVED) {
            let _ = k.remove_value(name);
        }

        assert!(entry.points_to(&exe), "{entry:?}");
        assert!(!entry.blocked);
        assert_eq!(read(name), Entry::default(), "unregister left the value behind");

        let mut line: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
        let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                None,
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW,
                None,
                None,
                &si,
                &mut pi,
            )
        }
        .unwrap_or_else(|e| panic!("could not start {cmd}: {}", e.message()));
        unsafe {
            WaitForSingleObject(pi.hProcess, 5000);
            let _ = windows::Win32::Foundation::CloseHandle(pi.hProcess);
            let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
