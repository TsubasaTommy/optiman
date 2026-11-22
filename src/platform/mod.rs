//! OS-specific code. Each platform module exposes the same submodules:
//! `paths`, `events`, `image`, `fsutil`, `power`, `autostart`, `ui` and `update`.

use std::fs::{File, FileTimes, Metadata};
use std::io;
use std::path::PathBuf;

/// Copies the modified, accessed and created times of `src` onto `dst`.
pub fn copy_times(dst: &File, src: &Metadata) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    use std::os::macos::fs::FileTimesExt;
    #[cfg(windows)]
    use std::os::windows::fs::FileTimesExt;
    let mut t = FileTimes::new();
    if let Ok(m) = src.modified() {
        t = t.set_modified(m);
    }
    if let Ok(a) = src.accessed() {
        t = t.set_accessed(a);
    }
    if let Ok(c) = src.created() {
        t = t.set_created(c);
    }
    dst.set_times(t)
}

/// What `autostart::install` needs to register the agent.
pub struct AgentSpec {
    pub exe: PathBuf,
    pub config: PathBuf,
    pub log_dir: PathBuf,
    pub folders: Vec<PathBuf>,
    /// Also schedule the daily update check (macOS; the Windows agent runs it itself).
    pub auto_update: bool,
}

cfg_select! {
    target_os = "macos" => {
        mod macos;
        pub use macos::{autostart, events, fsutil, image, paths, power, ui, update};
    }
    windows => {
        mod windows;
        pub use self::windows::{autostart, events, fsutil, image, paths, power, ui, update};
    }
    _ => {
        compile_error!("optiman supports macOS and Windows only");
    }
}
