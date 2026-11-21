//! FSEvents watcher delivered on a private serial GCD queue.
//!
//! While nothing changes no thread of ours exists for the watcher at all: fseventsd wakes
//! the process only when events arrive (coalesced by `LATENCY`). `IgnoreSelf` suppresses
//! events caused by our own writes and deletions, so converting a file does not wake us again.

use std::ffi::{CStr, OsStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::{Result, bail};

use super::ffi::*;
use crate::event::Msg;

/// Seconds fseventsd may coalesce events before waking us.
const LATENCY: f64 = 1.0;

struct CallbackCtx {
    tx: Sender<Msg>,
}

pub struct Watcher {
    stream: FSEventStreamRef,
    queue: DispatchQueue,
    ctx: *mut CallbackCtx,
}

// The raw handles are only touched from `Drop`, which runs on the owning thread.
unsafe impl Send for Watcher {}

impl Watcher {
    /// `since`: replay events after this FSEvents id (catch-up after a restart).
    pub fn start(roots: &[PathBuf], since: Option<u64>, tx: Sender<Msg>) -> Result<Watcher> {
        if roots.is_empty() {
            bail!("監視するフォルダがありません");
        }
        let paths: Vec<Cf> = roots.iter().map(|r| cf_string(&r.to_string_lossy())).collect();
        let ptrs: Vec<*const c_void> = paths.iter().map(|p| p.ptr()).collect();
        unsafe {
            let array = Cf::from_create(CFArrayCreate(
                kCFAllocatorDefault,
                ptrs.as_ptr(),
                ptrs.len() as CFIndex,
                &kCFTypeArrayCallBacks,
            ))
            .ok_or_else(|| anyhow::anyhow!("CFArrayCreate に失敗"))?;
            let ctx = Box::into_raw(Box::new(CallbackCtx { tx }));
            let mut context = FSEventStreamContext {
                version: 0,
                info: ctx.cast(),
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            let flags = kFSEventStreamCreateFlagFileEvents
                | kFSEventStreamCreateFlagIgnoreSelf
                | kFSEventStreamCreateFlagWatchRoot;
            let stream = FSEventStreamCreate(
                kCFAllocatorDefault,
                callback,
                &mut context,
                array.ptr(),
                since.unwrap_or(kFSEventStreamEventIdSinceNow),
                LATENCY,
                flags,
            );
            if stream.is_null() {
                drop(Box::from_raw(ctx));
                bail!("FSEventStreamCreate に失敗しました");
            }
            let queue = dispatch_queue_create(
                c"io.github.tsubasatommy.optimize-image-folder.fsevents".as_ptr(),
                std::ptr::null(),
            );
            FSEventStreamSetDispatchQueue(stream, queue);
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                dispatch_release(queue);
                drop(Box::from_raw(ctx));
                bail!("FSEventStreamStart に失敗しました");
            }
            Ok(Watcher { stream, queue, ctx })
        }
    }
}

/// Current global FSEvents id, persisted so the next start can replay what it missed.
pub fn current_event_id() -> Option<u64> {
    Some(unsafe { FSEventsGetCurrentEventId() })
}

extern "C" fn drain(_: *mut c_void) {}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            // Wait for any callback still running on the queue before freeing its context.
            dispatch_sync_f(self.queue, std::ptr::null_mut(), drain);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
            drop(Box::from_raw(self.ctx));
        }
    }
}

extern "C" fn callback(
    _stream: FSEventStreamRef,
    info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const u32,
    event_ids: *const u64,
) {
    let ctx = unsafe { &*(info as *const CallbackCtx) };
    let paths = event_paths as *const *const c_char;
    let mut newest = 0u64;
    for i in 0..num_events {
        let (flags, id, path) = unsafe {
            let raw = CStr::from_ptr(*paths.add(i));
            (*event_flags.add(i), *event_ids.add(i), Path::new(OsStr::from_bytes(raw.to_bytes())).to_path_buf())
        };
        if flags & kFSEventStreamEventFlagHistoryDone != 0 {
            let _ = ctx.tx.send(Msg::HistoryDone);
            continue;
        }
        newest = newest.max(id);
        if let Some(msg) = classify(flags, path) {
            let _ = ctx.tx.send(msg);
        }
    }
    if newest > 0 {
        let _ = ctx.tx.send(Msg::EventId(newest));
    }
}

fn classify(flags: u32, path: PathBuf) -> Option<Msg> {
    let dropped = kFSEventStreamEventFlagMustScanSubDirs
        | kFSEventStreamEventFlagUserDropped
        | kFSEventStreamEventFlagKernelDropped;
    if flags & (dropped | kFSEventStreamEventFlagRootChanged) != 0 {
        return Some(Msg::Rescan(path));
    }
    let arrived =
        kFSEventStreamEventFlagItemCreated | kFSEventStreamEventFlagItemRenamed | kFSEventStreamEventFlagItemCloned;
    if flags & kFSEventStreamEventFlagItemIsDir != 0 {
        // A folder moved/copied into the tree only produces one event for the folder itself.
        return (flags & arrived != 0).then_some(Msg::NewDir(path));
    }
    if flags & kFSEventStreamEventFlagItemIsFile != 0 && flags & (arrived | kFSEventStreamEventFlagItemModified) != 0 {
        return Some(Msg::Changed(path));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        let p = PathBuf::from("/x/a.jpg");
        assert!(matches!(
            classify(kFSEventStreamEventFlagItemIsFile | kFSEventStreamEventFlagItemCreated, p.clone()),
            Some(Msg::Changed(_))
        ));
        assert!(matches!(
            classify(kFSEventStreamEventFlagItemIsDir | kFSEventStreamEventFlagItemRenamed, p.clone()),
            Some(Msg::NewDir(_))
        ));
        assert!(matches!(classify(kFSEventStreamEventFlagMustScanSubDirs, p.clone()), Some(Msg::Rescan(_))));
        assert!(classify(kFSEventStreamEventFlagItemIsDir | kFSEventStreamEventFlagItemModified, p.clone()).is_none());
        assert!(classify(kFSEventStreamEventFlagItemIsFile, p).is_none());
    }
}
