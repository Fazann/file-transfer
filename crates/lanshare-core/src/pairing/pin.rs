//! The 6-digit pairing PIN.

use std::io;

use ring::rand::{SecureRandom, SystemRandom};

use crate::error::{Error, Result};

/// Number of digits in a PIN.
pub const PIN_LENGTH: usize = 6;

/// One more than the largest PIN (`999999`).
const PIN_RANGE: u32 = 1_000_000;

/// A random PIN from the operating system's secure random source, with leading zeros, e.g. `"042917"`.
///
/// # Errors
///
/// [`Error::Io`] if the operating system has no random source.
pub fn generate_pin() -> Result<String> {
    let random = SystemRandom::new();
    // Taking `value % PIN_RANGE` of any u32 would make low PINs slightly more likely. Drawing again whenever the
    // value falls in the last, incomplete block of a million keeps every PIN equally likely.
    let limit = u32::MAX - u32::MAX % PIN_RANGE;
    loop {
        let mut bytes = [0; 4];
        random.fill(&mut bytes).map_err(|_| Error::Io(io::Error::other("no secure random source")))?;
        let value = u32::from_le_bytes(bytes);
        if value < limit {
            return Ok(format!("{:0width$}", value % PIN_RANGE, width = PIN_LENGTH));
        }
    }
}

/// Removes spaces and dashes the user may have typed, and returns the PIN if what is left is exactly six digits.
#[must_use]
pub fn normalize_pin(typed: &str) -> Option<String> {
    let digits: String = typed.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    (digits.len() == PIN_LENGTH && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_six_digits_and_vary() {
        let pins: Vec<String> = (0..20).map(|_| generate_pin().unwrap()).collect();
        for pin in &pins {
            assert_eq!(normalize_pin(pin).as_deref(), Some(pin.as_str()));
        }
        let mut unique = pins.clone();
        unique.sort();
        unique.dedup();
        assert!(unique.len() > 1);
    }

    #[test]
    fn typed_pins_are_normalized() {
        assert_eq!(normalize_pin(" 123 456 ").as_deref(), Some("123456"));
        assert_eq!(normalize_pin("123-456").as_deref(), Some("123456"));
        assert_eq!(normalize_pin("12345"), None);
        assert_eq!(normalize_pin("12345a"), None);
        assert_eq!(normalize_pin("١٢٣٤٥٦"), None, "only ASCII digits");
    }
}
