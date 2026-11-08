//! What the language servers' answers to Find Usages and Rename become: the
//! word they were asked about, a file's text edits as one change, a
//! workspace edit's changes by file, and the edits to a file no editor has
//! open, made through its shared document.

use std::collections::BTreeMap;

use gpui_component::input::{Rope, RopeExt};
use hangar_proto::{Change, ChangeSet, Text};
use lsp_types::{DocumentChanges, OneOf, TextEdit, WorkspaceEdit};
use serde_json::json;

use crate::code::Code;
use crate::languages::uri_path;

/// The identifier the caret is in or just after, by byte offset.
pub fn word_at(text: &str, at: usize) -> String {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| word(*c))
        .last()
        .map_or(at, |(i, _)| i);
    let end = text[at..]
        .char_indices()
        .find(|(_, c)| !word(*c))
        .map_or(text.len(), |(i, _)| at + i);
    text[start..end].to_string()
}

/// A file's text edits as one change to text, in UTF-16 positions; None
/// when there are none, or one lies outside the text.
pub fn changeset(text: &Rope, edits: &[TextEdit]) -> Option<ChangeSet> {
    let at = |p: &lsp_types::Position| {
        let byte = text.position_to_offset(p);
        text.byte_to_utf16_idx(byte.min(text.len()))
    };
    let mut changes: Vec<Change> = edits
        .iter()
        .map(|e| Change {
            from: at(&e.range.start),
            to: at(&e.range.end),
            insert: Text::from(e.new_text.as_str()),
        })
        .collect();
    if changes.is_empty() {
        return None;
    }
    changes.sort_by_key(|c| c.from);
    ChangeSet::of(changes, text.len_utf16()).ok()
}

/// A workspace edit's text edits, by each file's absolute path.
pub fn by_file(edit: WorkspaceEdit) -> BTreeMap<String, Vec<TextEdit>> {
    let mut out: BTreeMap<String, Vec<TextEdit>> = BTreeMap::new();
    for (uri, edits) in edit.changes.unwrap_or_default() {
        if let Some(abs) = uri_path(uri.as_str()) {
            out.entry(abs).or_default().extend(edits);
        }
    }
    let docs = match edit.document_changes {
        Some(DocumentChanges::Edits(e)) => e,
        Some(DocumentChanges::Operations(ops)) => ops
            .into_iter()
            .filter_map(|o| match o {
                lsp_types::DocumentChangeOperation::Edit(e) => Some(e),
                lsp_types::DocumentChangeOperation::Op(_) => None,
            })
            .collect(),
        None => vec![],
    };
    for d in docs {
        if let Some(abs) = uri_path(d.text_document.uri.as_str()) {
            out.entry(abs)
                .or_default()
                .extend(d.edits.into_iter().map(|e| match e {
                    OneOf::Left(e) => e,
                    OneOf::Right(a) => a.text_edit,
                }));
        }
    }
    out
}

/// Makes a file's edits through its shared document, for a file no
/// editor here has open: opened, changed, and closed, the service saving it
/// and any tab that has it seeing the change.
pub async fn edit_closed(code: Code, abs: String, edits: Vec<TextEdit>) -> Result<(), String> {
    let opened = code.call("doc.open", json!({ "path": abs })).await?;
    let doc = opened["doc"].as_u64().ok_or("the document did not open")?;
    let text = Rope::from(opened["text"].as_str().unwrap_or_default());
    let result = match changeset(&text, &edits) {
        Some(change) => code
            .call(
                "doc.push",
                json!({
                    "doc": doc,
                    "version": opened["version"],
                    "changes": [change.to_json()],
                }),
            )
            .await
            .map(|_| ()),
        None => Ok(()),
    };
    let _ = code.call("doc.close", json!({ "doc": doc })).await;
    result
}
