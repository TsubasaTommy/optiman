//! macOS: FSEvents, launchd launch events (XPC), ImageIO/CoreGraphics, libdispatch.

pub mod autostart;
pub mod events;
pub(crate) mod ffi;
pub mod fsutil;
pub mod image;
pub mod paths;
pub mod power;
pub mod tray;
pub mod ui;
pub mod update;
pub mod watch;
mod xpc;

use crate::platform::AgentSpec;

/// The app icon (`scripts/render_icon.py`): the settings app's bundle icon, and the Dock icon when
/// the settings window is opened from the command line.
pub const APP_ICON: &[u8] = include_bytes!("../../../assets/optiman.icns");

/// The agent is started on demand, so the menu bar icon and the update check are launchd jobs of
/// their own. After an upgrade this brings them and the settings app in line with this binary.
pub fn on_agent_start(spec: &AgentSpec) {
    if let Err(e) = autostart::refresh(spec) {
        crate::warn!("登録を更新できません: {e:#}");
    }
}

pub fn on_agent_exit() {}
