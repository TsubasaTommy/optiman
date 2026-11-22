//! OS-specific code. Each platform module exposes the same submodules:
//! `paths`, `events`, `image`, `fsutil`, `power`, `autostart` and `ui`.

cfg_select! {
    target_os = "macos" => {
        mod macos;
        pub use macos::{autostart, events, fsutil, image, paths, power, ui};
    }
    windows => {
        mod windows;
        pub use self::windows::{autostart, events, fsutil, image, paths, power, ui};
    }
    _ => {
        compile_error!("optiman supports macOS and Windows only");
    }
}
