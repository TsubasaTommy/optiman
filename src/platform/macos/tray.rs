//! The menu bar icon: a small process launchd starts at login (`optiman tray`). It only waits in
//! the AppKit run loop; clicking the icon opens the settings app.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage, NSStatusBar, NSVariableStatusItemLength};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSSize, NSString};

use super::autostart;

/// `scripts/render_icon.py`: 36 px, shown at 18 pt as a template (tinted to match the menu bar).
const ICON: &[u8] = include_bytes!("../../../assets/menubar.png");

pub struct Ivars {
    config_path: PathBuf,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Target does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            if let Err(e) = open_settings(&self.ivars().config_path) {
                crate::warn!("設定画面を開けません: {e:#}");
            }
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker, config_path: &Path) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { config_path: config_path.to_path_buf() });
        unsafe { msg_send![super(this), init] }
    }
}

/// The settings app (one window, brought to the front if already open); the command line for a
/// non-standard config, which the app would not use.
fn open_settings(config_path: &Path) -> Result<()> {
    let app = autostart::app_bundle_path();
    let mut command = if config_path == crate::app::default_config_path() && app.exists() {
        let mut open = Command::new("/usr/bin/open");
        open.arg(app);
        open
    } else {
        let mut cli = Command::new(std::env::current_exe()?);
        cli.arg("--config").arg(config_path).arg("settings");
        cli
    };
    command.spawn().context("起動できません")?;
    Ok(())
}

/// Shows the icon and waits until the process is stopped.
pub fn run(config_path: &Path) -> Result<()> {
    let mtm = MainThreadMarker::new().context("メニューバーのアイコンはメインスレッドで動かす必要があります")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let button = item.button(mtm).context("メニューバーにアイコンを置けません")?;
    if let Some(icon) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(ICON)) {
        icon.setSize(NSSize::new(18.0, 18.0));
        icon.setTemplate(true);
        button.setImage(Some(&icon));
    }
    button.setToolTip(Some(&NSString::from_str(crate::app::ui_text::TITLE)));
    let target = Target::new(mtm, config_path);
    unsafe {
        button.setTarget(Some(&target));
        button.setAction(Some(sel!(openSettings:)));
    }
    app.run();
    Ok(())
}
