//! Parameter Info, as IntelliJ shows it (⌘P, and as a call is typed): the
//! signatures of the call the caret is in, from the file's language server
//! (textDocument/signatureHelp), the parameter the caret is at in bold.

use std::ops::Range;

use lsp_types::{Documentation, ParameterLabel, SignatureHelp};
use serde_json::{Value, json};

/// Why the server is asked, as the protocol's SignatureHelpTriggerKind has
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// ⌘P.
    Invoked,
    /// A character the server starts or moves on a call's help at.
    Typed(String),
    /// What is shown, asked again as the text or the caret moves.
    Again,
}

/// Whether text just typed asks for help: a character the server names, at
/// its end; or, while help is shown, any change, which may move it on or
/// end it.
pub fn asks(typed: &str, triggers: &[String], retriggers: &[String], shown: bool) -> Option<Ask> {
    let ends = |all: &[String]| {
        all.iter()
            .find(|t| !t.is_empty() && typed.ends_with(t.as_str()))
            .cloned()
    };
    if let Some(t) = ends(triggers) {
        return Some(Ask::Typed(t));
    }
    if !shown {
        return None;
    }
    Some(ends(retriggers).map_or(Ask::Again, Ask::Typed))
}

impl Ask {
    /// The request's context: why it is asked, and what is shown, which
    /// the server may carry on from.
    pub fn context(&self, shown: Option<&SignatureHelp>) -> Value {
        let mut c = json!({
            "triggerKind": match self {
                Ask::Invoked => 1,
                Ask::Typed(_) => 2,
                Ask::Again => 3,
            },
            "isRetrigger": shown.is_some(),
        });
        if let Ask::Typed(t) = self {
            c["triggerCharacter"] = json!(t);
        }
        if let Some(s) = shown {
            c["activeSignatureHelp"] = json!(s);
        }
        c
    }
}

/// One signature as shown: its label, the place in it of the parameter
/// the caret is at, and whether it is the one the server takes the call
/// for.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub label: String,
    pub param: Option<Range<usize>>,
    pub active: bool,
}

/// The signatures shown, in the server's order.
pub fn lines(help: &SignatureHelp) -> Vec<Line> {
    let chosen = help.active_signature.unwrap_or(0) as usize;
    help.signatures
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let at = s.active_parameter.or(help.active_parameter);
            let param = at
                .and_then(|a| s.parameters.as_ref()?.get(a as usize))
                .and_then(|p| place(&s.label, &p.label));
            Line {
                label: s.label.clone(),
                param,
                active: i == chosen.min(help.signatures.len().saturating_sub(1)),
            }
        })
        .collect()
}

/// Where a parameter is in a signature's label, in bytes: from its offsets,
/// which the server counts in UTF-16, or its text, found after the opening
/// parenthesis.
fn place(label: &str, p: &ParameterLabel) -> Option<Range<usize>> {
    match p {
        ParameterLabel::LabelOffsets([a, b]) => Some(byte_at(label, *a)?..byte_at(label, *b)?),
        ParameterLabel::Simple(text) if !text.is_empty() => {
            let from = label.find('(').map_or(0, |i| i + 1);
            let i = label[from..].find(text.as_str())? + from;
            Some(i..i + text.len())
        }
        ParameterLabel::Simple(_) => None,
    }
}

/// The byte a UTF-16 offset falls at, if it falls between characters.
fn byte_at(s: &str, utf16: u32) -> Option<usize> {
    let mut n = 0u32;
    for (i, c) in s.char_indices() {
        if n == utf16 {
            return Some(i);
        }
        if n > utf16 {
            return None;
        }
        n += c.len_utf16() as u32;
    }
    (n == utf16).then_some(s.len())
}

/// What the server says of the parameter the caret is at, else of the
/// signature: its first paragraph.
pub fn about(help: &SignatureHelp) -> Option<String> {
    let chosen = help.active_signature.unwrap_or(0) as usize;
    let s = help.signatures.get(chosen).or(help.signatures.first())?;
    let at = s.active_parameter.or(help.active_parameter);
    let doc = at
        .and_then(|a| {
            s.parameters
                .as_ref()?
                .get(a as usize)?
                .documentation
                .as_ref()
        })
        .or(s.documentation.as_ref())?;
    let text = match doc {
        Documentation::String(t) => t.as_str(),
        Documentation::MarkupContent(m) => m.value.as_str(),
    };
    let first = text.trim().split("\n\n").next()?.trim();
    (!first.is_empty()).then(|| first.replace('\n', " "))
}
