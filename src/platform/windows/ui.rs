//! Settings window (plain Win32 + common controls v6). It runs only while open: closing it ends
//! the process, so nothing stays resident. Per-monitor DPI aware (v2) through the manifest.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Result, bail};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, CreateFontIndirectW, DeleteObject, GetSysColorBrush, HFONT, HGDIOBJ,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TBM_SETPAGESIZE, TBM_SETPOS,
    TBM_SETRANGE, TBS_AUTOTICKS, TRACKBAR_CLASSW, WC_BUTTONW, WC_LISTBOXW, WC_STATICW,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, HMENU, IDC_ARROW, IDCANCEL, IDOK, IsDialogMessageW, LB_ADDSTRING, LB_ERR, LB_GETCURSEL,
    LB_RESETCONTENT, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LoadCursorW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    MESSAGEBOX_STYLE, MSG, MessageBoxW, NONCLIENTMETRICSW, PostMessageW, PostQuitMessage, RegisterClassExW,
    SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_DESTROY, WM_DPICHANGED, WM_HSCROLL, WM_SETFONT, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_EX_CONTROLPARENT, WS_GROUP, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

use super::{ensure_com, image};
use crate::app::{self, Settings};
use crate::config::{Config, EFFORT_RANGE, Quality};

const WIDTH: i32 = 560;
const HEIGHT: i32 = 624;

const ID_ADD_INCLUDE: u16 = 101;
const ID_REMOVE_INCLUDE: u16 = 102;
const ID_ADD_EXCLUDE: u16 = 103;
const ID_REMOVE_EXCLUDE: u16 = 104;
const ID_INSTALL_EXTENSION: u16 = 105;
/// The quality radio buttons are `ID_QUALITY + index in Quality::ALL`.
const ID_QUALITY: u16 = 110;
const ID_SLIDER: u16 = 201;
const ID_INCLUDE_LIST: u16 = 202;
const ID_EXCLUDE_LIST: u16 = 203;
/// `TBM_GETPOS` is `WM_USER` (not exported by the windows crate).
const TBM_GETPOS: u32 = 0x0400;
/// Radio button message and style (winuser.h).
const BM_SETCHECK: u32 = 0x00F1;
const BS_AUTORADIOBUTTON: u32 = 0x0009;
/// Posted by the extension installer thread; `wparam` is 1 on success.
const WM_EXTENSION_DONE: u32 = WM_APP + 1;

/// A child control and its frame in 96-DPI units (x, y, width, height).
struct Child {
    hwnd: HWND,
    frame: [i32; 4],
}

struct Ui {
    config_path: PathBuf,
    window: Cell<HWND>,
    include: RefCell<Vec<PathBuf>>,
    exclude: RefCell<Vec<PathBuf>>,
    effort: Cell<u8>,
    quality: Cell<Quality>,
    children: RefCell<Vec<Child>>,
    font: Cell<HFONT>,
    slider: Cell<HWND>,
    effort_label: Cell<HWND>,
    quality_note: Cell<HWND>,
    include_list: Cell<HWND>,
    exclude_list: Cell<HWND>,
    hint_label: Cell<HWND>,
    extension_button: Cell<HWND>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(v: i32, dpi: u32) -> i32 {
    (i64::from(v) * i64::from(dpi) / 96) as i32
}

fn message_box(owner: HWND, text: &str, style: MESSAGEBOX_STYLE) {
    let text = wide(text);
    unsafe {
        MessageBoxW(Some(owner), PCWSTR(text.as_ptr()), w!("Optiman"), style | MB_OK);
    }
}

fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(text.as_ptr()));
    }
}

/// Lets the user pick a folder with the standard folder dialog.
fn pick_folder(owner: HWND, title: &str) -> Option<PathBuf> {
    ensure_com();
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).ok()?;
        let title = wide(title);
        let _ = dialog.SetTitle(PCWSTR(title.as_ptr()));
        dialog.Show(Some(owner)).ok()?; // cancelled → error
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const c_void));
        path.map(PathBuf::from)
    }
}

impl Ui {
    fn add_child(&self, class: PCWSTR, text: &str, style: WINDOW_STYLE, id: u16, frame: [i32; 4]) -> HWND {
        let text = wide(text);
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                0,
                0,
                Some(self.window.get()),
                Some(HMENU(usize::from(id) as *mut c_void)),
                None,
                None,
            )
        }
        .unwrap_or_default();
        self.children.borrow_mut().push(Child { hwnd, frame });
        hwnd
    }

    fn label(&self, text: &str, frame: [i32; 4]) -> HWND {
        self.add_child(WC_STATICW, text, WINDOW_STYLE::default(), 0, frame)
    }

    fn button(&self, text: &str, id: u16, frame: [i32; 4]) -> HWND {
        let style = if id == IDOK.0 as u16 { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON };
        self.add_child(WC_BUTTONW, text, WS_TABSTOP | WINDOW_STYLE(style as u32), id, frame)
    }

    fn list(&self, id: u16, frame: [i32; 4]) -> HWND {
        let style = WS_TABSTOP | WS_BORDER | WS_VSCROLL | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32);
        self.add_child(WC_LISTBOXW, "", style, id, frame)
    }

    fn build(&self) {
        self.label(
            "写真を JPEG XL に変換します。JPEG はいつも元の JPEG にそのまま戻せる形で変換します。PNG、TIFF、BMP、ロスレス WebP は下の画質で変換します。HEIC と非可逆 WebP は変換しません。",
            [20, 16, 520, 52],
        );

        self.label("圧縮の強さ", [20, 86, 106, 20]);
        let slider = self.add_child(
            TRACKBAR_CLASSW,
            "",
            WS_TABSTOP | WINDOW_STYLE(TBS_AUTOTICKS),
            ID_SLIDER,
            [130, 80, 300, 30],
        );
        unsafe {
            let (lo, hi) = (*EFFORT_RANGE.start() as isize, *EFFORT_RANGE.end() as isize);
            SendMessageW(slider, TBM_SETRANGE, Some(WPARAM(0)), Some(LPARAM(lo | (hi << 16))));
            SendMessageW(slider, TBM_SETPAGESIZE, Some(WPARAM(0)), Some(LPARAM(1)));
            SendMessageW(slider, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(isize::from(self.effort.get()))));
        }
        self.slider.set(slider);
        self.effort_label.set(self.label(&app::effort_label(self.effort.get()), [440, 86, 110, 20]));
        self.label("速い ← 時間をかけるほど小さくなります (画質は同じ) → 小さい", [130, 112, 400, 18]);

        self.label("画質", [20, 148, 106, 20]);
        let widths = [150, 70, 70, 70];
        let mut x = 130;
        for (i, (q, w)) in Quality::ALL.iter().zip(widths).enumerate() {
            // WS_GROUP on the first button makes the four one group (arrow keys move within it).
            let group = if i == 0 { WS_GROUP | WS_TABSTOP } else { WINDOW_STYLE::default() };
            let radio = self.add_child(
                WC_BUTTONW,
                q.name(),
                group | WINDOW_STYLE(BS_AUTORADIOBUTTON),
                ID_QUALITY + i as u16,
                [x, 146, w, 22],
            );
            if *q == self.quality.get() {
                unsafe {
                    SendMessageW(radio, BM_SETCHECK, Some(WPARAM(1)), None);
                }
            }
            x += w + 10;
        }
        // WS_GROUP here closes the radio group, so arrow keys stay among the four choices.
        let note = self.add_child(WC_STATICW, &quality_note(self.quality.get()), WS_GROUP, 0, [130, 174, 410, 40]);
        self.quality_note.set(note);

        self.label("変換するフォルダ (サブフォルダも対象)", [20, 226, 400, 20]);
        self.include_list.set(self.list(ID_INCLUDE_LIST, [20, 248, 410, 110]));
        self.button("追加…", ID_ADD_INCLUDE, [440, 248, 100, 28]);
        self.button("削除", ID_REMOVE_INCLUDE, [440, 282, 100, 28]);

        self.label("除外するフォルダ (この中は変換しません)", [20, 374, 400, 20]);
        self.exclude_list.set(self.list(ID_EXCLUDE_LIST, [20, 396, 410, 110]));
        self.button("追加…", ID_ADD_EXCLUDE, [440, 396, 100, 28]);
        self.button("削除", ID_REMOVE_EXCLUDE, [440, 430, 100, 28]);

        if let Some(hint) = image::viewer_hint() {
            let extension = image::viewer_extension_supported();
            self.hint_label.set(self.label(&hint, [20, 518, if extension { 350 } else { 520 }, 48]));
            if extension {
                self.extension_button.set(self.button(
                    "拡張機能をインストール",
                    ID_INSTALL_EXTENSION,
                    [380, 522, 160, 30],
                ));
            }
        }

        self.button("キャンセル", IDCANCEL.0 as u16, [330, 578, 100, 30]);
        self.button("保存", IDOK.0 as u16, [440, 578, 100, 30]);
        self.refresh_lists();
    }

    /// Positions every control and applies the message font for `dpi`.
    fn layout(&self, dpi: u32) {
        let mut metrics = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let font = unsafe {
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                metrics.cbSize,
                Some((&raw mut metrics).cast()),
                0,
                dpi,
            )
            .map(|()| CreateFontIndirectW(&metrics.lfMessageFont))
            .unwrap_or_default()
        };
        for c in self.children.borrow().iter() {
            let [x, y, w, h] = c.frame.map(|v| scale(v, dpi));
            unsafe {
                let _ = SetWindowPos(c.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
                SendMessageW(c.hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            }
        }
        let old = self.font.replace(font);
        if !old.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
        }
    }

    fn refresh_lists(&self) {
        for (list, paths) in [(self.include_list.get(), &self.include), (self.exclude_list.get(), &self.exclude)] {
            unsafe {
                SendMessageW(list, LB_RESETCONTENT, None, None);
                for p in paths.borrow().iter() {
                    let text = wide(&p.to_string_lossy());
                    SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
                }
            }
        }
    }

    fn add_folder(&self, include: bool) {
        let title = if include { "変換するフォルダを選択" } else { "除外するフォルダを選択" };
        let Some(path) = pick_folder(self.window.get(), title) else { return };
        let list = if include { &self.include } else { &self.exclude };
        if !list.borrow().contains(&path) {
            list.borrow_mut().push(path);
        }
        self.refresh_lists();
    }

    fn remove_selected(&self, include: bool) {
        let (hwnd, list) =
            if include { (self.include_list.get(), &self.include) } else { (self.exclude_list.get(), &self.exclude) };
        let sel = unsafe { SendMessageW(hwnd, LB_GETCURSEL, None, None) }.0;
        if sel != LB_ERR as isize
            && let Ok(i) = usize::try_from(sel)
            && i < list.borrow().len()
        {
            list.borrow_mut().remove(i);
            self.refresh_lists();
        }
    }

    fn effort_changed(&self) {
        let pos = unsafe { SendMessageW(self.slider.get(), TBM_GETPOS, None, None) }.0;
        let effort = u8::try_from(pos).unwrap_or(7).clamp(*EFFORT_RANGE.start(), *EFFORT_RANGE.end());
        if effort != self.effort.get() {
            self.effort.set(effort);
            set_text(self.effort_label.get(), &app::effort_label(effort));
        }
    }

    fn quality_changed(&self, index: usize) {
        if let Some(&q) = Quality::ALL.get(index) {
            self.quality.set(q);
            set_text(self.quality_note.get(), &quality_note(q));
        }
    }

    fn save(&self) {
        let settings = Settings {
            effort: self.effort.get(),
            quality: self.quality.get(),
            include: self.include.borrow().clone(),
            exclude: self.exclude.borrow().clone(),
        };
        let window = self.window.get();
        match app::save_settings(&self.config_path, &settings) {
            Ok(_) => {
                message_box(window, "保存しました。新しい設定で監視を再開しました。", MB_ICONINFORMATION);
                unsafe {
                    let _ = DestroyWindow(window);
                }
            }
            Err(e) => message_box(window, &format!("保存できませんでした:\n{e:#}"), MB_ICONERROR),
        }
    }

    fn install_extension(&self) {
        let button = self.extension_button.get();
        unsafe {
            let _ = EnableWindow(button, false);
        }
        set_text(button, "インストール中…");
        let window = self.window.get().0 as isize;
        std::thread::spawn(move || {
            let ok = image::install_viewer_extension();
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(window as *mut c_void)),
                    WM_EXTENSION_DONE,
                    WPARAM(usize::from(ok)),
                    LPARAM(0),
                );
            }
        });
    }

    fn extension_done(&self, ok: bool) {
        let button = self.extension_button.get();
        if ok {
            set_text(
                self.hint_label.get(),
                "JPEG XL Image Extension をインストールしました。エクスプローラーとフォトで JPEG XL を表示できます。",
            );
            unsafe {
                let _ = ShowWindow(button, SW_HIDE);
            }
        } else {
            image::open_store_page();
            set_text(
                self.hint_label.get(),
                "自動インストールできなかったため、Microsoft Store のページを開きました。「入手」を押すとインストールされます (無料)。",
            );
            set_text(button, "拡張機能をインストール");
            unsafe {
                let _ = EnableWindow(button, true);
            }
        }
    }
}

/// The line under the quality choices: what the selected level means.
fn quality_note(q: Quality) -> String {
    format!("{} JPEG はどの画質でも劣化なしで変換します。", q.describe())
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Clone the handle so no RefCell borrow is held while modal loops (dialogs) re-enter here.
    let Some(ui) = UI.with_borrow(Clone::clone) else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as u16 {
                id if id == IDOK.0 as u16 => ui.save(),
                id if id == IDCANCEL.0 as u16 => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                ID_ADD_INCLUDE => ui.add_folder(true),
                ID_REMOVE_INCLUDE => ui.remove_selected(true),
                ID_ADD_EXCLUDE => ui.add_folder(false),
                ID_REMOVE_EXCLUDE => ui.remove_selected(false),
                ID_INSTALL_EXTENSION => ui.install_extension(),
                id if (ID_QUALITY..ID_QUALITY + Quality::ALL.len() as u16).contains(&id) => {
                    ui.quality_changed(usize::from(id - ID_QUALITY))
                }
                _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
            LRESULT(0)
        }
        WM_HSCROLL if lparam.0 == ui.slider.get().0 as isize => {
            ui.effort_changed();
            LRESULT(0)
        }
        WM_EXTENSION_DONE => {
            ui.extension_done(wparam.0 == 1);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 >> 16) as u32 & 0xFFFF;
            let r = unsafe { &*(lparam.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            ui.layout(dpi);
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Opens the settings window and returns when it is closed.
pub fn run_settings(config_path: &Path) -> Result<()> {
    ensure_com();
    let cfg = Config::load(config_path).unwrap_or_default();
    unsafe {
        // Also declared in the manifest; this covers builds without it (tests, cargo run).
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
    }
    let instance = unsafe { GetModuleHandleW(None) }?;
    let class_name = w!("OptimanSettings");
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        hbrBackground: unsafe { GetSysColorBrush(COLOR_BTNFACE) },
        lpszClassName: class_name,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        bail!("ウィンドウクラスを登録できません");
    }

    let ui = Rc::new(Ui {
        config_path: config_path.to_path_buf(),
        window: Cell::new(HWND::default()),
        include: RefCell::new(cfg.folder_paths()),
        exclude: RefCell::new(cfg.exclude_folder_paths()),
        effort: Cell::new(cfg.effort.clamp(*EFFORT_RANGE.start(), *EFFORT_RANGE.end())),
        quality: Cell::new(cfg.quality),
        children: RefCell::new(Vec::new()),
        font: Cell::new(HFONT::default()),
        slider: Cell::new(HWND::default()),
        effort_label: Cell::new(HWND::default()),
        quality_note: Cell::new(HWND::default()),
        include_list: Cell::new(HWND::default()),
        exclude_list: Cell::new(HWND::default()),
        hint_label: Cell::new(HWND::default()),
        extension_button: Cell::new(HWND::default()),
    });

    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let dpi = unsafe { GetDpiForSystem() };
    let mut rect = RECT { left: 0, top: 0, right: scale(WIDTH, dpi), bottom: scale(HEIGHT, dpi) };
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut rect, style, false, WS_EX_CONTROLPARENT, dpi);
    }
    let window = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class_name,
            w!("Optiman の設定"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }?;
    ui.window.set(window);
    ui.build();
    let actual = unsafe { GetDpiForWindow(window) };
    if actual != dpi {
        let mut rect = RECT { left: 0, top: 0, right: scale(WIDTH, actual), bottom: scale(HEIGHT, actual) };
        unsafe {
            let _ = AdjustWindowRectExForDpi(&mut rect, style, false, WS_EX_CONTROLPARENT, actual);
            let _ = SetWindowPos(
                window,
                None,
                0,
                0,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
            );
        }
    }
    ui.layout(actual.max(96));
    UI.set(Some(ui.clone()));
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
    }

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        unsafe {
            if !IsDialogMessageW(window, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    UI.set(None);
    let font = ui.font.get();
    if !font.is_invalid() {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
    Ok(())
}
