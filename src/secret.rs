//! Wrapper that keeps secrets out of logs, error messages and debug output.

use std::fmt;

use zeroize::Zeroize;

/// A secret value (token, password). `Debug` and `Display` never reveal it; the memory is
/// zeroized on drop. Reading it requires an explicit [`Secret::expose`] call.
pub struct Secret<T: Zeroize>(T);

impl<T: Zeroize> Secret<T> {
    /// Wraps a value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Gives access to the wrapped value. Never log the result.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> Drop for Secret<T> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T: Zeroize> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUE: &str = "hunter2-token";

    fn secret() -> Secret<String> {
        Secret::new(VALUE.to_owned())
    }

    #[test]
    fn debug_is_redacted() {
        let out = format!("{:?}", secret());
        assert_eq!(out, "[redacted]");
        assert!(!out.contains("hunter"));
    }

    #[test]
    fn display_is_redacted() {
        let out = format!("{}", secret());
        assert_eq!(out, "[redacted]");
        assert!(!out.contains("hunter"));
    }

    #[test]
    fn expose_returns_value() {
        assert_eq!(secret().expose(), VALUE);
    }
}
