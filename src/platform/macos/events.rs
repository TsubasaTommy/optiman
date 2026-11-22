//! macOS event loop. FSEvents callbacks (on a GCD queue), launchd launch events and worker
//! results all arrive on one channel; the main thread blocks in `recv`.
//!
//! In normal use there is no resident process at all: launchd starts the agent when FSEvents
//! reports a change below a watched folder (`LaunchEvents` / `com.apple.fsevents.matching`), and
//! the agent exits again once it is idle.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use anyhow::Result;

use super::watch::Watcher;
use crate::event::Msg;

#[derive(Clone)]
pub struct Notifier(Sender<Msg>);

impl Notifier {
    pub fn send(&self, msg: Msg) -> bool {
        self.0.send(msg).is_ok()
    }
}

pub enum Wait {
    Msg(Msg),
    Timeout,
    Closed,
}

pub struct EventLoop {
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    watcher: Option<Watcher>,
}

impl EventLoop {
    pub fn new() -> Result<(EventLoop, Notifier)> {
        let (tx, rx) = mpsc::channel();
        Ok((EventLoop { rx, tx: tx.clone(), watcher: None }, Notifier(tx)))
    }

    /// Starts FSEvents for `roots`, replaying history after `since`, never reporting `exclude`.
    pub fn watch(&mut self, roots: &[PathBuf], since: Option<u64>, exclude: &[PathBuf]) -> Result<()> {
        self.watcher = Some(Watcher::start(roots, since, exclude, self.tx.clone())?);
        Ok(())
    }

    /// launchd stops the agent with SIGTERM; no control channel is needed.
    pub fn listen_control(&mut self, _config: &std::path::Path) -> Result<()> {
        Ok(())
    }

    /// Consumes the launch events launchd started us for (otherwise it relaunches us).
    pub fn listen_launch_events(&mut self) {
        super::xpc::consume_fsevents_launch_events(Notifier(self.tx.clone()));
    }

    pub fn wait(&mut self, timeout: Option<Duration>) -> Wait {
        match timeout {
            None => self.rx.recv().map_or(Wait::Closed, Wait::Msg),
            Some(t) => match self.rx.recv_timeout(t) {
                Ok(m) => Wait::Msg(m),
                Err(RecvTimeoutError::Timeout) => Wait::Timeout,
                Err(RecvTimeoutError::Disconnected) => Wait::Closed,
            },
        }
    }

    pub fn try_next(&mut self) -> Option<Msg> {
        self.rx.try_recv().ok()
    }

    pub fn stop_watching(&mut self) {
        self.watcher = None;
    }
}

pub fn current_event_id() -> Option<u64> {
    super::watch::current_event_id()
}

/// Ask a running agent to stop. On macOS launchd does this (`launchctl bootout`).
pub fn request_stop(_config: &std::path::Path) -> bool {
    false
}
