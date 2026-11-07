//! What Hangar's app and guest agent say to each other.
//!
//! A shared document is edited through one authority, the Code service in the
//! guest, which orders every change; each client rebases its own unconfirmed
//! changes on the ones it is sent ([`collab`]). Changes are [`ChangeSet`]s,
//! which count positions in UTF-16 code units over text whose lines are
//! joined by `\n` ([`Text`]), exactly as CodeMirror's do, so a browser
//! running `@codemirror/collab` takes part unchanged.

pub mod change;
pub mod collab;
mod error;
pub mod text;

pub use change::{Change, ChangeSet, MapMode};
pub use error::Error;
pub use text::Text;
