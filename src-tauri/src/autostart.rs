//! "Start with Windows": a Run entry that starts Clipper hidden in the tray.

#[cfg(windows)]
pub fn apply(on: bool) {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    let Ok((run, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY) else { return };
    let r = if on {
        match std::env::current_exe() {
            Ok(exe) => run.set_value("Clipper", &format!("\"{}\" --hidden", exe.display())),
            Err(e) => Err(e),
        }
    } else {
        match run.delete_value("Clipper") {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    };
    match r {
        Ok(()) => crate::clog!("autostart {}", if on { "on" } else { "off" }),
        Err(e) => crate::clog!("ERROR autostart: {e}"),
    }
}

#[cfg(not(windows))]
pub fn apply(_on: bool) {}
