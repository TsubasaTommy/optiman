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
