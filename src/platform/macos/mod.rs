//! macOS: FSEvents, launchd launch events (XPC), ImageIO/CoreGraphics, libdispatch.

pub mod autostart;
pub mod events;
pub(crate) mod ffi;
pub mod fsutil;
pub mod image;
pub mod paths;
pub mod power;
pub mod ui;
pub mod watch;
mod xpc;

/// Per-thread OS initialisation (nothing needed on macOS).
pub struct ThreadInit;

impl ThreadInit {
    pub fn new() -> ThreadInit {
        ThreadInit
    }
}

impl Default for ThreadInit {
    fn default() -> Self {
        Self::new()
    }
}
