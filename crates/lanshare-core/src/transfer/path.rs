//! Path sanitizing, so a peer can never make us write outside the download folder, or create a name that is unsafe
//! or impossible on any of the three operating systems.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Longest accepted name of one path component, in bytes. Most file systems limit names to 255 bytes.
const MAX_COMPONENT_BYTES: usize = 255;

/// Most components in one relative path.
const MAX_DEPTH: usize = 64;

/// Names Windows reserves for devices, in any case and with any extension (`con.txt` is reserved too).
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Characters that are not allowed in a name on Windows. `\` and `:` also rule out other separators, drive
/// letters (`C:`) and NTFS alternate data streams (`file:stream`).
const FORBIDDEN_CHARACTERS: [char; 8] = ['\\', ':', '<', '>', '"', '|', '?', '*'];

/// A checked relative path from an offer, such as `Photos/2024/beach.jpg`, or `Photos/empty/` for a folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelativePath {
    components: Vec<String>,
    is_dir: bool,
}

impl RelativePath {
    /// The `/`-separated form, with a trailing `/` for folders, as sent in offers.
    #[must_use]
    pub fn as_wire(&self) -> String {
        let joined = self.components.join("/");
        if self.is_dir {
            joined + "/"
        } else {
            joined
        }
    }

    /// Whether this is a folder entry (it ended with `/`).
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// The last component: the file or folder name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.components.last().map_or("", String::as_str)
    }

    /// The first component: the top-level file or folder this entry belongs to.
    #[must_use]
    pub fn top(&self) -> &str {
        self.components.first().map_or("", String::as_str)
    }

    /// This path under `base`, using the operating system's separator.
    #[must_use]
    pub fn under(&self, base: &Path) -> PathBuf {
        let mut path = base.to_owned();
        path.extend(&self.components);
        path
    }

    /// The folder this entry is in, under `base`.
    #[must_use]
    pub fn parent_under(&self, base: &Path) -> PathBuf {
        let mut path = base.to_owned();
        path.extend(&self.components[..self.components.len().saturating_sub(1)]);
        path
    }
}

/// Checks a relative path from an offer. Components are separated by `/`; a trailing `/` marks a folder.
///
/// # Errors
///
/// [`Error::InvalidFileName`] if the path is empty or absolute, has more than 64 components, or any component is
/// empty, `.` or `..`, longer than 255 bytes, contains a control character or one of `\ : < > " | ? *`, ends with
/// a dot or a space (Windows would silently drop it), or is a reserved Windows device name such as `CON` or `com1.txt`.
pub fn sanitize_relative_path(path: &str) -> Result<RelativePath> {
    let (body, is_dir) = match path.strip_suffix('/') {
        Some(body) => (body, true),
        None => (path, false),
    };
    if body.is_empty() || body.starts_with('/') {
        return Err(Error::InvalidFileName);
    }
    let components: Vec<String> = body.split('/').map(str::to_owned).collect();
    if components.len() > MAX_DEPTH || !components.iter().all(|component| is_safe_component(component)) {
        return Err(Error::InvalidFileName);
    }
    Ok(RelativePath { components, is_dir })
}

/// Checks one plain name, such as a file name from a local path, with the same rules as each component of
/// [`sanitize_relative_path`].
///
/// # Errors
///
/// [`Error::InvalidFileName`] if the name is not safe.
pub fn sanitize_file_name(name: &str) -> Result<String> {
    if is_safe_component(name) {
        Ok(name.to_owned())
    } else {
        Err(Error::InvalidFileName)
    }
}

fn is_safe_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && component.len() <= MAX_COMPONENT_BYTES
        && !component.ends_with(['.', ' '])
        // `/` can only appear in a single name; split components never contain it.
        && !component.chars().any(|c| c.is_control() || c == '/' || FORBIDDEN_CHARACTERS.contains(&c))
        && !is_reserved(component)
}

/// `CON`, `con.txt`, `Lpt1.tar.gz` and so on.
fn is_reserved(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or_default().trim_end();
    RESERVED_NAMES.iter().any(|reserved| reserved.eq_ignore_ascii_case(stem))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn assert_invalid(path: &str) {
        let error = sanitize_relative_path(path).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidFileName, "{path:?} should be invalid");
    }

    #[test]
    fn plain_paths_are_kept() {
        for path in ["report.pdf", ".bashrc", "my file (1).tar.gz", "ឯកសារ.txt", "ملف.txt", "Photos/2024/beach.jpg"]
        {
            assert_eq!(sanitize_relative_path(path).unwrap().as_wire(), path);
        }
    }

    #[test]
    fn folders_end_with_a_slash() {
        let folder = sanitize_relative_path("Photos/empty/").unwrap();
        assert!(folder.is_dir());
        assert_eq!((folder.name(), folder.top(), folder.as_wire().as_str()), ("empty", "Photos", "Photos/empty/"));
        assert!(!sanitize_relative_path("Photos/a.jpg").unwrap().is_dir());
    }

    #[test]
    fn paths_stay_under_the_base() {
        let path = sanitize_relative_path("Photos/2024/beach.jpg").unwrap();
        let base = Path::new("/downloads");
        assert_eq!(path.under(base), Path::new("/downloads/Photos/2024/beach.jpg"));
        assert_eq!(path.parent_under(base), Path::new("/downloads/Photos/2024"));
    }

    #[test]
    fn parent_references_and_absolute_paths_are_rejected() {
        for path in ["../x", "a/../../x", "..", ".", "a/./b", "/etc/passwd", "a//b", "", "/", "./"] {
            assert_invalid(path);
        }
    }

    #[test]
    fn windows_drives_separators_and_streams_are_rejected() {
        for path in ["C:\\x", "C:x", "c:/x", "a\\b", "..\\x", "file.txt:stream", "a/b:c"] {
            assert_invalid(path);
        }
    }

    #[test]
    fn reserved_windows_names_are_rejected_in_every_component() {
        for path in ["CON/x", "con", "nul.txt", "Photos/AUX/a.jpg", "com1", "LPT9.tar.gz", "prn ", "a/Com5/b"] {
            assert_invalid(path);
        }
        // Names that only start like a reserved one are fine.
        for path in ["CONSOLE.txt", "nullable", "com10", "auxiliary/a"] {
            assert!(sanitize_relative_path(path).is_ok(), "{path:?} should be valid");
        }
    }

    #[test]
    fn unsafe_characters_and_lengths_are_rejected() {
        for path in ["a\nb", "tab\there", "what?", "star*", "pipe|", "quote\"", "<tag>", "dot.", "space "] {
            assert_invalid(path);
        }
        assert_invalid(&"a".repeat(256));
        assert!(sanitize_relative_path(&"a".repeat(255)).is_ok());
        assert_invalid(&vec!["d"; 65].join("/"));
    }

    #[test]
    fn single_names_follow_the_same_rules() {
        assert_eq!(sanitize_file_name("movie.mkv").unwrap(), "movie.mkv");
        for name in ["a/b", "..", "CON", "x:y", ""] {
            assert!(sanitize_file_name(name).is_err(), "{name:?} should be invalid");
        }
    }
}
