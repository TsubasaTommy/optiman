//! Stop requests via a named event, waited on by the system thread pool
//! (`RegisterWaitForSingleObject`), so no thread of ours is spent waiting for it.

use std::ffi::c_void;
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, RegisterWaitForSingleObject, ResetEvent, SetEvent,
    UnregisterWaitEx, WT_EXECUTEONLYONCE,
};
use windows::core::{PCWSTR, w};

use crate::event::Msg;

const STOP_EVENT: PCWSTR = w!("Local\\optimize-image-folder-stop");

pub struct StopListener {
    event: HANDLE,
    wait: HANDLE,
    ctx: *mut Sender<Msg>,
}

unsafe extern "system" fn on_stop(ctx: *mut c_void, _timed_out: bool) {
    let tx = unsafe { &*(ctx as *const Sender<Msg>) };
    let _ = tx.send(Msg::Stop);
}

pub fn listen_stop(tx: Sender<Msg>) -> Result<StopListener> {
    unsafe {
        let event = CreateEventW(None, true, false, STOP_EVENT).context("停止イベントを作成できません")?;
        let _ = ResetEvent(event);
        let ctx = Box::into_raw(Box::new(tx));
        let mut wait = HANDLE::default();
        if let Err(e) = RegisterWaitForSingleObject(
            &mut wait,
            event,
            Some(on_stop),
            Some(ctx as *const c_void),
            INFINITE,
            WT_EXECUTEONLYONCE,
        ) {
            drop(Box::from_raw(ctx));
            let _ = CloseHandle(event);
            return Err(e).context("停止イベントを待機できません");
        }
        Ok(StopListener { event, wait, ctx })
    }
}

impl Drop for StopListener {
    fn drop(&mut self) {
        unsafe {
            // Blocks until a running callback finishes, then it is safe to free the context.
            let _ = UnregisterWaitEx(self.wait, Some(INVALID_HANDLE_VALUE));
            let _ = CloseHandle(self.event);
            drop(Box::from_raw(self.ctx));
        }
    }
}

/// Signals the running agent to stop. Returns false if no agent is listening.
pub fn request_stop() -> bool {
    unsafe {
        match OpenEventW(EVENT_MODIFY_STATE, false, STOP_EVENT) {
            Ok(event) => {
                let ok = SetEvent(event).is_ok();
                let _ = CloseHandle(event);
                ok
            }
            Err(_) => false,
        }
    }
}
