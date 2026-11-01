//! Detects a running Steam game or Minecraft.

use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    pub name: String,
    pub pid: u32,
    /// OBS window string "title:class:exe" (Windows only; empty elsewhere).
    pub obs_window: String,
}

/// Steam folders that live in steamapps/common but are not games.
const NOT_GAMES: &[&str] = &["steamworks shared", "steam controller configs", "proton", "steamlinuxruntime",
                              "steamvr", "wallpaper_engine", "soundpad"];

struct SteamGame {
    /// "<library>/steamapps/common/<installdir>/", lowercase, forward slashes.
    dir: String,
    name: String,
}

fn norm(s: &str) -> String {
    s.replace('\\', "/").to_lowercase()
}

/// Last quoted value on a `"key"  "value"` VDF line.
fn vdf_value(line: &str, key: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix(&format!("\"{key}\""))?;
    let v = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some(v.replace("\\\\", "\\"))
}

fn steam_roots() -> Vec<PathBuf> {
    let mut roots = vec![];
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam") {
            if let Ok(p) = k.get_value::<String, _>("SteamPath") {
                roots.push(PathBuf::from(p));
            }
        }
        roots.push(PathBuf::from("C:\\Program Files (x86)\\Steam"));
    }
    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.push(home.join(".local/share/Steam"));
        roots.push(home.join(".steam/steam"));
    }
    roots
}

fn library_paths() -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut libs = vec![];
    for root in steam_roots() {
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        let mut found: Vec<PathBuf> = fs::read_to_string(&vdf)
            .map(|s| s.lines().filter_map(|l| vdf_value(l, "path")).map(PathBuf::from).collect())
            .unwrap_or_default();
        found.push(root);
        for lib in found {
            let key = fs::canonicalize(&lib).unwrap_or(lib.clone());
            if lib.join("steamapps").is_dir() && seen.insert(key) {
                libs.push(lib);
            }
        }
    }
    libs
}

/// Every game Clipper can record on this PC (installed Steam games + Minecraft),
/// sorted by name.
pub fn installed_games() -> Vec<String> {
    let mut names: Vec<String> = index_steam().into_iter().map(|g| g.name).collect();
    names.push("Minecraft".into());
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

fn index_steam() -> Vec<SteamGame> {
    let mut games = vec![];
    for lib in library_paths() {
        let apps = lib.join("steamapps");
        let Ok(rd) = fs::read_dir(&apps) else { continue };
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().into_owned();
            if !fname.starts_with("appmanifest_") || !fname.ends_with(".acf") {
                continue;
            }
            let Ok(text) = fs::read_to_string(e.path()) else { continue };
            let (mut name, mut dir) = (None, None);
            for l in text.lines() {
                name = name.or_else(|| vdf_value(l, "name"));
                dir = dir.or_else(|| vdf_value(l, "installdir"));
            }
            let (Some(name), Some(dir)) = (name, dir) else { continue };
            let lower = dir.to_lowercase();
            if NOT_GAMES.iter().any(|n| lower.starts_with(n)) {
                continue;
            }
            let full = apps.join("common").join(&dir);
            games.push(SteamGame { dir: format!("{}/", norm(&full.to_string_lossy())), name });
        }
    }
    games
}

pub struct Detector {
    sys: System,
    steam: Vec<SteamGame>,
    indexed: Option<Instant>,
}

impl Detector {
    pub fn new() -> Self {
        Self { sys: System::new(), steam: vec![], indexed: None }
    }

    fn steam_name(&self, haystacks: &[String]) -> Option<&str> {
        self.steam
            .iter()
            .find(|g| haystacks.iter().any(|h| h.contains(&g.dir)))
            .map(|g| g.name.as_str())
    }

    /// One scan of the process list.
    /// `disabled`: game names the user switched off (case-insensitive).
    pub fn detect(&mut self, disabled: &[String]) -> Option<Game> {
        if self.indexed.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
            self.steam = index_steam();
            self.indexed = Some(Instant::now());
        }
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );

        let mut candidates: Vec<(String, u32)> = vec![];
        for (pid, p) in self.sys.processes() {
            if p.thread_kind().is_some() {
                continue; // Linux threads show up as processes
            }
            let pname = p.name().to_string_lossy().to_lowercase();
            let exe = p.exe().map(|e| norm(&e.to_string_lossy())).unwrap_or_default();
            let cmd = norm(&p.cmd().iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));

            let name = if matches!(pname.as_str(), "java" | "javaw" | "java.exe" | "javaw.exe")
                && cmd.contains("minecraft")
            {
                Some("Minecraft")
            } else if pname == "minecraft.windows.exe" {
                Some("Minecraft")
            } else {
                // Windows: the exe sits in the game folder. Linux/Proton: the game
                // path is on the command line of the wine process.
                if cfg!(windows) { self.steam_name(&[exe]) } else { self.steam_name(&[exe, cmd]) }
            };
            if let Some(n) = name {
                if !disabled.iter().any(|d| d.eq_ignore_ascii_case(n)) {
                    candidates.push((n.to_string(), pid.as_u32()));
                }
            }
        }
        candidates.sort_by_key(|c| c.1);
        pick(candidates)
    }
}

/// Windows: prefer the process with the biggest window (skips launchers' helper
/// processes, crash handlers, anti-cheat services). A game whose window is
/// minimized or hidden (alt-tab, other app focused) still counts: the buffer
/// must keep running until the process exits.
#[cfg(windows)]
fn pick(candidates: Vec<(String, u32)>) -> Option<Game> {
    candidates
        .into_iter()
        .map(|(name, pid)| match win::main_window(pid) {
            Some(w) => (w.area, Game { name, pid, obs_window: w.obs_string() }),
            None => (-1, Game { name, pid, obs_window: String::new() }),
        })
        .max_by_key(|(area, _)| *area)
        .map(|(_, g)| g)
}

#[cfg(not(windows))]
fn pick(candidates: Vec<(String, u32)>) -> Option<Game> {
    candidates
        .into_iter()
        .next()
        .map(|(name, pid)| Game { name, pid, obs_window: String::new() })
}

#[cfg(windows)]
mod win {
    use windows::core::BOOL;
    use windows::Win32::{
        Foundation::{HWND, LPARAM, RECT},
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
            IsWindowVisible, GetWindow, GW_OWNER, IsIconic, GetWindowPlacement, WINDOWPLACEMENT,
        },
    };

    pub struct Win {
        pub title: String,
        pub class: String,
        pub exe: String,
        pub area: i64,
    }

    impl Win {
        /// OBS encodes ':' and '#' inside each part.
        pub fn obs_string(&self) -> String {
            let enc = |s: &str| s.replace('#', "#22").replace(':', "#3A");
            format!("{}:{}:{}", enc(&self.title), enc(&self.class), enc(&self.exe))
        }
    }

    struct Search {
        pid: u32,
        best: Option<(HWND, i64)>,
    }

    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let s = &mut *(lp.0 as *mut Search);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == s.pid && IsWindowVisible(hwnd).as_bool()
            && GetWindow(hwnd, GW_OWNER).map(|o| o.0.is_null()).unwrap_or(true)
        {
            // A minimized window's rect is a tiny strip: use its restored size.
            let mut r = RECT::default();
            let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            let got = if IsIconic(hwnd).as_bool() && GetWindowPlacement(hwnd, &mut wp).is_ok() {
                r = wp.rcNormalPosition;
                true
            } else {
                GetWindowRect(hwnd, &mut r).is_ok()
            };
            if got {
                let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                if area >= 320 * 240 && s.best.is_none_or(|(_, a)| area > a) {
                    s.best = Some((hwnd, area));
                }
            }
        }
        true.into()
    }

    fn exe_name(pid: u32) -> Option<String> {
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows::Win32::Foundation::CloseHandle;
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
            let _ = CloseHandle(h);
            ok.ok()?;
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            full.rsplit(['\\', '/']).next().map(String::from)
        }
    }

    pub fn main_window(pid: u32) -> Option<Win> {
        let mut s = Search { pid, best: None };
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut s as *mut _ as isize));
        }
        let (hwnd, area) = s.best?;
        let mut t = [0u16; 512];
        let mut c = [0u16; 256];
        let (tn, cn) = unsafe { (GetWindowTextW(hwnd, &mut t), GetClassNameW(hwnd, &mut c)) };
        Some(Win {
            title: String::from_utf16_lossy(&t[..tn.max(0) as usize]),
            class: String::from_utf16_lossy(&c[..cn.max(0) as usize]),
            exe: exe_name(pid)?,
            area,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vdf_values() {
        assert_eq!(vdf_value("\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"", "path").as_deref(), Some("D:\\SteamLibrary"));
        assert_eq!(vdf_value("\t\"name\"\t\t\"STAR WARS Jedi: Survivor™\"", "name").as_deref(),
                   Some("STAR WARS Jedi: Survivor™"));
        assert_eq!(vdf_value("\t\"installdir\"\t\"X\"", "name"), None);
    }

    /// Prints what the scanner sees on this machine (run with --nocapture).
    #[test]
    fn scan_this_machine() {
        let games = index_steam();
        for g in &games {
            println!("steam: {} -> {}", g.name, g.dir);
        }
        println!("running: {:?}", Detector::new().detect(&[]));
    }
}
