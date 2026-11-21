//! Stop requests. On macOS the agent is managed by launchd (SIGTERM on `bootout`) or Ctrl-C,
//! so no extra listener is needed.

use std::sync::mpsc::Sender;

use anyhow::Result;

use crate::event::Msg;

pub struct StopListener;

pub fn listen_stop(_tx: Sender<Msg>) -> Result<StopListener> {
    Ok(StopListener)
}

/// Ask a running agent that is not managed by launchd to stop. Not supported on macOS.
pub fn request_stop() -> bool {
    false
}
