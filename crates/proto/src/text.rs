use std::fmt;

/// A document's text as the protocol counts it: UTF-16 code units, with
/// every line break `\n`.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Text(Vec<u16>);

impl Text {
    pub fn new() -> Self {
        Self::default()
    }

    /// Joins `lines` with `\n`. A line holding a line break of its own
    /// keeps it as it is.
    pub fn from_lines<S: AsRef<str>>(lines: &[S]) -> Self {
        let mut units = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                units.push(u16::from(b'\n'));
            }
            units.extend(line.as_ref().encode_utf16());
        }
        Text(units)
    }

    /// `s` as it is, line breaks included.
    pub fn exact(s: &str) -> Self {
        Text(s.encode_utf16().collect())
    }

    pub fn from_units(units: Vec<u16>) -> Self {
        Text(units)
    }

    /// The text split at each `\n`. A line cut through a surrogate pair
    /// has U+FFFD in its place.
    pub fn lines(&self) -> Vec<String> {
        self.0
            .split(|&u| u == u16::from(b'\n'))
            .map(String::from_utf16_lossy)
            .collect()
    }

    pub fn units(&self) -> &[u16] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The code units from `from` to `to`, clamped to the text.
    pub fn slice(&self, from: usize, to: usize) -> Text {
        let to = to.min(self.0.len());
        let from = from.min(to);
        Text(self.0[from..to].to_vec())
    }

    pub fn append(&mut self, other: &Text) {
        self.0.extend_from_slice(&other.0);
    }

    pub(crate) fn push_units(&mut self, units: &[u16]) {
        self.0.extend_from_slice(units);
    }

    pub(crate) fn with_capacity(n: usize) -> Self {
        Text(Vec::with_capacity(n))
    }
}

/// Takes any line breaks, `\r\n` and a lone `\r` becoming `\n`, as
/// CodeMirror splits inserted text.
impl From<&str> for Text {
    fn from(s: &str) -> Self {
        let mut units = Vec::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        let mut buf = [0u16; 2];
        while let Some(c) = chars.next() {
            if c == '\r' {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                units.push(u16::from(b'\n'));
            } else {
                units.extend_from_slice(c.encode_utf16(&mut buf));
            }
        }
        Text(units)
    }
}

/// The rope's text unit for unit, line breaks as they are.
impl From<&ropey::Rope> for Text {
    fn from(rope: &ropey::Rope) -> Self {
        let mut units = Vec::with_capacity(rope.len_utf16_cu());
        for chunk in rope.chunks() {
            units.extend(chunk.encode_utf16());
        }
        Text(units)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf16_lossy(&self.0))
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.to_string(), f)
    }
}
