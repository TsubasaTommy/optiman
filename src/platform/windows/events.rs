//! Windows event loop on a single I/O completion port, waited on by the main thread only:
//! - `ReadDirectoryChangesW` completions for every watched folder (recursive, overlapped),
//! - a connection on the control pipe (stop requests from the CLI/settings window),
//! - messages posted by encoder threads (`PostQueuedCompletionStatus`).
//!
//! While idle the process has exactly one thread, blocked in `GetQueuedCompletionStatus`.

use std::collections::VecDeque;
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_RENAMED_NEW_NAME, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
    ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED, PostQueuedCompletionStatus,
};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcessId, INFINITE};
use windows::core::PCWSTR;

use super::wide;
use crate::APP_NAME;
use crate::event::{Msg, Wait};

const BUF_BYTES: usize = 64 * 1024;
const MSG_KEY: usize = usize::MAX;
const PIPE_KEY: usize = usize::MAX - 1;

struct Port(HANDLE);
// The port handle is used concurrently only through thread-safe kernel calls.
unsafe impl Send for Port {}
unsafe impl Sync for Port {}
impl Drop for Port {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Posts messages into the event loop's completion port (used by encoder threads).
#[derive(Clone)]
pub struct Notifier(Arc<Port>);

impl Notifier {
    pub fn send(&self, msg: Msg) -> bool {
        let raw = Box::into_raw(Box::new(msg));
        let ok = unsafe { PostQueuedCompletionStatus(self.0.0, 0, MSG_KEY, Some(raw as *const OVERLAPPED)) }.is_ok();
        if !ok {
            drop(unsafe { Box::from_raw(raw) });
        }
        ok
    }
}

struct Dir {
    root: PathBuf,
    handle: HANDLE,
    // Heap-allocated so the kernel's pointers stay valid when `Dir` moves.
    overlapped: Box<OVERLAPPED>,
    buf: Vec<u64>, // u64 storage keeps the buffer DWORD-aligned
    armed: bool,
}

struct Pipe {
    handle: HANDLE,
    overlapped: Box<OVERLAPPED>,
}

pub struct EventLoop {
    port: Arc<Port>,
    dirs: Vec<Dir>,
    pipe: Option<Pipe>,
    pending: VecDeque<Msg>,
}

/// One control pipe per logon session and config file, so agents for different configs (tests,
/// several users on one machine) never collide.
fn pipe_name(config: &Path) -> Vec<u16> {
    let mut session = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
    }
    let config = std::fs::canonicalize(config).unwrap_or_else(|_| config.to_path_buf());
    // FNV-1a: stable across builds, unlike std's hasher.
    let hash = config
        .to_string_lossy()
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    wide(format!(r"\\.\pipe\{APP_NAME}-{session}-{hash:016x}"))
}

impl EventLoop {
    pub fn new() -> Result<(EventLoop, Notifier)> {
        let handle = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }
            .context("CreateIoCompletionPort に失敗")?;
        let port = Arc::new(Port(handle));
        let notifier = Notifier(Arc::clone(&port));
        Ok((EventLoop { port, dirs: Vec::new(), pipe: None, pending: VecDeque::new() }, notifier))
    }

    pub fn watch(&mut self, roots: &[PathBuf], _since: Option<u64>, _exclude: &[PathBuf]) -> Result<()> {
        if roots.is_empty() {
            bail!("監視するフォルダがありません");
        }
        for root in roots {
            let w = wide(root);
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
            let key = self.dirs.len() + 1;
            unsafe { CreateIoCompletionPort(handle, Some(self.port.0), key, 0) }
                .context("CreateIoCompletionPort に失敗")?;
            let mut dir = Dir {
                root: root.clone(),
                handle,
                overlapped: Box::default(),
                buf: vec![0u64; BUF_BYTES / 8],
                armed: false,
            };
            arm(&mut dir).with_context(|| format!("監視を開始できません: {}", root.display()))?;
            self.dirs.push(dir);
        }
        Ok(())
    }

    /// Stop requests arrive as a connection on a per-session named pipe (no thread waits for it).
    pub fn listen_control(&mut self, config: &Path) -> Result<()> {
        let name = pipe_name(config);
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(
                    PIPE_ACCESS_INBOUND.0 | FILE_FLAG_OVERLAPPED.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0,
                ),
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                0,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            bail!("制御用パイプを作成できません (すでに起動している可能性があります)");
        }
        unsafe { CreateIoCompletionPort(handle, Some(self.port.0), PIPE_KEY, 0) }
            .context("CreateIoCompletionPort に失敗")?;
        let mut pipe = Pipe { handle, overlapped: Box::default() };
        match unsafe { ConnectNamedPipe(handle, Some(&mut *pipe.overlapped as *mut OVERLAPPED)) } {
            Ok(()) => self.pending.push_back(Msg::Stop),
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => self.pending.push_back(Msg::Stop),
            Err(e) => return Err(e).context("制御用パイプを待機できません"),
        }
        self.pipe = Some(pipe);
        Ok(())
    }

    /// launchd-style launch events do not exist on Windows.
    pub fn listen_launch_events(&mut self) {}

    pub fn wait(&mut self, timeout: Option<Duration>) -> Wait {
        let ms = timeout.map_or(INFINITE, |t| u32::try_from(t.as_millis()).unwrap_or(INFINITE - 1));
        self.next(ms)
    }

    pub fn try_next(&mut self) -> Option<Msg> {
        match self.next(0) {
            Wait::Msg(m) => Some(m),
            Wait::Timeout => None,
        }
    }

    fn next(&mut self, ms: u32) -> Wait {
        loop {
            if let Some(m) = self.pending.pop_front() {
                return Wait::Msg(m);
            }
            let mut bytes = 0u32;
            let mut key = 0usize;
            let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
            let r = unsafe { GetQueuedCompletionStatus(self.port.0, &mut bytes, &mut key, &mut ov, ms) };
            if r.is_err() && ov.is_null() {
                return Wait::Timeout; // timed out (or the port is gone)
            }
            match key {
                MSG_KEY => return Wait::Msg(*unsafe { Box::from_raw(ov.cast::<Msg>()) }),
                PIPE_KEY => return Wait::Msg(Msg::Stop),
                k => {
                    let Some(d) = self.dirs.get_mut(k - 1) else { continue };
                    d.armed = false;
                    match r {
                        Err(e) => {
                            crate::warn!("フォルダの監視でエラー: {} ({e})", d.root.display());
                            self.pending.push_back(Msg::Rescan(d.root.clone()));
                        }
                        // Buffer overflow: changes were lost, rescan this tree.
                        Ok(()) if bytes == 0 => self.pending.push_back(Msg::Rescan(d.root.clone())),
                        Ok(()) => {
                            let data =
                                unsafe { std::slice::from_raw_parts(d.buf.as_ptr().cast::<u8>(), bytes as usize) };
                            self.pending.extend(parse(data, &d.root));
                        }
                    }
                    if let Err(e) = arm(d) {
                        crate::error!("フォルダの監視を再開できません: {} ({e})", d.root.display());
                    }
                }
            }
        }
    }

    /// Cancels outstanding reads and waits for their completions before buffers are freed.
    pub fn stop_watching(&mut self) {
        let mut outstanding = 0;
        for d in &self.dirs {
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
            let r = unsafe { GetQueuedCompletionStatus(self.port.0, &mut bytes, &mut key, &mut ov, 5000) };
            if r.is_err() && ov.is_null() {
                // Timed out: the kernel may still own the buffers, so never free them.
                std::mem::forget(std::mem::take(&mut self.dirs));
                return;
            }
            match key {
                MSG_KEY => self.pending.push_back(*unsafe { Box::from_raw(ov.cast::<Msg>()) }),
                PIPE_KEY => {}
                _ => outstanding -= 1,
            }
        }
        for d in self.dirs.drain(..) {
            unsafe {
                let _ = CloseHandle(d.handle);
            }
        }
    }
}

impl Drop for EventLoop {
    fn drop(&mut self) {
        self.stop_watching();
        if let Some(pipe) = self.pipe.take() {
            unsafe {
                let _ = CancelIoEx(pipe.handle, None);
                let _ = CloseHandle(pipe.handle);
            }
            // The cancelled connect may still reference the OVERLAPPED; keep it alive.
            std::mem::forget(pipe.overlapped);
        }
    }
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
            d.buf.as_mut_ptr().cast::<c_void>(),
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

/// Decodes a FILE_NOTIFY_INFORMATION chain.
fn parse(data: &[u8], root: &Path) -> Vec<Msg> {
    let mut out = Vec::new();
    let mut off = 0usize;
    let rd = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    while let (Some(next), Some(action), Some(len)) = (rd(off), rd(off + 4), rd(off + 8)) {
        let start = off + 12;
        let Some(name) = data.get(start..start + len as usize) else { break };
        let units: Vec<u16> = name.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
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

/// FSEvents-style replay does not exist on Windows; catch-up uses timestamps instead.
pub fn current_event_id() -> Option<u64> {
    None
}

/// Asks the running agent to stop by connecting to its control pipe.
pub fn request_stop(config: &Path) -> bool {
    let name = pipe_name(config);
    match unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    } {
        Ok(h) => {
            unsafe {
                let _ = CloseHandle(h);
            }
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(action: u32, name: &str, last: bool) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let len = (12 + units.len() * 2 + 3) & !3;
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

    #[test]
    fn notifier_messages_arrive_through_the_port() {
        let (mut ev, n) = EventLoop::new().unwrap();
        assert!(n.send(Msg::Wake));
        assert!(matches!(ev.wait(Some(Duration::from_secs(1))), Wait::Msg(Msg::Wake)));
        assert!(matches!(ev.wait(Some(Duration::from_millis(10))), Wait::Timeout));
    }

    #[test]
    fn control_pipe_delivers_stop() {
        let (mut ev, _n) = EventLoop::new().unwrap();
        let config = std::env::temp_dir().join(format!("optiman-pipe-test-{}.toml", std::process::id()));
        ev.listen_control(&config).unwrap();
        assert!(!request_stop(Path::new("C:\\some\\other\\config.toml")));
        assert!(request_stop(&config));
        assert!(matches!(ev.wait(Some(Duration::from_secs(2))), Wait::Msg(Msg::Stop)));
    }
}
