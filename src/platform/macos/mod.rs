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

/// The app icon (`scripts/render_icon.py`): the settings app's bundle icon, and the Dock icon when
/// the settings window is opened from the command line.
pub const APP_ICON: &[u8] = include_bytes!("../../../assets/optiman.icns");
