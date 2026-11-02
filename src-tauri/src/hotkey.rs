//! The clip key (default F8, user-configurable). A low-level keyboard hook and
//! raw input only *listen*: the key still reaches the game (a combo like Shift+F can be
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

/// A press seen by the hook and by raw input is one press: the first path to
/// see it fires, the other is ignored when it comes within this time (raw
/// input can lag behind the hook while a game keeps the CPU busy).
#[cfg_attr(not(windows), allow(dead_code))]
const SAME_PRESS_MS: u64 = 750;
/// Auto-repeat sends key-downs at least this often while a key is held
/// (Windows' longest repeat delay is 1 s). A key-down after a longer silence
/// is a new press, even if its key-up was never seen (Ctrl+Alt+Del, Win+L).
#[cfg_attr(not(windows), allow(dead_code))]
const REPEAT_GAP_MS: u64 = 1_100;

/// Milliseconds on a monotonic clock (the wall clock can jump back).
#[cfg_attr(not(windows), allow(dead_code))]
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

/// Per listener and slot: is the key held, and when did its last key-down
/// (or repeat) come. Returns true for a new press, false for auto-repeat.
#[cfg_attr(not(windows), allow(dead_code))]
struct Held {
    down: [AtomicBool; 3],
    last: [std::sync::atomic::AtomicU64; 3],
}
#[cfg_attr(not(windows), allow(dead_code))]
impl Held {
    const fn new() -> Self {
        use std::sync::atomic::AtomicU64;
        Self {
            down: [AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)],
            last: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
        }
    }
    fn key_down(&self, i: usize, now: u64) -> bool {
        let was_down = self.down[i].swap(true, Ordering::SeqCst);
        let prev = self.last[i].swap(now, Ordering::SeqCst);
        !was_down || now.saturating_sub(prev) > REPEAT_GAP_MS
    }
    fn key_up(&self, i: usize) {
        self.down[i].store(false, Ordering::SeqCst);
    }
}

/// Both listeners call this on a new press (not auto-repeat) with the
/// modifiers held at that moment (bit 0 shift, 1 ctrl, 2 alt).
#[cfg_attr(not(windows), allow(dead_code))]
fn key_down(vk: u32, mods: u32, via: &'static str, now: u64) -> Option<Slot> {
    use std::sync::atomic::AtomicU64;
    static LAST: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
    // Exactly these modifiers: F8 must not fire on Ctrl+F8, and with F8 and
    // Shift+F8 on two slots only one of them fires.
    let slot = [Slot::Clip, Slot::Record, Slot::Screenshot].into_iter().find(|s| {
        VK[*s as usize].load(Ordering::SeqCst) == vk && MODS[*s as usize].load(Ordering::SeqCst) == mods
    })?;
    if slot == Slot::Clip && !ARMED.load(Ordering::SeqCst) {
        return None;
    }
    // Claim the press in one atomic step: the hook and raw-input threads can
    // get here at the same instant.
    let claimed = LAST[slot as usize].try_update(Ordering::SeqCst, Ordering::SeqCst, |prev| {
        (prev == 0 || now.saturating_sub(prev) >= SAME_PRESS_MS).then_some(now)
    });
    if claimed.is_err() {
        return None;
    }
    crate::clog!("{slot:?} key pressed ({via})");
    Some(slot)
}

/// Two independent listeners, because either can miss a press on some PCs:
/// - a low-level keyboard hook. Windows skips (and after a while silently
///   removes) a hook that doesn't answer fast enough, which happens when a
///   game keeps the CPU busy: a short tap was then lost, and only holding the
///   key (auto-repeat) got through.
/// - raw input (WM_INPUT to a hidden window): posted, never skipped.
/// Neither blocks the key; it always reaches the game.
#[cfg(windows)]
pub fn start(app: tauri::AppHandle) {
    use std::sync::{mpsc, OnceLock};
    use windows::core::w;
    use windows::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL},
        },
        UI::{
            Input::{
                GetRawInputData, GetRegisteredRawInputDevices, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
                RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEKEYBOARD,
                KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT},
            },
            WindowsAndMessaging::{
                CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW,
                RegisterClassW, SetTimer, SetWindowsHookExW, HC_ACTION, HWND_MESSAGE, KBDLLHOOKSTRUCT, MSG,
                WH_KEYBOARD_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_KEYDOWN, WM_TIMER, WM_KEYUP,
                WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW,
            },
        },
    };

    static TX: OnceLock<mpsc::Sender<Slot>> = OnceLock::new();
    fn send(slot: Option<Slot>) {
        if let (Some(slot), Some(tx)) = (slot, TX.get()) {
            let _ = tx.send(slot);
        }
    }
    fn is_down(msg: u32) -> Option<bool> {
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(true),
            WM_KEYUP | WM_SYSKEYUP => Some(false),
            _ => None,
        }
    }

    // ---- low-level hook ----
    static HOOK: Held = Held::new();
    static RAW: Held = Held::new();

    // Modifiers as Windows sees them now (for raw input that is right after
    // the key, so a held Shift/Ctrl/Alt is included and can never get stuck).
    fn mods() -> u32 {
        let held = |vk: u16| unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 };
        held(VK_SHIFT.0) as u32 | (held(VK_CONTROL.0) as u32) << 1 | (held(VK_MENU.0) as u32) << 2
    }

    unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            if let Some(i) = VK.iter().position(|v| v.load(Ordering::SeqCst) == k.vkCode) {
                match is_down(wp.0 as u32) {
                    // Auto-repeat sends more key-downs: only a new press counts.
                    Some(true) => {
                        let now = now_ms();
                        if HOOK.key_down(i, now) {
                            send(key_down(k.vkCode, mods(), "hook", now));
                        }
                    }
                    Some(false) => HOOK.key_up(i),
                    None => {}
                }
            }
        }
        // Always pass the key on.
        CallNextHookEx(None, code, wp, lp)
    }

    // ---- raw input ----
    // One window per process gets keyboard raw input. Tauri's event loop may
    // register its own window too (device events) and would silently take it
    // over: check now and then, and take it back.
    unsafe fn register(hwnd: HWND) -> windows::core::Result<()> {
        // Keyboard (generic desktop page 1, usage 6), also while another app has focus.
        let dev = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        RegisterRawInputDevices(&[dev], std::mem::size_of::<RAWINPUTDEVICE>() as u32)
    }
    unsafe fn still_ours(hwnd: HWND) -> bool {
        let mut list = [RAWINPUTDEVICE::default(); 16];
        let mut n = list.len() as u32;
        let got = GetRegisteredRawInputDevices(Some(list.as_mut_ptr()), &mut n, std::mem::size_of::<RAWINPUTDEVICE>() as u32);
        if got == u32::MAX {
            return true; // can't tell: leave it
        }
        list[..got as usize].iter().any(|d| d.usUsagePage == 1 && d.usUsage == 6 && d.hwndTarget == hwnd)
    }
    const CHECK_TIMER: usize = 1;

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_TIMER && wp.0 == CHECK_TIMER {
            if !still_ours(hwnd) {
                match register(hwnd) {
                    Ok(()) => crate::clog!("raw keyboard input was taken over; registered again"),
                    Err(e) => crate::clog!("ERROR raw keyboard input lost and could not register again: {e}"),
                }
            }
            return LRESULT(0);
        }
        if msg == WM_INPUT {
            let mut raw = RAWINPUT::default();
            let mut size = std::mem::size_of::<RAWINPUT>() as u32;
            let got = GetRawInputData(HRAWINPUT(lp.0 as _), RID_INPUT, Some(&mut raw as *mut _ as *mut _),
                &mut size, std::mem::size_of::<RAWINPUTHEADER>() as u32);
            if got != u32::MAX && got > 0 && raw.header.dwType == RIM_TYPEKEYBOARD.0 {
                let kb = raw.data.keyboard;
                let vk = kb.VKey as u32;
                if let (Some(down), Some(i)) = (is_down(kb.Message), VK.iter().position(|v| v.load(Ordering::SeqCst) == vk)) {
                    let now = now_ms();
                    if !down {
                        RAW.key_up(i);
                    } else if RAW.key_down(i, now) {
                        send(key_down(vk, mods(), "raw", now));
                    }
                }
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    let (tx, rx) = mpsc::channel::<Slot>();
    let _ = TX.set(tx);

    // The hook needs a thread with a message loop; keep the callback tiny and
    // let it run before the game's threads, so Windows never times it out.
    std::thread::spawn(|| unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) {
            Ok(_) => {
                crate::clog!("keyboard hook installed");
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
            }
            Err(e) => crate::clog!("ERROR keyboard hook failed: {e}"),
        }
    });
    std::thread::spawn(|| unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        let r = (|| -> windows::core::Result<()> {
            let inst = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst, lpszClassName: w!("ClipperRawKeys"), ..Default::default() };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_win32());
            }
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("ClipperRawKeys"), w!(""), WINDOW_STYLE(0),
                0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst), None)?;
            register(hwnd)?;
            SetTimer(Some(hwnd), CHECK_TIMER, 10_000, None);
            Ok(())
        })();
        match r {
            Ok(()) => {
                crate::clog!("raw keyboard input registered");
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
            Err(e) => crate::clog!("ERROR raw keyboard input failed: {e}"),
        }
    });
    std::thread::spawn(move || {
        for slot in rx {
            let app = app.clone();
            match slot {
                Slot::Clip => {
                    tauri::async_runtime::spawn(async move {
                        let _ = crate::save_clip_inner(&app).await; // errors are reported inside
                    });
                }
                Slot::Screenshot => {
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = crate::take_screenshot(&app).await {
                            crate::report_error(&app, format!("Screenshot: {e}"));
                        }
                    });
                }
                Slot::Record => {
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

    #[test]
    fn one_press_from_two_listeners() {
        apply(Slot::Screenshot, &Hotkey { code: "F7".into(), shift: false, ctrl: false, alt: false }).unwrap();
        assert_eq!(key_down(0x76, 1, "test", 1000), None, "Shift+F7 is not F7");
        assert_eq!(key_down(0x76, 0, "hook", 1000), Some(Slot::Screenshot));
        assert_eq!(key_down(0x76, 0, "raw", 1600), None, "same press, raw input lagging");
        assert_eq!(key_down(0x76, 0, "hook", 2000), Some(Slot::Screenshot), "next press");
    }

    #[test]
    fn repeat_and_lost_key_up() {
        let h = Held::new();
        assert!(h.key_down(0, 100));
        assert!(!h.key_down(0, 600), "auto-repeat");
        assert!(!h.key_down(0, 640));
        h.key_up(0);
        assert!(h.key_down(0, 900));
        // key-up never arrived (Win+L): a press after a silence still counts
        assert!(h.key_down(0, 5000));
    }
}
