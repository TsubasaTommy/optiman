//! OS-specific code. Each platform module exposes the same submodules:
//! `paths`, `watch`, `image`, `fsutil`, `power`, `autostart`, `instance` and `ThreadInit`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primaries {
    Srgb,
    DisplayP3,
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{ThreadInit, autostart, fsutil, image, instance, paths, power, watch};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{ThreadInit, autostart, fsutil, image, instance, paths, power, watch};

#[cfg(not(any(target_os = "macos", windows)))]
compile_error!("optimize-image-folder supports macOS and Windows only");
