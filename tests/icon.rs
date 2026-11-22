//! Both Windows executables carry the app icon (Explorer, the taskbar and the Start menu show it).
#![cfg(windows)]

use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW};
use windows::core::{HSTRING, PCWSTR};

#[test]
fn executables_carry_the_app_icon() {
    for exe in [env!("CARGO_BIN_EXE_optiman"), env!("CARGO_BIN_EXE_optiman-agent")] {
        unsafe {
            let module = LoadLibraryExW(&HSTRING::from(exe), None, LOAD_LIBRARY_AS_DATAFILE).unwrap();
            for size in [16, 32, 256] {
                // MAKEINTRESOURCE(1): the icon group build.rs embeds.
                let icon = LoadImageW(
                    Some(module.into()),
                    PCWSTR(std::ptr::without_provenance(1)),
                    IMAGE_ICON,
                    size,
                    size,
                    LR_DEFAULTCOLOR,
                )
                .unwrap_or_else(|e| panic!("{exe}: no {size} px icon: {e}"));
                let _ = DestroyIcon(HICON(icon.0));
            }
            let _ = FreeLibrary(module);
        }
    }
}
