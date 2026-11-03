//! Self-install on Windows: the downloaded clipper.exe copies itself to
//! %LOCALAPPDATA%\Programs\Clipper, adds Start menu + desktop shortcuts and an
//! entry under "Installed apps" (with Uninstall), then starts the installed copy.
//! The downloaded file can be deleted afterwards. No admin rights needed.

/// What `main` should do before the app starts.
pub enum Action {
    /// Already the installed copy (or not Windows): run normally.
    Run,
    /// Installed (or uninstalled) and handed over: exit now.
    #[cfg_attr(not(windows), allow(dead_code))]
    Exit,
}

#[cfg(not(windows))]
pub fn handle_start() -> Action {
    Action::Run
}

#[cfg(windows)]
pub fn handle_start() -> Action {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--uninstall") {
        win::uninstall();
        return Action::Exit;
    }
    // Development and "portable" use: --no-install keeps running from where it is.
    if args.iter().any(|a| a == "--no-install") {
        return Action::Run;
    }
    match win::install_if_needed() {
        Ok(true) => Action::Exit,
        Ok(false) => Action::Run,
        Err(e) => {
            win::message(&format!("Clipper could not install itself:\n{e}\n\nIt will run from this location instead."), false);
            Action::Run
        }
    }
}

/// A newer version was installed over this running one: start it (it waits
/// until this one is gone) and quit.
pub fn replace_with_installed(app: &tauri::AppHandle) {
    #[cfg(windows)]
    if let Some(exe) = win::installed_exe() {
        crate::clog!("a newer version was installed: handing over to it");
        let ok = std::process::Command::new(&exe)
            .args(["--installed", "--wait-pid", &std::process::id().to_string()])
            .current_dir(exe.parent().unwrap_or(std::path::Path::new(".")))
            .spawn()
            .is_ok();
        if ok {
            app.exit(0);
        }
    }
    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(windows)]
mod win {
    use std::{
        os::windows::process::CommandExt,
        path::{Path, PathBuf},
        process::Command,
    };
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    const NO_WINDOW: u32 = 0x0800_0000;
    const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Clipper";

    pub fn installed_exe() -> Option<PathBuf> {
        install_dir().map(|d| d.join("clipper.exe"))
    }

    fn install_dir() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("Clipper"))
    }

    fn shortcut_paths() -> Vec<PathBuf> {
        let mut v = vec![];
        if let Some(a) = std::env::var_os("APPDATA") {
            v.push(PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\Clipper.lnk"));
        }
        if let Some(u) = std::env::var_os("USERPROFILE") {
            v.push(PathBuf::from(u).join("Desktop\\Clipper.lnk"));
        }
        v
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
            _ => false,
        }
    }

    pub fn message(text: &str, question: bool) -> bool {
        use windows::{
            core::PCWSTR,
            Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO},
        };
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (t, c) = (wide(text), wide("Clipper"));
        let style = if question { MB_YESNO | MB_ICONQUESTION } else { MB_OK | MB_ICONINFORMATION };
        unsafe { MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style) == IDYES }
    }

    fn powershell(script: &str) -> bool {
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(NO_WINDOW)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn make_shortcut(lnk: &Path, target: &Path) {
        let q = |p: &Path| p.to_string_lossy().replace('\'', "''");
        let dir = target.parent().unwrap_or(target);
        powershell(&format!(
            "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath='{}'; \
             $s.WorkingDirectory='{}'; $s.IconLocation='{},0'; $s.Description='Clipper'; $s.Save()",
            q(lnk), q(target), q(dir), q(target)
        ));
    }

    /// Ok(true): installed and the installed copy was started (caller exits).
    pub fn install_if_needed() -> std::io::Result<bool> {
        let me = std::env::current_exe()?;
        let dir = install_dir().ok_or_else(|| std::io::Error::other("no LOCALAPPDATA"))?;
        let target = dir.join("clipper.exe");
        if same_file(&me, &target) {
            return Ok(false);
        }
        std::fs::create_dir_all(&dir)?;
        let mut replaced = false;
        if std::fs::copy(&me, &target).is_err() {
            replaced = true;
            // An installed Clipper is running: a running exe can be renamed, not overwritten.
            let old = target.with_extension("exe.old");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&target, &old)?;
            std::fs::copy(&me, &target)?;
        }
        for lnk in shortcut_paths() {
            make_shortcut(&lnk, &target);
        }
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(UNINSTALL_KEY)?;
        let exe = target.display().to_string();
        key.set_value("DisplayName", &"Clipper")?;
        key.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION"))?;
        key.set_value("Publisher", &"Jeremy Klootwijk")?;
        key.set_value("DisplayIcon", &exe)?;
        key.set_value("InstallLocation", &dir.display().to_string())?;
        key.set_value("UninstallString", &format!("\"{exe}\" --uninstall"))?;
        key.set_value("NoModify", &1u32)?;
        key.set_value("NoRepair", &1u32)?;
        key.set_value("EstimatedSize", &(std::fs::metadata(&target).map(|m| m.len() / 1024).unwrap_or(0) as u32))?;
        // With the old version still running, the single-instance plugin hands
        // this start to it: "--replace" tells it to quit and start the new one.
        let mut c = Command::new(&target);
        c.arg("--installed").current_dir(&dir);
        if replaced {
            c.arg("--replace");
        }
        c.spawn()?;
        Ok(true)
    }

    pub fn uninstall() {
        if !message("Remove Clipper from this PC?\n\nYour clips stay in Videos\\Clipper.", true) {
            return;
        }
        let remove_data = message(
            "Also remove Clipper's settings and its downloaded OBS and ffmpeg (about 500 MB)?",
            true,
        );
        // Stop a running Clipper (and with it, its OBS).
        let _ = Command::new("taskkill").args(["/IM", "clipper.exe", "/F", "/FI", &format!("PID ne {}", std::process::id())])
            .creation_flags(NO_WINDOW).status();
        for lnk in shortcut_paths() {
            let _ = std::fs::remove_file(lnk);
        }
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(run) = hkcu.open_subkey_with_flags("Software\\Microsoft\\Windows\\CurrentVersion\\Run", winreg::enums::KEY_WRITE) {
            let _ = run.delete_value("Clipper");
        }
        let _ = hkcu.delete_subkey_all(UNINSTALL_KEY);
        let mut dirs: Vec<PathBuf> = install_dir().into_iter().collect();
        if remove_data {
            if let Some(a) = std::env::var_os("APPDATA") {
                dirs.push(PathBuf::from(a).join("nl.jeremyklootwijk.clipper"));
            }
            if let Some(a) = std::env::var_os("LOCALAPPDATA") {
                dirs.push(PathBuf::from(a).join("nl.jeremyklootwijk.clipper"));
            }
        }
        // This exe is in the install folder: delete it after we've exited.
        let rm: Vec<String> = dirs.iter().map(|d| format!("rmdir /s /q \"{}\"", d.display())).collect();
        let _ = Command::new("cmd")
            .raw_arg(format!("/c ping 127.0.0.1 -n 3 > nul & {}", rm.join(" & ")))
            .creation_flags(NO_WINDOW)
            .spawn();
        message("Clipper has been removed.", false);
    }
}
