//! Building a multi-entry offer from the files and folders the user picked.
//!
//! Each picked file becomes one entry named after it. Each picked folder becomes one entry per file inside it, with
//! `/`-separated paths that start with the folder's name (`Photos/2024/beach.jpg`), plus an entry with a trailing
//! `/` and size 0 for every empty folder (`Photos/empty/`). Entries are sorted, so the same selection always gives
//! the same offer.

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use tracing::debug;

use super::path::{sanitize_file_name, sanitize_relative_path, RelativePath};
use crate::error::{Error, Result};

/// Most entries in one offer. The offer must fit in one 1 MiB frame; 50,000 entries leave about 20 bytes per path.
pub const MAX_ENTRIES: usize = 50_000;

/// One entry of an offer, with where its data comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestEntry {
    /// The path the receiver sees.
    pub relative: RelativePath,
    /// The local file, or `None` for a folder.
    pub source: Option<PathBuf>,
    /// Size in bytes; 0 for folders.
    pub size: u64,
    /// Last modification time, to give a changed file a new transfer id.
    pub modified: SystemTime,
}

/// The entries an offer of `paths` would list, in order: each `/`-separated path (folders end with `/`) and its
/// size. Lets a UI show what is being sent, and which file progress refers to.
///
/// # Errors
///
/// As [`send_files`](super::send_files) for names and unreadable paths.
pub async fn list_entries(paths: &[PathBuf]) -> Result<Vec<(String, u64)>> {
    let paths = paths.to_vec();
    let entries = tokio::task::spawn_blocking(move || build_manifest(&paths))
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))??;
    Ok(entries.into_iter().map(|entry| (entry.relative.as_wire(), entry.size)).collect())
}

/// Walks `paths` into entries. Blocking.
///
/// Symbolic links inside folders, and anything that is neither a file nor a folder, are skipped.
///
/// # Errors
///
/// - [`Error::InvalidFileName`] if a name is not safe to send (see
///   [`sanitize_relative_path`](super::sanitize_relative_path)), two picked items have the same name, or there are
///   more than [`MAX_ENTRIES`] entries
/// - [`Error::Io`] if a path cannot be read
pub(crate) fn build_manifest(paths: &[PathBuf]) -> Result<Vec<ManifestEntry>> {
    let mut entries = Vec::new();
    let mut top_names = HashSet::new();
    for path in paths {
        let name = path.file_name().and_then(|name| name.to_str()).ok_or(Error::InvalidFileName)?;
        let name = sanitize_file_name(name)?;
        if !top_names.insert(name.to_lowercase()) {
            // `a.txt` and `A.txt` would clash on case-insensitive file systems (macOS and Windows).
            return Err(Error::InvalidFileName);
        }
        let metadata = fs::metadata(path).map_err(Error::Io)?;
        if metadata.is_dir() {
            walk(path, &name, &mut entries)?;
        } else if metadata.is_file() {
            entries.push(file_entry(&name, path, &metadata)?);
        } else {
            return Err(Error::Io(io::Error::new(io::ErrorKind::InvalidInput, "not a file or folder")));
        }
        if entries.len() > MAX_ENTRIES {
            return Err(Error::InvalidFileName);
        }
    }
    if entries.is_empty() {
        return Err(Error::Io(io::Error::new(io::ErrorKind::InvalidInput, "nothing to send")));
    }
    Ok(entries)
}

/// Adds the files under `dir` (sorted by name), or one folder entry if it is empty.
fn walk(dir: &Path, relative: &str, entries: &mut Vec<ManifestEntry>) -> Result<()> {
    let mut children: Vec<fs::DirEntry> = fs::read_dir(dir).map_err(Error::Io)?.collect::<io::Result<_>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    let before = entries.len();
    for child in children {
        // `file_type` does not follow symbolic links, so a link is never mistaken for what it points to.
        let file_type = child.file_type().map_err(Error::Io)?;
        let Some(name) = child.file_name().to_str().map(str::to_owned) else {
            return Err(Error::InvalidFileName);
        };
        let child_relative = format!("{relative}/{name}");
        if file_type.is_dir() {
            walk(&child.path(), &child_relative, entries)?;
        } else if file_type.is_file() {
            let metadata = child.metadata().map_err(Error::Io)?;
            entries.push(file_entry(&child_relative, &child.path(), &metadata)?);
        } else {
            debug!(path = %child.path().display(), "skipping a symbolic link or special file");
        }
        if entries.len() > MAX_ENTRIES {
            return Err(Error::InvalidFileName);
        }
    }
    if entries.len() == before {
        let metadata = fs::metadata(dir).map_err(Error::Io)?;
        entries.push(ManifestEntry {
            relative: sanitize_relative_path(&format!("{relative}/"))?,
            source: None,
            size: 0,
            modified: metadata.modified().map_err(Error::Io)?,
        });
    }
    Ok(())
}

fn file_entry(relative: &str, path: &Path, metadata: &fs::Metadata) -> Result<ManifestEntry> {
    Ok(ManifestEntry {
        relative: sanitize_relative_path(relative)?,
        source: Some(fs::canonicalize(path).map_err(Error::Io)?),
        size: metadata.len(),
        modified: metadata.modified().map_err(Error::Io)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_paths(entries: &[ManifestEntry]) -> Vec<String> {
        entries.iter().map(|entry| entry.relative.as_wire()).collect()
    }

    #[test]
    fn files_and_folders_become_sorted_entries() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("Photos");
        fs::create_dir_all(photos.join("2024")).unwrap();
        fs::create_dir_all(photos.join("empty")).unwrap();
        fs::write(photos.join("2024/b.jpg"), b"bb").unwrap();
        fs::write(photos.join("2024/a.jpg"), b"a").unwrap();
        fs::write(photos.join("notes.txt"), b"n").unwrap();
        let single = dir.path().join("report.pdf");
        fs::write(&single, b"pdf").unwrap();

        let entries = build_manifest(&[photos, single]).unwrap();
        assert_eq!(
            wire_paths(&entries),
            ["Photos/2024/a.jpg", "Photos/2024/b.jpg", "Photos/empty/", "Photos/notes.txt", "report.pdf"]
        );
        assert_eq!(entries[1].size, 2);
        assert!(entries[2].source.is_none() && entries[2].relative.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_inside_folders_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("docs");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("real.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", folder.join("link")).unwrap();
        assert_eq!(wire_paths(&build_manifest(&[folder]).unwrap()), ["docs/real.txt"]);
    }

    #[test]
    fn clashing_or_unsafe_names_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("one/a.txt"), dir.path().join("two/A.txt"));
        for path in [&first, &second] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
        assert_eq!(build_manifest(&[first, second]).unwrap_err().code(), crate::ErrorCode::InvalidFileName);
    }

    /// Windows cannot even create such a name, so this only runs elsewhere.
    #[cfg(unix)]
    #[test]
    fn names_windows_cannot_store_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let unsafe_name = dir.path().join("what?.txt");
        fs::write(&unsafe_name, b"x").unwrap();
        assert_eq!(build_manifest(&[unsafe_name]).unwrap_err().code(), crate::ErrorCode::InvalidFileName);
    }
}
