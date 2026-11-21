//! optimize-image-folder: watches photo folders and converts new photos to AV1 (AVIF).
//!
//! Design goals:
//! - Talk to the OS directly (FSEvents / ImageIO on macOS, ReadDirectoryChangesW / WIC on Windows).
//! - Zero CPU while idle: every thread blocks in a kernel wait (GCD, IOCP, channel recv); there are
//!   no timers or polling loops unless work is pending, and encoder threads exist only while
//!   there is something to encode.

pub mod cli;
pub mod config;
pub mod convert;
pub mod daemon;
pub mod encode;
pub mod event;
pub mod exif;
pub mod filter;
pub mod ipc;
pub mod logger;
pub mod metadata;
pub mod platform;
pub mod scheduler;
pub mod state;
pub mod walk;

pub const APP_NAME: &str = "optimize-image-folder";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
