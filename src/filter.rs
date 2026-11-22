//! Path filtering: names (hidden files, library bundles, patterns), extensions and the
//! excluded folders (blacklist). No filesystem access.

use std::path::{Component, Path, PathBuf};

use crate::config::Config;
use crate::platform;

/// Suffix of our own in-progress output files (they are also dot-prefixed, i.e. hidden).
pub const TEMP_SUFFIX: &str = ".oif-tmp";

/// Library bundles and system folders that must never be touched.
const BUILTIN_EXCLUDE: &[&str] = &[
    "*.photoslibrary",
    "*.photolibrary",
    "*.aplibrary",
    "*.migratedphotolibrary",
    "*.lrdata",
    "*.lrlibrary",
    "*.lrcat-data",
    "*.app",
    "*.bundle",
    "*.imovielibrary",
    "*.fcpbundle",
    "$recycle.bin",
    "system volume information",
    "@eadir",
    "#recycle",
    "node_modules",
];

#[derive(Debug, Clone)]
pub struct Filter {
    extensions: Vec<String>,
    exclude: Vec<String>,
    /// Canonical excluded folders (config `exclude_folders`).
    excluded_dirs: Vec<PathBuf>,
}

impl Filter {
    pub fn new(cfg: &Config) -> Filter {
        let extensions = cfg.extensions.iter().map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect();
        let exclude =
            BUILTIN_EXCLUDE.iter().map(|s| s.to_string()).chain(cfg.exclude.iter().map(|s| s.to_lowercase())).collect();
        let excluded_dirs =
            cfg.exclude_folder_paths().into_iter().map(|p| platform::paths::canonical(&p).unwrap_or(p)).collect();
        Filter { extensions, exclude, excluded_dirs }
    }

    pub fn excluded_dirs(&self) -> &[PathBuf] {
        &self.excluded_dirs
    }

    /// True if `path` is inside one of the excluded folders.
    pub fn in_excluded_dir(&self, path: &Path) -> bool {
        self.excluded_dirs.iter().any(|d| path.starts_with(d))
    }

    /// True when `path` (somewhere below `root`) is a photo we should convert, judging by name only.
    pub fn accepts(&self, root: &Path, path: &Path) -> bool {
        if self.in_excluded_dir(path) {
            return false;
        }
        let Ok(rel) = path.strip_prefix(root) else { return false };
        let mut last = None;
        for comp in rel.components() {
            let Component::Normal(name) = comp else { return false };
            let name = name.to_string_lossy();
            if self.is_excluded_name(&name) {
                return false;
            }
            last = Some(name);
        }
        let Some(file_name) = last else { return false };
        self.has_target_extension(&file_name)
    }

    /// Directory/file names that are skipped entirely (hidden, library bundles, user excludes).
    pub fn is_excluded_name(&self, name: &str) -> bool {
        if name.starts_with('.') || name.ends_with(TEMP_SUFFIX) {
            return true;
        }
        let lower = name.to_lowercase();
        self.exclude.iter().any(|p| glob_match(p, &lower))
    }

    pub fn has_target_extension(&self, file_name: &str) -> bool {
        match file_name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => {
                let ext = ext.to_ascii_lowercase();
                self.extensions.contains(&ext)
            }
            _ => false,
        }
    }
}

/// Glob with `*` (any run) and `?` (one char). Both inputs are expected lowercase.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn filter() -> Filter {
        Filter::new(&Config {
            exclude: vec!["Private*".into()],
            exclude_folders: vec!["/r/skip-me".into()],
            ..Config::default()
        })
    }

    #[test]
    fn globbing() {
        assert!(glob_match("*.photoslibrary", "photos library.photoslibrary"));
        assert!(glob_match("img_??.jpg", "img_01.jpg"));
        assert!(!glob_match("img_??.jpg", "img_001.jpg"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
    }

    #[test]
    fn accepts_photos_only() {
        let f = filter();
        let root = PathBuf::from("/r");
        assert!(f.accepts(&root, Path::new("/r/2024/IMG_0001.JPG")));
        assert!(f.accepts(&root, Path::new("/r/a.WebP")));
        assert!(!f.accepts(&root, Path::new("/r/a.jxl")));
        assert!(!f.accepts(&root, Path::new("/r/a.heic")));
        assert!(!f.accepts(&root, Path::new("/r/a.CR3")));
        assert!(!f.accepts(&root, Path::new("/r/.jpg")));
        assert!(!f.accepts(&root, Path::new("/r/noext")));
        assert!(!f.accepts(&root, Path::new("/other/a.jpg")));
    }

    #[test]
    fn rejects_hidden_bundles_and_excludes() {
        let f = filter();
        let root = PathBuf::from("/r");
        assert!(!f.accepts(&root, Path::new("/r/.hidden/a.jpg")));
        assert!(!f.accepts(&root, Path::new("/r/Photos Library.photoslibrary/originals/a.jpg")));
        assert!(!f.accepts(&root, Path::new("/r/Lightroom Catalog Previews.lrdata/a.jpg")));
        assert!(!f.accepts(&root, Path::new("/r/$RECYCLE.BIN/a.jpg")));
        assert!(!f.accepts(&root, Path::new("/r/private stuff/a.jpg")));
        assert!(!f.accepts(&root, Path::new("/r/.a.jxl.oif-tmp")));
        assert!(!f.accepts(&root, Path::new("/r/skip-me/deep/a.jpg")));
        assert!(f.accepts(&root, Path::new("/r/skip-me-not/a.jpg")));
    }
}
