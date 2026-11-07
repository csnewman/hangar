use std::fmt;

/// Why a change could not be read, combined or applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A change set met a text, or another change set, of a length other
    /// than the one it was made for.
    Length { expected: usize, got: usize },
    /// A position lies outside the text.
    Range { pos: usize, len: usize },
    /// A change set's JSON is not the shape CodeMirror writes.
    Json(String),
    /// Updates arrived that do not follow on from the client's version.
    Gap { have: u64, got: u64 },
    /// A client's own update came after another client's in one batch,
    /// which an authority accepting pushes only at its current version
    /// never sends.
    Order,
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
            Error::Json(msg) => write!(f, "change set JSON: {msg}"),
            Error::Gap { have, got } => {
                write!(f, "updates start at version {got}, after {have}")
            }
            Error::Order => write!(f, "own update after another client's"),
        }
    }
}

impl std::error::Error for Error {}
