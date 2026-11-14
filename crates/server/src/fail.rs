//! Why something asked of the server is refused, as its domains tell it;
//! the HTTP layer answers each with the spec's status.

use crate::store;

#[derive(Debug)]
pub enum Fail {
    /// The request is not one the server takes (400).
    Invalid(String),
    /// It cannot be done as things stand (409).
    Conflict(String),
    /// There is no such thing, or none the asker may see (404).
    NotFound(String),
    /// The store failed (500).
    Store(store::Error),
}

impl From<store::Error> for Fail {
    fn from(e: store::Error) -> Fail {
        Fail::Store(e)
    }
}

pub type Result<T> = std::result::Result<T, Fail>;

pub fn invalid<T>(why: impl Into<String>) -> Result<T> {
    Err(Fail::Invalid(format!("invalid: {}", why.into())))
}

pub fn conflict<T>(why: impl Into<String>) -> Result<T> {
    Err(Fail::Conflict(format!("conflict: {}", why.into())))
}
