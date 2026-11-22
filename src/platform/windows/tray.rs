//! The notification area (tray) icon of the resident agent. A hidden window on its own thread
//! receives the icon's clicks (the thread waits in `GetMessageW`); a click opens the settings.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{LIM_SMALL, LoadIconMetric};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW,
    IsIconic, MSG, RegisterClassExW, RegisterWindowMessageW, SW_RESTORE, SetForegroundWindow, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSEXW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use super::{autostart, spawn_detached};
use crate::platform::AgentSpec;

const WM_TRAY: u32 = WM_APP + 1;

/// The hidden window that owns the icon (0 until it exists).
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// Broadcast by Explorer after it restarts: the icon has to be added again.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// The agent executable and the config, to start the settings window.
static SETTINGS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

/// Shows the icon for the registered agent (not for test or manual runs).
pub fn start(spec: &AgentSpec) {
    if !spec.tray_icon || !autostart::is_registered_as(&spec.exe, &spec.config) {
        return;
    }
    let _ = SETTINGS.set((spec.exe.clone(), spec.config.clone()));
    if let Err(e) = std::thread::Builder::new().name("tray".into()).spawn(run) {
        crate::warn!("タスクトレイにアイコンを置けません: {e}");
    }
}

/// Takes the icon away before the agent exits (Explorer would keep it until hovered).
pub fn remove() {
    let hwnd = WINDOW.load(Ordering::Acquire);
    if hwnd != 0 {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(HWND(hwnd as *mut _)));
        }
    }
}

fn run() {
    let Ok(instance) = (unsafe { GetModuleHandleW(None) }) else { return };
    let class = w!("OptimanTray");
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: class,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return;
    }
    // A top-level window that is never shown: a message-only one would miss TaskbarCreated.
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Optiman"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    };
    let Ok(hwnd) = created else { return };
    TASKBAR_CREATED.store(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }, Ordering::Relaxed);
    WINDOW.store(hwnd.0 as isize, Ordering::Release);
    add(hwnd);
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
}

fn add(hwnd: HWND) {
    let mut nid = icon_data(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    // The app icon embedded by build.rs (MAKEINTRESOURCE(1)), at the small icon size for the DPI.
    let instance = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
    nid.hIcon =
        unsafe { LoadIconMetric(instance, PCWSTR(std::ptr::without_provenance(1)), LIM_SMALL) }.unwrap_or_default();
    let tip: Vec<u16> = crate::app::ui_text::TITLE.encode_utf16().take(nid.szTip.len() - 1).collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool() {
        crate::warn!("タスクトレイにアイコンを置けません");
    }
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TRAY && matches!(lparam.0 as u32, WM_LBUTTONUP | WM_RBUTTONUP) {
        open_settings();
        return LRESULT(0);
    }
    if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) {
        add(hwnd);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Brings an open settings window to the front, or starts one.
fn open_settings() {
    if let Ok(window) = unsafe { FindWindowW(w!("OptimanSettings"), PCWSTR::null()) } {
        unsafe {
            if IsIconic(window).as_bool() {
                let _ = ShowWindow(window, SW_RESTORE);
            }
            let _ = SetForegroundWindow(window);
        }
        return;
    }
    let Some((exe, config)) = SETTINGS.get() else { return };
    // The click lets this process bring a window forward; pass that on to the settings window.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    if let Err(e) = spawn_detached(Command::new(exe).arg("--settings").arg("--config").arg(config)) {
        crate::warn!("設定画面を開けません: {e}");
    }
}
