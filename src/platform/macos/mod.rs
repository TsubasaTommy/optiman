//! macOS: FSEvents, ImageIO/CoreGraphics, libdispatch, launchd.

pub mod autostart;
pub(crate) mod ffi;
pub mod fsutil;
pub mod image;
pub mod instance;
pub mod paths;
pub mod power;
pub mod watch;

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
