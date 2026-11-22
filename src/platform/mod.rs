//! OS-specific code. Each platform module exposes the same submodules:
//! `paths`, `events`, `image`, `fsutil`, `power`, `autostart` and `ui`.

use std::fs::{File, FileTimes, Metadata};
use std::io;
use std::path::{Path, PathBuf};

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
pub struct AgentSpec<'a> {
    pub exe: &'a Path,
    pub config: &'a Path,
    pub log_dir: &'a Path,
    pub folders: &'a [PathBuf],
}

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
