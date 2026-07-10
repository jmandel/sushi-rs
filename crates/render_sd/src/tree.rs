//! TreeSource — the render layer's read seam (the `PackageSource` precedent
//! applied to `render_sd`/`render_page`).
//!
//! `IgContext`, `SiteData` and `PageProvider` are *lazily* file-coupled: they
//! read resource JSONs, package indexes, tx-cache bodies, `_data` and
//! `_includes` at RENDER time, not just at load. Natively those reads are
//! plain `std::fs` (`FsTree`, byte-for-byte the old behavior — every native
//! gate is unchanged by construction). In the browser session the same paths
//! resolve against an in-memory tree (`MemTree`) assembled from the compiled
//! project, the mounted package bundles, the template bundle and the IG's own
//! VFS inputs.
//!
//! Paths are used EXACTLY as the pre-seam code passed them to `std::fs` — the
//! tree is keyed by whatever `PathBuf`s the callers construct. `MemTree`
//! normalizes only redundant `./` components; it does NOT resolve `..` (no
//! caller produces them).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

/// A directory entry: (file_name, is_file).
pub type DirEntry = (String, bool);

pub trait TreeSource {
    /// UTF-8 text read (`std::fs::read_to_string` equivalent).
    fn read(&self, path: &Path) -> Option<String>;
    /// Raw byte read (`std::fs::read` equivalent).
    fn read_bytes(&self, path: &Path) -> Option<Vec<u8>>;
    /// Flat directory listing (`std::fs::read_dir` equivalent): entry names +
    /// is_file. `None` if the directory does not exist.
    fn read_dir(&self, path: &Path) -> Option<Vec<DirEntry>>;
}

/// The native passthrough — byte-identical to the pre-seam `std::fs` calls.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsTree;

impl TreeSource for FsTree {
    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
    fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }
    fn read_dir(&self, path: &Path) -> Option<Vec<DirEntry>> {
        let rd = std::fs::read_dir(path).ok()?;
        let mut out = Vec::new();
        for e in rd.flatten() {
            let is_file = e.file_type().map(|t| t.is_file()).unwrap_or(false);
            out.push((e.file_name().to_string_lossy().to_string(), is_file));
        }
        Some(out)
    }
}

/// An in-memory tree keyed by normalized paths (the wasm/session source).
#[derive(Debug, Default)]
pub struct MemTree {
    files: HashMap<PathBuf, Vec<u8>>,
}

fn normalize(p: &Path) -> PathBuf {
    // Strip `.` components only; keep everything else verbatim (incl. leading
    // `/` and any `..` — no caller produces `..`).
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl MemTree {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, path: impl Into<PathBuf>, bytes: Vec<u8>) {
        self.files.insert(normalize(&path.into()), bytes);
    }
    pub fn insert_text(&mut self, path: impl Into<PathBuf>, text: &str) {
        self.insert(path, text.as_bytes().to_vec());
    }
    pub fn len(&self) -> usize {
        self.files.len()
    }
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl TreeSource for MemTree {
    fn read(&self, path: &Path) -> Option<String> {
        let b = self.files.get(&normalize(path))?;
        String::from_utf8(b.clone()).ok()
    }
    fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.get(&normalize(path)).cloned()
    }
    fn read_dir(&self, path: &Path) -> Option<Vec<DirEntry>> {
        let dir = normalize(path);
        let mut names: Vec<DirEntry> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut any = false;
        for k in self.files.keys() {
            let Ok(rest) = k.strip_prefix(&dir) else {
                continue;
            };
            any = true;
            let mut comps = rest.components();
            let Some(first) = comps.next() else { continue };
            let name = first.as_os_str().to_string_lossy().to_string();
            let is_file = comps.next().is_none();
            if seen.insert((name.clone(), is_file)) {
                names.push((name, is_file));
            }
        }
        if any {
            Some(names)
        } else {
            None
        }
    }
}

/// Shared handle used throughout the render layer.
pub type Tree = Rc<dyn TreeSource>;

/// The default native tree.
pub fn fs_tree() -> Tree {
    Rc::new(FsTree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memtree_read_and_dir() {
        let mut t = MemTree::new();
        t.insert_text("/own/A-b.json", "{}");
        t.insert_text("/own/sub/C-d.json", "{}");
        assert_eq!(t.read(Path::new("/own/A-b.json")).as_deref(), Some("{}"));
        assert_eq!(t.read(Path::new("/own/./A-b.json")).as_deref(), Some("{}"));
        let mut dir = t.read_dir(Path::new("/own")).unwrap();
        dir.sort();
        assert_eq!(
            dir,
            vec![("A-b.json".to_string(), true), ("sub".to_string(), false)]
        );
        assert!(t.read_dir(Path::new("/nope")).is_none());
    }
}
