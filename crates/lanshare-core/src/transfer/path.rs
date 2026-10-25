//! File name sanitizing, so a peer can never make us write outside the download folder.

use crate::error::{Error, Result};

/// Longest accepted file name in bytes. Most file systems limit names to 255 bytes.
const MAX_FILE_NAME_BYTES: usize = 255;

/// Reduces a received name to a safe, plain file name.
///
/// Directory components are stripped for both `/` and `\` separators, as is a Windows drive prefix such as `C:`.
/// So `../../evil.txt` and `C:\evil.txt` both become `evil.txt`.
///
/// # Errors
///
/// [`Error::InvalidFileName`] if the remaining name is empty, `.` or `..`, contains control characters, or is
/// longer than 255 bytes.
pub fn sanitize_file_name(name: &str) -> Result<String> {
    // `rsplit` walks the parts from the end, so `next()` gives the last component.
    let last_component = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let file_name = strip_drive_prefix(last_component);

    let is_invalid = file_name.is_empty()
        || file_name == "."
        || file_name == ".."
        || file_name.len() > MAX_FILE_NAME_BYTES
        || file_name.chars().any(char::is_control);
    if is_invalid {
        return Err(Error::InvalidFileName);
    }
    Ok(file_name.to_owned())
}

/// Removes a leading `X:` drive prefix, which Windows treats as a path even without a separator.
fn strip_drive_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        &name[2..]
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn assert_invalid(name: &str) {
        let error = sanitize_file_name(name).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidFileName, "{name:?} should be invalid");
    }

    #[test]
    fn plain_names_are_kept() {
        assert_eq!(sanitize_file_name("report.pdf").unwrap(), "report.pdf");
        assert_eq!(sanitize_file_name(".bashrc").unwrap(), ".bashrc");
        assert_eq!(sanitize_file_name("my file (1).tar.gz").unwrap(), "my file (1).tar.gz");
        assert_eq!(sanitize_file_name("ឯកសារ.txt").unwrap(), "ឯកសារ.txt");
        assert_eq!(sanitize_file_name("ملف.txt").unwrap(), "ملف.txt");
    }

    #[test]
    fn parent_traversal_is_stripped() {
        assert_eq!(sanitize_file_name("../../evil.txt").unwrap(), "evil.txt");
        assert_eq!(sanitize_file_name("/etc/passwd").unwrap(), "passwd");
        assert_eq!(sanitize_file_name("dir/sub/file.txt").unwrap(), "file.txt");
    }

    #[test]
    fn windows_paths_are_stripped() {
        assert_eq!(sanitize_file_name("C:\\evil.txt").unwrap(), "evil.txt");
        assert_eq!(sanitize_file_name("C:evil.txt").unwrap(), "evil.txt");
        assert_eq!(sanitize_file_name("..\\..\\Windows\\evil.dll").unwrap(), "evil.dll");
    }

    #[test]
    fn empty_and_dot_names_are_rejected() {
        assert_invalid("");
        assert_invalid(".");
        assert_invalid("..");
        assert_invalid("dir/");
        assert_invalid("dir/..");
        assert_invalid("C:");
        assert_invalid("C:\\");
    }

    #[test]
    fn control_characters_are_rejected() {
        assert_invalid("bad\nname.txt");
        assert_invalid("bad\0name.txt");
        assert_invalid("\u{1b}[31mred.txt");
    }

    #[test]
    fn length_limit_is_in_bytes() {
        assert_eq!(sanitize_file_name(&"a".repeat(255)).unwrap().len(), 255);
        assert_invalid(&"a".repeat(256));
        // 86 three-byte characters = 258 bytes, even though it is only 86 characters.
        assert_invalid(&"ក".repeat(86));
    }
}
