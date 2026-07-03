//! `PackageSource` — the storage abstraction under the FHIR package read layer.
//!
//! Every `std::fs` access in the package read path (`.index.json` read,
//! `derived_index::load`/write-once sidecar, the `readdir` version-resolvers, the
//! deep-scan fallback, `PackageContext` fetch) goes through this trait instead of
//! `std::fs` directly. The native impl [`DiskSource`] is byte-for-byte today's
//! behavior; a read-only in-memory impl (see `crates/package_store/src/bundle.rs`)
//! lets the browser mount a prebuilt package bundle without any `std::fs`.
//!
//! Surface (deliberately minimal, mirroring only what the read path needs):
//! - [`PackageSource::read`] — read a file's bytes (replaces `std::fs::read`).
//! - [`PackageSource::read_dir`] — list a directory's entries with a cheap
//!   `is_file` flag (replaces `std::fs::read_dir` + `file_type`).
//! - [`PackageSource::exists`] / [`PackageSource::is_dir`] — presence checks.
//! - [`PackageSource::write_new`] — write-once sidecar support. Writable sources
//!   (disk) create the file atomically; read-only sources return an error and the
//!   caller falls back to an in-memory derive (fail-soft, per the derived-index
//!   design).
//!
//! Paths are the same `PathBuf`s the code already threads around (cache dir joins
//! package `#`-versioned dirs joins resource filenames), so a source is free to key
//! its virtual FS on them. `DiskSource` simply forwards to `std::fs`, so those keys
//! are real disk paths and every path that used to be read still resolves.

use std::io;
use std::path::{Path, PathBuf};

/// One directory entry returned by [`PackageSource::read_dir`]. Carries just the
/// leaf name and whether it is a regular file — the only two things the readdir
/// call sites need (version-resolvers match on name; the deep scan filters on
/// `is_file`).
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// The entry's leaf file name (no parent path).
    pub file_name: String,
    /// True iff the entry is a regular file. `DiskSource` fills this from
    /// `std::fs::FileType::is_file`; entries whose type cannot be determined are
    /// reported `false` (matching the old `unwrap_or(false)` sites).
    pub is_file: bool,
}

/// Read (and write-once) access to a FHIR package cache. Object-safe: the code
/// holds a `&dyn PackageSource`, so the native/browser split is a constructor
/// choice, not a type parameter smeared through the read path.
///
/// `Debug` is a supertrait so the trait object can live in `#[derive(Debug)]`
/// structs (e.g. `snapshot_gen::PackageContext`) without hand-written impls.
pub trait PackageSource: std::fmt::Debug {
    /// Read the full contents of the file at `path`.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// List the entries directly under `path`. Order is unspecified (all call
    /// sites sort or match by name); `DiskSource` returns the OS order, exactly as
    /// the old `read_dir().flatten()` loops did.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;

    /// True iff `path` exists (file or directory).
    fn exists(&self, path: &Path) -> bool;

    /// True iff `path` exists and is a directory. Callers gate package indexing on
    /// this (`pkg_dir.is_dir()`), so a source must answer it faithfully.
    fn is_dir(&self, path: &Path) -> bool;

    /// Write `bytes` to `path` if it does not already exist (write-once). Writable
    /// sources create it atomically and return `Ok(())`; read-only sources return
    /// an `Err` so the caller falls back to an in-memory derive. Never overwrites
    /// an existing file. Default impl = read-only (unsupported).
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let _ = (path, bytes);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "PackageSource is read-only",
        ))
    }
}

// Let `&dyn` / `Box<dyn>` / `Rc<dyn>` transparently be a `PackageSource` too, so
// callers can hold whichever ownership shape fits without extra glue.
impl<T: PackageSource + ?Sized> PackageSource for &T {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        (**self).read(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        (**self).read_dir(path)
    }
    fn exists(&self, path: &Path) -> bool {
        (**self).exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        (**self).is_dir(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        (**self).write_new(path, bytes)
    }
}

/// The native, disk-backed [`PackageSource`]: every method forwards to `std::fs`,
/// so it is byte-for-byte the behavior the read path had before the trait existed.
/// This is the only impl that touches `std::fs`; the trait keeps `std::fs` out of
/// every other read-path site (which is what keeps the wasm build plausible).
#[derive(Debug, Clone, Copy, Default)]
pub struct DiskSource;

impl DiskSource {
    /// Construct the disk source. Zero state — it is a marker for "use `std::fs`".
    pub fn new() -> Self {
        DiskSource
    }
}

impl PackageSource for DiskSource {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for ent in std::fs::read_dir(path)? {
            let Ok(ent) = ent else { continue };
            let is_file = ent.file_type().map(|t| t.is_file()).unwrap_or(false);
            out.push(DirEntry {
                file_name: ent.file_name().to_string_lossy().into_owned(),
                is_file,
            });
        }
        Ok(out)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        // Write-once, atomically, exactly like the old `derived_index::write_once`:
        // skip if present, write to a pid-suffixed temp, rename into place.
        if path.exists() {
            return Ok(());
        }
        let tmp = tmp_sibling(path);
        std::fs::write(&tmp, bytes)?;
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

/// A pid-suffixed temp sibling of `path` (`<path>.tmp.<pid>`), used for the
/// atomic write-once rename. Kept identical to the old sidecar temp naming.
fn tmp_sibling(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp.{pid}"));
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}
