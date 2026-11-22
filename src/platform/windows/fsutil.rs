//! File-system helpers: OneDrive placeholders, share-mode writer detection, Recycle Bin.

use std::fs::{Metadata, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::SystemTime;

use windows::Win32::Foundation::ERROR_SHARING_VIOLATION;
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_ATTRIBUTE_SYSTEM, FILE_SHARE_READ,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation,
    IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName,
};
use windows::core::PCWSTR;

use super::{ensure_com, wide};

/// Cloud files (OneDrive Files On-Demand) whose data is not on disk; reading would download them.
pub fn is_placeholder(md: &Metadata) -> bool {
    md.file_attributes()
        & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS).0
        != 0
}

pub fn is_hidden(md: &Metadata) -> bool {
    md.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM).0 != 0
}

/// Copying a file on Windows sets a fresh creation time, which marks when it arrived here.
pub fn arrival_time(md: &Metadata) -> SystemTime {
    let created = md.created().unwrap_or(SystemTime::UNIX_EPOCH);
    md.modified().map_or(created, |m| m.max(created))
}

/// True while another process holds the file open for writing (opening it with read-only
/// sharing fails with a sharing violation).
pub fn writer_active(path: &Path) -> bool {
    match OpenOptions::new().read(true).share_mode(FILE_SHARE_READ.0).open(path) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(ERROR_SHARING_VIOLATION.0 as i32),
    }
}

pub fn copy_extra_metadata(_src: &Path, _dst: &Path) {}

/// NTFS journals the rename; nothing extra to flush.
pub fn sync_dir(_dir: &Path) {}

/// Sends a file to the Recycle Bin with IFileOperation (no UI).
pub fn trash(path: &Path) -> io::Result<()> {
    ensure_com();
    let to_io = |e: windows::core::Error| io::Error::other(e.message());
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(to_io)?;
        op.SetOperationFlags(FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOFX_RECYCLEONDELETE)
            .map_err(to_io)?;
        let w = wide(path);
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).map_err(to_io)?;
        op.DeleteItem(&item, None::<&IFileOperationProgressSink>).map_err(to_io)?;
        op.PerformOperations().map_err(to_io)?;
        if op.GetAnyOperationsAborted().map_err(to_io)?.as_bool() {
            return Err(io::Error::other("ごみ箱への移動が中断されました"));
        }
    }
    Ok(())
}
