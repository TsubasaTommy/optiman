//! ReadDirectoryChangesW (recursive, overlapped) completed through an I/O completion port.
//!
//! The watcher thread sits in `GetQueuedCompletionStatus(INFINITE)`: a kernel wait that consumes
//! no CPU until the file system reports a change or `Drop` posts a shutdown packet.

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_RENAMED_NEW_NAME, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED, PostQueuedCompletionStatus,
};
use windows::Win32::System::Threading::INFINITE;
use windows::core::PCWSTR;

use super::wide;
use crate::event::Msg;

const BUF_BYTES: usize = 64 * 1024;
const SHUTDOWN_KEY: usize = 0;

struct Dir {
    root: PathBuf,
    handle: HANDLE,
    overlapped: Box<OVERLAPPED>,
    /// u64 storage keeps the buffer DWORD-aligned as ReadDirectoryChangesW requires.
    buf: Vec<u64>,
    armed: bool,
}

// Handles and buffers are owned by exactly one thread (moved into the watcher thread).
unsafe impl Send for Dir {}

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

pub struct Watcher {
    port: SendHandle,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    pub fn start(roots: &[PathBuf], _since: Option<u64>, tx: Sender<Msg>) -> Result<Watcher> {
        if roots.is_empty() {
            bail!("監視するフォルダがありません");
        }
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }
            .context("CreateIoCompletionPort に失敗")?;
        let mut dirs = Vec::new();
        for (i, root) in roots.iter().enumerate() {
            let w = wide(root.as_os_str());
            let handle = unsafe {
                CreateFileW(
                    PCWSTR(w.as_ptr()),
                    FILE_LIST_DIRECTORY.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                    None,
                )
            }
            .with_context(|| format!("フォルダを開けません: {}", root.display()))?;
            unsafe { CreateIoCompletionPort(handle, Some(port), i + 1, 0) }.context("CreateIoCompletionPort に失敗")?;
            // OVERLAPPED and the buffer live on the heap, so moving `Dir` keeps the kernel's pointers valid.
            let mut dir = Dir {
                root: root.clone(),
                handle,
                overlapped: Box::default(),
                buf: vec![0u64; BUF_BYTES / 8],
                armed: false,
            };
            arm(&mut dir).with_context(|| format!("監視を開始できません: {}", root.display()))?;
            dirs.push(dir);
        }
        let p = SendHandle(port);
        let thread = thread::Builder::new()
            .name("oif-watch".into())
            .stack_size(256 * 1024)
            .spawn(move || watch_loop(p, dirs, tx))
            .context("監視スレッドを作成できません")?;
        Ok(Watcher { port: SendHandle(port), thread: Some(thread) })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            let _ = PostQueuedCompletionStatus(self.port.0, 0, SHUTDOWN_KEY, None);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(self.port.0);
        }
    }
}

/// FSEvents-style replay does not exist on Windows; catch-up uses timestamps instead.
pub fn current_event_id() -> Option<u64> {
    None
}

fn arm(d: &mut Dir) -> windows::core::Result<()> {
    *d.overlapped = OVERLAPPED::default();
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE;
    let r = unsafe {
        ReadDirectoryChangesW(
            d.handle,
            d.buf.as_mut_ptr() as *mut c_void,
            BUF_BYTES as u32,
            true,
            filter,
            None,
            Some(&mut *d.overlapped as *mut OVERLAPPED),
            None,
        )
    };
    d.armed = r.is_ok();
    r
}

fn watch_loop(port: SendHandle, mut dirs: Vec<Dir>, tx: Sender<Msg>) {
    loop {
        let mut bytes = 0u32;
        let mut key = 0usize;
        let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
        let r = unsafe { GetQueuedCompletionStatus(port.0, &mut bytes, &mut key, &mut ov, INFINITE) };
        if key == SHUTDOWN_KEY {
            break;
        }
        let Some(d) = dirs.get_mut(key - 1) else { continue };
        d.armed = false;
        match r {
            Err(e) => {
                crate::warn!("フォルダの監視でエラー: {} ({e})", d.root.display());
                let _ = tx.send(Msg::Rescan(d.root.clone()));
            }
            Ok(()) if bytes == 0 => {
                // Buffer overflow: changes were lost, rescan this tree.
                let _ = tx.send(Msg::Rescan(d.root.clone()));
            }
            Ok(()) => {
                let data = unsafe { std::slice::from_raw_parts(d.buf.as_ptr() as *const u8, bytes as usize) };
                for msg in parse(data, &d.root) {
                    let _ = tx.send(msg);
                }
            }
        }
        if let Err(e) = arm(d) {
            crate::error!("フォルダの監視を再開できません: {} ({e})", d.root.display());
        }
    }
    shutdown(port, dirs);
}

/// Cancel outstanding reads and wait for their completions before the buffers are freed.
fn shutdown(port: SendHandle, dirs: Vec<Dir>) {
    let mut outstanding = 0;
    for d in &dirs {
        if d.armed {
            outstanding += 1;
            unsafe {
                let _ = CancelIoEx(d.handle, None);
            }
        }
    }
    while outstanding > 0 {
        let mut bytes = 0u32;
        let mut key = 0usize;
        let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
        let r = unsafe { GetQueuedCompletionStatus(port.0, &mut bytes, &mut key, &mut ov, 5000) };
        if r.is_err() && ov.is_null() {
            // Timed out: the kernel may still own the buffers, so never free them.
            std::mem::forget(dirs);
            return;
        }
        if key != SHUTDOWN_KEY {
            outstanding -= 1;
        }
    }
    for d in &dirs {
        unsafe {
            let _ = CloseHandle(d.handle);
        }
    }
}

/// Decodes a FILE_NOTIFY_INFORMATION chain.
fn parse(data: &[u8], root: &Path) -> Vec<Msg> {
    let mut out = Vec::new();
    let mut off = 0usize;
    let rd = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    while let (Some(next), Some(action), Some(len)) = (rd(off), rd(off + 4), rd(off + 8)) {
        let start = off + 12;
        let Some(name) = data.get(start..start + len as usize) else { break };
        let units: Vec<u16> = name.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let path = root.join(OsString::from_wide(&units));
        if action == FILE_ACTION_ADDED.0 || action == FILE_ACTION_RENAMED_NEW_NAME.0 {
            out.push(if path.is_dir() { Msg::NewDir(path) } else { Msg::Changed(path) });
        } else if action == FILE_ACTION_MODIFIED.0 && !path.is_dir() {
            out.push(Msg::Changed(path));
        }
        if next == 0 {
            break;
        }
        off += next as usize;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(action: u32, name: &str, last: bool) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut len = 12 + units.len() * 2;
        len = (len + 3) & !3;
        let mut r = Vec::new();
        r.extend_from_slice(&(if last { 0 } else { len as u32 }).to_le_bytes());
        r.extend_from_slice(&action.to_le_bytes());
        r.extend_from_slice(&((units.len() * 2) as u32).to_le_bytes());
        for u in units {
            r.extend_from_slice(&u.to_le_bytes());
        }
        r.resize(len, 0);
        r
    }

    #[test]
    fn parses_notify_chain() {
        let mut data = record(FILE_ACTION_ADDED.0, "a\\写真.jpg", false);
        data.extend(record(2, "gone.jpg", false)); // REMOVED: ignored
        data.extend(record(FILE_ACTION_RENAMED_NEW_NAME.0, "b.png", true));
        let msgs = parse(&data, Path::new("C:\\nonexistent-root"));
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], Msg::Changed(p) if p.ends_with("写真.jpg")));
        assert!(matches!(&msgs[1], Msg::Changed(p) if p.ends_with("b.png")));
    }
}
