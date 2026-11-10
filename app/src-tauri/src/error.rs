//! [`CommandError`]: what a failed command or transfer reports to the frontend, `{ code, params }`, built from a
//! `lanshare_core::ErrorCode`. The frontend translates it with the key `errors.<code>`.

use lanshare_core::{Error, ErrorCode};
use serde::Serialize;
use serde_json::{json, Value};

/// A stable error code plus the values its message needs, e.g.
/// `{ "code": "INSUFFICIENT_SPACE", "params": { "needed": 10, "available": 2 } }`. Sizes are raw bytes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandError {
    /// The `ErrorCode` name.
    pub code: String,
    /// Values for the message's placeholders; an empty object when there are none.
    pub params: Value,
}

/// What every command returns.
pub type CommandResult<T> = Result<T, CommandError>;

impl From<&ErrorCode> for CommandError {
    fn from(code: &ErrorCode) -> Self {
        let params = match code {
            ErrorCode::VersionMismatch { ours, theirs } => json!({ "ours": ours, "theirs": theirs }),
            ErrorCode::InsufficientSpace { needed, available } => json!({ "needed": needed, "available": available }),
            ErrorCode::PairingLocked { retry_after_secs } => json!({ "seconds": retry_after_secs }),
            ErrorCode::PermissionDenied { folder } => json!({ "folder": folder }),
            ErrorCode::TextTooLong { max_bytes } => json!({ "maxBytes": max_bytes }),
            _ => json!({}),
        };
        Self { code: code.name().to_owned(), params }
    }
}

impl From<Error> for CommandError {
    fn from(error: Error) -> Self {
        tracing::debug!(%error, "command failed");
        Self::from(&error.code())
    }
}

impl CommandError {
    /// An input/output failure in the app itself, such as an unreadable settings file.
    pub fn io(error: &dyn std::fmt::Display) -> Self {
        tracing::warn!(%error, "app input/output error");
        Self::from(&ErrorCode::Io)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lanshare_core::ERROR_CODE_NAMES;

    use super::*;

    #[test]
    fn codes_carry_their_params() {
        let error = CommandError::from(&ErrorCode::InsufficientSpace { needed: 10, available: 2 });
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "code": "INSUFFICIENT_SPACE", "params": { "needed": 10, "available": 2 } })
        );
        assert_eq!(
            CommandError::from(&ErrorCode::PairingLocked { retry_after_secs: 30 }).params,
            json!({ "seconds": 30 })
        );
        assert_eq!(CommandError::from(&ErrorCode::Busy).params, json!({}));
    }

    /// Writes the list of error code names for the frontend's i18n test, and fails if it was out of date.
    #[test]
    fn error_code_list_for_the_frontend_is_current() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/i18n/error-codes.json");
        let expected = format!("{}\n", serde_json::to_string_pretty(&ERROR_CODE_NAMES).unwrap());
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != expected {
            std::fs::write(&path, &expected).unwrap();
            panic!("{} was out of date and has been rewritten; run the tests again", path.display());
        }
    }
}
