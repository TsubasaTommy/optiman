//! Windows: one I/O completion port (ReadDirectoryChangesW, control pipe, worker messages),
//! WIC, Shell, registry, power throttling and a Win32 settings window.

pub mod autostart;
pub mod events;
pub mod fsutil;
pub mod image;
pub mod paths;
pub mod power;
pub mod ui;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};

/// COM initialisation for the current thread (WIC, IFileOperation).
pub struct ThreadInit {
    initialized: bool,
}

impl ThreadInit {
    pub fn new() -> ThreadInit {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        ThreadInit { initialized: hr.is_ok() }
    }
}

impl Default for ThreadInit {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ThreadInit {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static COM: ThreadInit = ThreadInit::new();
}

/// Lazily initialise COM on whatever thread calls into WIC/Shell (also covers test threads).
pub(crate) fn ensure_com() {
    COM.with(|_| ());
}

/// NUL-terminated UTF-16 for Win32 calls.
pub(crate) fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}
