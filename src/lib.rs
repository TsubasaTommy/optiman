//! optimize-image-folder: converts photos in watched folders to JPEG XL without any loss.
//!
//! - JPEG is recompressed losslessly (the original file can be rebuilt byte for byte);
//!   PNG/TIFF/BMP/lossless WebP are encoded losslessly. Every result is verified before the
//!   original is touched.
//! - Zero resources while idle: on macOS launchd starts the agent only when a watched folder
//!   changes; on Windows a single thread waits in an I/O completion port.

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
pub mod walk;

pub const APP_NAME: &str = "optimize-image-folder";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
