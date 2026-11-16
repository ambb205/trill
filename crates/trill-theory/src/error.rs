//! Errors for notation that cannot be read.

use std::fmt;

/// A piece of notation that could not be read. The message always quotes
/// the offending text and says what was expected in its place, because the
/// reader is usually a model that will fix its input and try again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: String,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Says where the error was found, e.g. `bar 3: ...`.
    pub(crate) fn within(self, place: impl fmt::Display) -> Self {
        Self { message: format!("{place}: {}", self.message) }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Joins names for an "expected one of" list.
pub(crate) fn list<T: AsRef<str>>(items: impl IntoIterator<Item = T>) -> String {
    items.into_iter().map(|item| item.as_ref().to_string()).collect::<Vec<_>>().join(", ")
}
