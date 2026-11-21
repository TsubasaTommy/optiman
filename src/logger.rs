//! Minimal file logger with size-based rotation. No background thread, no timers.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }
}

struct Sink {
    file: Option<(PathBuf, File)>,
    stderr: bool,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Log to a file (rotated at 1 MiB, one backup kept). `also_stderr` mirrors output to stderr.
pub fn init_file(path: PathBuf, also_stderr: bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Sink { file: Some((path, file)), stderr: also_stderr });
    Ok(())
}

pub fn init_stderr() {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Sink { file: None, stderr: true });
}

pub fn log(level: Level, args: fmt::Arguments<'_>) {
    let line = format!("{} {} {}\n", timestamp(), level.as_str(), args);
    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_mut() {
        None => eprint!("{line}"),
        Some(sink) => {
            if sink.stderr {
                eprint!("{line}");
            }
            if let Some((path, file)) = sink.file.as_mut() {
                if file.metadata().map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
                    let backup = path.with_extension("log.1");
                    let _ = fs::rename(&*path, backup);
                    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&*path) {
                        *file = f;
                    }
                }
                let _ = file.write_all(line.as_bytes());
            }
        }
    }
}

/// UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` without pulling in a date crate.
pub fn timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format_unix(secs)
}

pub fn format_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::logger::log($crate::logger::Level::Info, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::logger::log($crate::logger::Level::Warn, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! error {
    ($($t:tt)*) => { $crate::logger::log($crate::logger::Level::Error, format_args!($($t)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_dates() {
        assert_eq!(format_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_unix(1_795_000_000), "2026-11-18T11:06:40Z");
    }
}
