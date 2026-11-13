//! Logging with fields, through the log crate: what a line is about (an
//! environment, a component) travels with the handle. Each line is
//! `msg=MESSAGE` and then the fields as key=value pairs, as Go's slog text
//! handler writes them, quoted where a value needs it.

use std::fmt::Write;
use std::sync::Arc;

pub use ::log::Level;

/// A logger carrying fields every line it writes has.
#[derive(Clone, Default)]
pub struct Log {
    fields: Arc<String>,
}

impl Log {
    /// A logger with field k=v as well as this one's.
    pub fn with(&self, k: &str, v: impl std::fmt::Display) -> Log {
        let mut f = (*self.fields).clone();
        push(&mut f, k, &v.to_string());
        Log {
            fields: Arc::new(f),
        }
    }

    pub fn log(&self, level: Level, msg: &str, kv: &[(&str, &dyn std::fmt::Display)]) {
        let mut line = String::new();
        push(&mut line, "msg", msg);
        line.remove(0);
        line.push_str(&self.fields);
        for (k, v) in kv {
            push(&mut line, k, &v.to_string());
        }
        ::log::log!(target: "hangar", level, "{line}");
    }

    pub fn debug(&self, msg: &str, kv: &[(&str, &dyn std::fmt::Display)]) {
        self.log(Level::Debug, msg, kv);
    }

    pub fn info(&self, msg: &str, kv: &[(&str, &dyn std::fmt::Display)]) {
        self.log(Level::Info, msg, kv);
    }

    pub fn warn(&self, msg: &str, kv: &[(&str, &dyn std::fmt::Display)]) {
        self.log(Level::Warn, msg, kv);
    }
}

/// Appends k=v, quoting a value with spaces or quotes in it.
fn push(line: &mut String, k: &str, v: &str) {
    if v.is_empty() || v.contains([' ', '"', '=', '\n']) {
        let _ = write!(line, " {k}={v:?}");
    } else {
        let _ = write!(line, " {k}={v}");
    }
}
