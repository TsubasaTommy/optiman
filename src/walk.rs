//! Recursive directory walk honouring the name filter (used for catch-up scans and `convert`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::filter::{Filter, TEMP_SUFFIX};

/// Calls `f` with every candidate photo below `dir` (which must be inside `root`) and its
/// metadata from the directory listing (no extra lookup on Windows). Directories are filtered on
/// the way down, so files only need the extension check. Stale temporary files from an
/// interrupted conversion are removed along the way.
pub fn walk(root: &Path, dir: &Path, filter: &Filter, f: &mut dyn FnMut(PathBuf, fs::Metadata)) {
    if !filter.accepts_dir(root, dir) {
        return;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_file() && name.starts_with('.') && name.ends_with(TEMP_SUFFIX) {
                let _ = fs::remove_file(entry.path());
                continue;
            }
            if filter.is_excluded_name(&name) {
                continue;
            }
            let path = entry.path();
            if filter.in_excluded_dir(&path) {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file()
                && filter.has_target_extension(&name)
                && let Ok(md) = entry.metadata()
            {
                f(path, md);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn finds_photos_and_cleans_temp() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::create_dir_all(root.join("x.photoslibrary")).unwrap();
        for p in ["1.jpg", "a/2.PNG", "a/b/3.tif", "a/b/notes.txt", "x.photoslibrary/4.jpg", "a/.5.jpg"] {
            fs::write(root.join(p), b"x").unwrap();
        }
        fs::write(root.join("a/.6.jxl.oif-tmp"), b"x").unwrap();
        let filter = Filter::new(&Config::default());
        let mut found = Vec::new();
        walk(root, root, &filter, &mut |p, _| found.push(p.strip_prefix(root).unwrap().to_path_buf()));
        found.sort();
        let expected: Vec<PathBuf> = ["1.jpg", "a/2.PNG", "a/b/3.tif"].iter().map(PathBuf::from).collect();
        assert_eq!(found, expected);
        assert!(!root.join("a/.6.jxl.oif-tmp").exists());
    }
}
