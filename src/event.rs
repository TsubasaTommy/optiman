//! Messages delivered to the daemon's single blocking receive loop.

use std::path::PathBuf;

use crate::convert::Outcome;

#[derive(Debug)]
pub enum Msg {
    /// A file was created, modified or renamed into place.
    Changed(PathBuf),
    /// The watcher lost events for this directory (overflow, history gap); scan it again.
    Rescan(PathBuf),
    /// A directory appeared (created, copied or moved in); its contents produce no events of their own.
    NewDir(PathBuf),
    /// macOS: newest FSEvents event id delivered so far (persisted once fully processed).
    EventId(u64),
    /// macOS: replay of historical events (`sinceWhen`) finished.
    HistoryDone,
    /// An encoder worker finished a file.
    JobFinished(PathBuf, Outcome),
    /// An encoder worker thread exited because the queue was empty.
    WorkerExited,
    /// Stop requested (Windows named event, or watcher failure).
    Stop,
}
