//! Windows: one I/O completion port (ReadDirectoryChangesW, control pipe, worker messages),
//! WIC, Shell, registry, power throttling and a Win32 settings window.

pub mod autostart;
pub mod events;
pub mod fsutil;
pub mod image;
pub mod paths;
pub mod power;
pub mod ui;
pub mod update;

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Registry::{HKEY, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
use windows::core::{PCWSTR, PWSTR};

/// COM for the current thread (WIC, IFileOperation, dialogs); uninitialised when the thread ends.
struct ComGuard {
    initialized: bool,
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static COM: ComGuard = ComGuard {
        initialized: unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok(),
    };
}

/// Initialises COM on whatever thread calls into WIC/Shell, once per thread.
pub(crate) fn ensure_com() {
    COM.with(|_| ());
}

/// A REG_SZ registry value, if present.
pub(crate) fn reg_string(root: HKEY, subkey: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut size = 0u32;
    unsafe {
        RegGetValueW(root, subkey, value, RRF_RT_REG_SZ, None, None, Some(&mut size)).ok().ok()?;
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        RegGetValueW(root, subkey, value, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
            .ok()
            .ok()?;
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

/// Converts and frees a string the callee allocated with `CoTaskMemAlloc`.
///
/// # Safety
/// `p` must come from a Shell/COM API that transfers ownership of the string.
pub(crate) unsafe fn take_co_string(p: PWSTR) -> Option<String> {
    unsafe {
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// NUL-terminated UTF-16 for Win32 calls.
pub(crate) fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Starts a process that outlives this one, without a console and outside our job object when
/// the job allows it.
pub(crate) fn spawn_detached(command: &mut Command) -> io::Result<Child> {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    command
        .creation_flags((base | CREATE_BREAKAWAY_FROM_JOB).0)
        .spawn()
        .or_else(|_| command.creation_flags(base.0).spawn())
}
