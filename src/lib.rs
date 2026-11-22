//! Optiman: converts photos in watched folders to JPEG XL.
//!
//! - JPEG is recompressed losslessly (the original file can be rebuilt byte for byte).
//!   PNG/TIFF/BMP/lossless WebP are encoded losslessly by default, or lossily at a chosen
//!   quality level. Every result is verified before the original is touched.
//! - Zero resources while idle: on macOS launchd starts the agent only when a watched folder
//!   changes; on Windows a single thread waits in an I/O completion port (plus, for automatic
//!   updates, one that sleeps between daily checks).

pub mod app;
pub mod cli;
pub mod config;
pub mod convert;
pub mod daemon;
pub mod event;
pub mod exif;
pub mod filter;
pub mod image;
pub mod ipc;
pub mod jxl;
pub mod logger;
pub mod metadata;
pub mod platform;
pub mod restore;
pub mod scheduler;
pub mod state;
pub mod update;
pub mod walk;

/// Command, folder and registration name.
pub const APP_NAME: &str = "optiman";
/// Name shown to people (windows, the app bundle, the installer).
pub const DISPLAY_NAME: &str = "Optiman";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Locks a mutex even if a thread panicked while holding it: every value guarded here stays
/// consistent between statements, so a poisoned lock is still safe to use.
pub(crate) trait LockUnpoisoned<T> {
    fn lock_unpoisoned(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockUnpoisoned<T> for std::sync::Mutex<T> {
    fn lock_unpoisoned(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}
