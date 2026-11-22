//! OS-specific code. Each platform module exposes the same submodules:
//! `paths`, `events`, `image`, `fsutil`, `power`, `autostart`, `ui` and `ThreadInit`.

cfg_select! {
    target_os = "macos" => {
        mod macos;
        pub use macos::{ThreadInit, autostart, events, fsutil, image, paths, power, ui};
    }
    windows => {
        mod windows;
        pub use self::windows::{ThreadInit, autostart, events, fsutil, image, paths, power, ui};
    }
    _ => {
        compile_error!("optimize-image-folder supports macOS and Windows only");
    }
}
