use std::fmt;

/// Why a change could not be read, combined or applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A change set met a text, or another change set, of a length other
    /// than the one it was made for.
    Length { expected: usize, got: usize },
    /// A position lies outside the text.
    Range { pos: usize, len: usize },
    /// A change would cut a character outside the BMP in two, or insert half
    /// of one.
    Surrogate { pos: usize },
    /// A change set's JSON is not the shape CodeMirror writes.
    Json(String),
    /// Updates, or a push, from a version after the one they should follow.
    Gap { have: u64, got: u64 },
    /// The authority no longer keeps the updates since `version`: the client
    /// opens the document again.
    Forgotten { version: u64, first: u64 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Length { expected, got } => {
                write!(f, "change set is for length {expected}, not {got}")
            }
            Error::Range { pos, len } => {
                write!(f, "position {pos} is outside text of length {len}")
            }
            Error::Surrogate { pos } => {
                write!(f, "change at {pos} splits a surrogate pair")
            }
            Error::Json(msg) => write!(f, "change set JSON: {msg}"),
            Error::Gap { have, got } => {
                write!(f, "version {got} is after {have}")
            }
            Error::Forgotten { version, first } => {
                write!(
                    f,
                    "updates since {version} are gone; the first kept is {first}"
                )
            }
        }
    }
}

impl std::error::Error for Error {}
