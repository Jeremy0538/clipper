//! The clip key (default F8, user-configurable). A low-level keyboard hook
//! only *listens*: the key still reaches the game (a combo like Shift+F can be
//! sprint + interact). Armed only while the replay buffer runs, so the key does nothing
//! outside games.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Hotkey {
    /// DOM `KeyboardEvent.code`, e.g. "KeyF", "F8", "Digit1".
    pub code: String,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
}

impl Default for Hotkey {
    fn default() -> Self {
        Self { code: "F8".into(), shift: false, ctrl: false, alt: false }
    }
}

impl Hotkey {
    /// Windows virtual-key code for `code`; None for unsupported keys.
    pub fn vk(&self) -> Option<u32> {
        let c = self.code.as_str();
        let single = |s: &str| s.len() == 1;
        if let Some(l) = c.strip_prefix("Key").filter(|s| single(s)) {
            let ch = l.as_bytes()[0];
            return ch.is_ascii_uppercase().then_some(ch as u32);
        }
        if let Some(d) = c.strip_prefix("Digit").filter(|s| single(s)) {
            let ch = d.as_bytes()[0];
            return ch.is_ascii_digit().then_some(ch as u32);
        }
        if let Some(d) = c.strip_prefix("Numpad").and_then(|s| s.parse::<u32>().ok()) {
            return (d <= 9).then_some(0x60 + d);
        }
        if let Some(n) = c.strip_prefix('F').and_then(|s| s.parse::<u32>().ok()) {
            return (1..=24).contains(&n).then_some(0x70 + n - 1);
        }
        Some(match c {
            "Backquote" => 0xC0, "Minus" => 0xBD, "Equal" => 0xBB, "BracketLeft" => 0xDB,
            "BracketRight" => 0xDD, "Backslash" => 0xDC, "Semicolon" => 0xBA, "Quote" => 0xDE,
            "Comma" => 0xBC, "Period" => 0xBE, "Slash" => 0xBF, "Space" => 0x20, "Tab" => 0x09,
            "Insert" => 0x2D, "Delete" => 0x2E, "Home" => 0x24, "End" => 0x23,
            "PageUp" => 0x21, "PageDown" => 0x22, "PrintScreen" => 0x2C, "Pause" => 0x13,
            "ScrollLock" => 0x91, "ArrowUp" => 0x26, "ArrowDown" => 0x28, "ArrowLeft" => 0x25,
            "ArrowRight" => 0x27, "NumpadAdd" => 0x6B, "NumpadSubtract" => 0x6D,
            "NumpadMultiply" => 0x6A, "NumpadDivide" => 0x6F, "NumpadDecimal" => 0x6E,
            _ => return None,
        })
    }

    pub fn label(&self) -> String {
        let c = self.code.as_str();
        let key = c
            .strip_prefix("Key")
            .or_else(|| c.strip_prefix("Digit"))
            .map(String::from)
            .or_else(|| c.strip_prefix("Numpad").map(|n| format!("Num {n}")))
            .unwrap_or_else(|| match c {
                "Backquote" => "`".into(), "Minus" => "-".into(), "Equal" => "=".into(),
                "BracketLeft" => "[".into(), "BracketRight" => "]".into(), "Backslash" => "\\".into(),
                "Semicolon" => ";".into(), "Quote" => "'".into(), "Comma" => ",".into(),
                "Period" => ".".into(), "Slash" => "/".into(),
                other => other.replace("Arrow", ""),
            });
        let mut parts = vec![];
        if self.ctrl { parts.push("Ctrl".to_string()); }
        if self.alt { parts.push("Alt".to_string()); }
        if self.shift { parts.push("Shift".to_string()); }
        parts.push(key);
        parts.join("+")
    }
}

/// Which key: the clip key (only while clipping runs) or the record key (always).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slot {
    Clip = 0,
    Record = 1,
    Screenshot = 2,
}

impl Hotkey {
    /// Default record key: F9 (Alt+F9/F10 are NVIDIA's overlay keys).
    pub fn default_record() -> Self {
        Self { code: "F9".into(), shift: false, ctrl: false, alt: false }
    }
    /// Default screenshot key: F10 (F12 alone is Steam's).
    pub fn default_screenshot() -> Self {
        Self { code: "F10".into(), shift: false, ctrl: false, alt: false }
    }
}

// What the hook compares against (atomics: the hook callback can't lock).
static ARMED: AtomicBool = AtomicBool::new(false);
static VK: [AtomicU32; 3] = [AtomicU32::new(0x46), AtomicU32::new(0x78), AtomicU32::new(0x79)];
/// Required modifiers: bit 0 shift, 1 ctrl, 2 alt.
static MODS: [AtomicU32; 3] = [AtomicU32::new(1), AtomicU32::new(4), AtomicU32::new(4)];

pub fn set_armed(on: bool) {
    ARMED.store(on, Ordering::SeqCst);
}

pub fn apply(slot: Slot, h: &Hotkey) -> Result<(), String> {
    let vk = h.vk().ok_or_else(|| format!("{} can't be used as a key", h.code))?;
    VK[slot as usize].store(vk, Ordering::SeqCst);
    MODS[slot as usize].store(h.shift as u32 | (h.ctrl as u32) << 1 | (h.alt as u32) << 2, Ordering::SeqCst);
    Ok(())
}

#[cfg(windows)]
pub fn start(app: tauri::AppHandle) {
    use std::sync::{mpsc, OnceLock};
    use windows::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT},
            WindowsAndMessaging::{
                CallNextHookEx, GetMessageW, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, MSG,
                WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
            },
        },
    };

    static TX: OnceLock<mpsc::Sender<Slot>> = OnceLock::new();
    static KEY_DOWN: [AtomicBool; 3] = [AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)];

    unsafe fn held(vk: u16) -> bool {
        (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0
    }

    unsafe extern "system" fn proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            for slot in [Slot::Clip, Slot::Record, Slot::Screenshot] {
                let i = slot as usize;
                if k.vkCode != VK[i].load(Ordering::SeqCst) {
                    continue;
                }
                let msg = wp.0 as u32;
                if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                    // Auto-repeat sends more key-downs: fire once per press.
                    let first = !KEY_DOWN[i].swap(true, Ordering::SeqCst);
                    let m = MODS[i].load(Ordering::SeqCst);
                    // Exactly these modifiers: F8 must not fire on Ctrl+F8, and with
                    // F8 and Shift+F8 on two slots only one of them fires.
                    let mods_ok = (m & 1 != 0) == held(VK_SHIFT.0)
                        && (m & 2 != 0) == held(VK_CONTROL.0)
                        && (m & 4 != 0) == held(VK_MENU.0);
                    let armed = slot != Slot::Clip || ARMED.load(Ordering::SeqCst);
                    if first && mods_ok && armed {
                        if let Some(tx) = TX.get() {
                            let _ = tx.send(slot);
                        }
                    }
                } else if msg == WM_KEYUP || msg == WM_SYSKEYUP {
                    KEY_DOWN[i].store(false, Ordering::SeqCst);
                }
            }
        }
        // Always pass the key on.
        CallNextHookEx(None, code, wp, lp)
    }

    let (tx, rx) = mpsc::channel::<Slot>();
    let _ = TX.set(tx);

    // The hook needs a thread with a message loop; keep the callback tiny.
    std::thread::spawn(|| unsafe {
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), None, 0) {
            Ok(_) => {
                crate::clog!("keyboard hook installed");
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
            }
            Err(e) => crate::clog!("ERROR keyboard hook failed: {e}"),
        }
    });
    std::thread::spawn(move || {
        for slot in rx {
            let app = app.clone();
            match slot {
                Slot::Clip => {
                    crate::clog!("clip key pressed");
                    tauri::async_runtime::spawn(async move {
                        let _ = crate::save_clip_inner(&app).await; // errors are reported inside
                    });
                }
                Slot::Screenshot => {
                    crate::clog!("screenshot key pressed");
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = crate::take_screenshot(&app).await {
                            crate::report_error(&app, format!("Screenshot: {e}"));
                        }
                    });
                }
                Slot::Record => {
                    crate::clog!("record key pressed");
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = crate::modes::toggle_recording(&app).await {
                            crate::report_error(&app, format!("Recording: {e}"));
                        }
                    });
                }
            }
        }
    });
}

#[cfg(not(windows))]
pub fn start(_app: tauri::AppHandle) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let h = Hotkey::default();
        assert_eq!((h.vk(), h.label().as_str()), (Some(0x77), "F8"));
        let f8 = Hotkey { code: "F8".into(), shift: false, ctrl: false, alt: false };
        assert_eq!((f8.vk(), f8.label().as_str()), (Some(0x77), "F8"));
        let c = Hotkey { code: "Digit3".into(), shift: false, ctrl: true, alt: true };
        assert_eq!((c.vk(), c.label().as_str()), (Some(0x33), "Ctrl+Alt+3"));
        assert_eq!(Hotkey { code: "ShiftLeft".into(), ..h.clone() }.vk(), None);
        assert_eq!(Hotkey { code: "Fn".into(), ..h }.vk(), None);
    }
}
