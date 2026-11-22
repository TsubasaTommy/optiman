//! File-system helpers: cloud placeholders, timestamps, Trash, Finder metadata.

use std::ffi::CString;
use std::fs::{File, FileTimes, Metadata};
use std::io;
use std::os::macos::fs::{FileTimesExt, MetadataExt};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFileManager, NSURL};

const SF_DATALESS: u32 = 0x4000_0000;
const UF_HIDDEN: u32 = 0x0000_8000;

/// iCloud Drive / File Provider files that are not downloaded. Reading them would trigger a download.
pub fn is_placeholder(md: &Metadata) -> bool {
    md.st_flags() & SF_DATALESS != 0
}

pub fn is_hidden(md: &Metadata) -> bool {
    md.st_flags() & UF_HIDDEN != 0
}

/// When the file appeared at its current location. `ctime` changes on create/rename/copy and
/// cannot be set by user space, unlike mtime/birthtime which copies preserve.
pub fn arrival_time(md: &Metadata) -> SystemTime {
    let ctime =
        UNIX_EPOCH + Duration::new(md.st_ctime().max(0) as u64, md.st_ctime_nsec().clamp(0, 999_999_999) as u32);
    md.modified().map_or(ctime, |m| m.max(ctime))
}

/// macOS has no mandatory share locks; completion is judged by the settle period alone.
pub fn writer_active(_path: &Path) -> bool {
    false
}

/// Copy modified/accessed/created times from the source.
pub fn copy_times(dst: &File, src: &Metadata) -> io::Result<()> {
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

/// Finder tags and download origin are worth keeping; other xattrs (quarantine, macl, FinderInfo
/// type codes) are deliberately not copied.
const KEEP_XATTRS: &[&std::ffi::CStr] =
    &[c"com.apple.metadata:_kMDItemUserTags", c"com.apple.metadata:kMDItemWhereFroms"];

pub fn copy_extra_metadata(src: &Path, dst: &Path) {
    let (Ok(s), Ok(d)) = (CString::new(src.as_os_str().as_bytes()), CString::new(dst.as_os_str().as_bytes())) else {
        return;
    };
    for name in KEEP_XATTRS {
        unsafe {
            let len = libc::getxattr(s.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, libc::XATTR_NOFOLLOW);
            if len <= 0 {
                continue;
            }
            let mut buf = vec![0u8; len as usize];
            let got =
                libc::getxattr(s.as_ptr(), name.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, libc::XATTR_NOFOLLOW);
            if got > 0 {
                libc::setxattr(d.as_ptr(), name.as_ptr(), buf.as_ptr().cast(), got as usize, 0, libc::XATTR_NOFOLLOW);
            }
        }
    }
}

/// Moves a file to the Trash (`-[NSFileManager trashItemAtURL:resultingItemURL:error:]`).
pub fn trash(path: &Path) -> io::Result<()> {
    autoreleasepool(|_| {
        let url = NSURL::from_file_path(path).ok_or_else(|| io::Error::other("パスを URL に変換できません"))?;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|e| io::Error::other(format!("ゴミ箱へ移動できませんでした: {}", e.localizedDescription())))
    })
}

/// fsync the directory so the rename is durable before the original is removed.
pub fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}
