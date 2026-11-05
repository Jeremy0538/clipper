#![cfg_attr(not(windows), allow(dead_code))]
//! Clip confirmation: a short chime and a small toast in the top-right corner.
//! The toast never takes focus (a fullscreen game must not minimize) and lets
//! clicks through. Both can be switched off in the settings.

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

pub const TOAST: &str = "toast";
const TOAST_W: f64 = 340.0;
const TOAST_H: f64 = 84.0;
const MARGIN: f64 = 24.0;
const SHOW_MS: u64 = 2600;

/// Create the (hidden) toast window. Windows only; on Linux this is a no-op.
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    #[cfg(windows)]
    {
        use tauri::{WebviewUrl, WebviewWindowBuilder};
        let w = WebviewWindowBuilder::new(app, TOAST, WebviewUrl::App("toast.html".into()))
            .title("Clipper")
            .inner_size(TOAST_W, TOAST_H)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false)
            .build()?;
        let _ = w.set_ignore_cursor_events(true);
        if let Ok(hwnd) = w.hwnd() {
            win::make_passive(windows::Win32::Foundation::HWND(hwnd.0));
        }
    }
    let _ = app;
    Ok(())
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Fire-and-forget toast (+ chime on success).
pub fn toast(app: &AppHandle, ok: bool, title: impl Into<String>, detail: impl Into<String>) {
    let (app, title, detail) = (app.clone(), title.into(), detail.into());
    tauri::async_runtime::spawn(async move { show(&app, ok, &title, &detail, ok, false).await });
}

/// Toast without the chime (status info like "game detected").
pub fn info(app: &AppHandle, title: impl Into<String>, detail: impl Into<String>) {
    let (app, title, detail) = (app.clone(), title.into(), detail.into());
    tauri::async_runtime::spawn(async move { show(&app, true, &title, &detail, false, false).await });
}

/// Info toast shown even with "Pop-up notifications" off: for rare, one-off
/// news the user would otherwise miss (an update found at start).
pub fn notice(app: &AppHandle, title: impl Into<String>, detail: impl Into<String>) {
    let (app, title, detail) = (app.clone(), title.into(), detail.into());
    tauri::async_runtime::spawn(async move { show(&app, true, &title, &detail, false, true).await });
}

/// `ok` = success styling; `title` big line, `detail` small line.
async fn show(app: &AppHandle, ok: bool, title: &str, detail: &str, with_sound: bool, always: bool) {
    let (toast_on, sound_on, theme) = {
        let st = app.state::<crate::AppState>();
        let u = st.user.lock().await;
        (u.toast, u.sound, u.theme.clone())
    };
    if sound_on && with_sound {
        chime();
    }
    if !toast_on && !always {
        return;
    }
    // saved = green edge (with chime), info = accent, error = red; same theme as the app.
    let kind = if !ok { "error" } else if with_sound { "saved" } else { "info" };
    let _ = app.emit_to(TOAST, "toast", json!({ "kind": kind, "title": title, "detail": detail, "theme": theme }));

    #[cfg(windows)]
    if let Some(w) = app.get_webview_window(TOAST) {
        use std::sync::atomic::Ordering;
        if let Ok(Some(m)) = w.primary_monitor() {
            let s = m.scale_factor();
            let x = m.position().x as f64 + m.size().width as f64 - (TOAST_W + MARGIN) * s;
            let y = m.position().y as f64 + MARGIN * s;
            let _ = w.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
        }
        if let Ok(hwnd) = w.hwnd() {
            win::show_no_activate(windows::Win32::Foundation::HWND(hwnd.0));
        }
        let gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        tokio::time::sleep(std::time::Duration::from_millis(SHOW_MS)).await;
        if GENERATION.load(Ordering::SeqCst) == gen {
            let _ = w.hide();
        }
    }
}

#[cfg(windows)]
fn chime() {
    use std::sync::OnceLock;
    use windows::{core::PCWSTR, Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT}};
    // Async playback reads the buffer after the call returns: keep it static.
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    let wav = WAV.get_or_init(chime_wav);
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

#[cfg(not(windows))]
fn chime() {}

/// Two soft rising notes as a 16-bit mono WAV.
#[cfg_attr(not(windows), allow(dead_code))]
fn chime_wav() -> Vec<u8> {
    const RATE: u32 = 44_100;
    let notes = [(880.0f32, 0.09f32), (1318.5, 0.16)];
    let mut pcm: Vec<i16> = vec![];
    for (freq, dur) in notes {
        let n = (RATE as f32 * dur) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let attack = (t / 0.005).min(1.0);
            let decay = (-t * 18.0).exp();
            let v = (2.0 * std::f32::consts::PI * freq * t).sin() * attack * decay * 0.35;
            pcm.push((v * i16::MAX as f32) as i16);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(windows)]
mod win {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_EX_LAYERED,
        },
    };

    /// Never activate, no taskbar/alt-tab entry, clicks pass through.
    pub fn make_passive(hwnd: HWND) {
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let add = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0) as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | add);
        }
    }

    pub fn show_no_activate(hwnd: HWND) {
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header() {
        let w = super::chime_wav();
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, w.len() - 8);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize, w.len() - 44);
    }
}
