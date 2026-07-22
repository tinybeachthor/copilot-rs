//! Errors from building a specification.

use std::fmt;

/// Result alias for the builder frontend.
pub type Result<T> = std::result::Result<T, Error>;

/// Something wrong with a specification, reported by
/// [`Builder::finish`](crate::Builder::finish).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// An error in the IR itself.
    Core(copilot_core::Error),

    /// `drop` was applied to an expression reading an external variable.
    ///
    /// `drop n` asks for a value `n` steps in the future. A stream can answer
    /// as far as its buffer reaches, but an external variable's next sample
    /// does not exist yet — the environment has not produced it. Buffer the
    /// extern in a stream first if its history is what you need.
    DropOnExtern(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Core(e) => e.fmt(f),
            Error::DropOnExtern(name) => write!(
                f,
                "cannot look ahead of external variable `{name}`: its future samples do not exist \
                 yet. Buffer it in a stream if you need its history."
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Core(e) => Some(e),
            Error::DropOnExtern(_) => None,
        }
    }
}

impl From<copilot_core::Error> for Error {
    fn from(e: copilot_core::Error) -> Self {
        Error::Core(e)
    }
}
