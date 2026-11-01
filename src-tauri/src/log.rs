//! Plain-text debug log in the data folder (clipper.log, rotated at 2 MB).

use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

static FILE: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());
const MAX_BYTES: u64 = 2_000_000;

pub fn init(path: PathBuf) {
    let _ = FILE.set(path);
    line(&format!("---- Clipper {} started ----", env!("CARGO_PKG_VERSION")));
}

pub fn path() -> Option<&'static PathBuf> {
    FILE.get()
}

pub fn line(msg: &str) {
    let Some(file) = FILE.get() else { return };
    let _g = LOCK.lock();
    if std::fs::metadata(file).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(file, file.with_extension("old.log"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"));
    }
    #[cfg(debug_assertions)]
    eprintln!("{msg}");
}

/// `clog!("x {}", y)` → one log line.
#[macro_export]
macro_rules! clog {
    ($($t:tt)*) => { $crate::log::line(&format!($($t)*)) };
}
