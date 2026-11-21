//! File-system helpers: cloud placeholders, timestamps, Trash, Finder metadata.

use std::ffi::{CString, c_void};
use std::fs::{File, FileTimes, Metadata};
use std::io;
use std::os::macos::fs::{FileTimesExt, MetadataExt};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::ffi::*;

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

/// Moves a file to the Trash via `-[NSFileManager trashItemAtURL:resultingItemURL:error:]`.
pub fn trash(path: &Path) -> io::Result<()> {
    unsafe {
        let pool = objc_autoreleasePoolPush();
        let result = (|| {
            let cls = objc_getClass(c"NSFileManager".as_ptr());
            if cls.is_null() {
                return Err(io::Error::other("NSFileManager が見つかりません"));
            }
            let send0: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let manager = send0(cls, sel_registerName(c"defaultManager".as_ptr()));
            let url = cf_url(path).ok_or_else(|| io::Error::other("パスを URL に変換できません"))?;
            let send_trash: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *const c_void,
                *mut c_void,
                *mut *mut c_void,
            ) -> i8 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let mut error: *mut c_void = std::ptr::null_mut();
            let ok = send_trash(
                manager,
                sel_registerName(c"trashItemAtURL:resultingItemURL:error:".as_ptr()),
                url.ptr(),
                std::ptr::null_mut(),
                &mut error,
            );
            if ok != 0 { Ok(()) } else { Err(io::Error::other("ゴミ箱へ移動できませんでした")) }
        })();
        objc_autoreleasePoolPop(pool);
        result
    }
}

/// fsync the directory so the rename is durable before the original is removed.
pub fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}
