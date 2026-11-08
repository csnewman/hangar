//! Find Usages' and Rename's answers: the word asked about, a file's edits
//! as one change, and a workspace edit's edits by file.

use gpui_component::input::Rope;
use hangar_app::lspops::{by_file, changeset, word_at};
use hangar_proto::Text;
use lsp_types::{Position, Range, TextEdit, Uri, WorkspaceEdit};

#[test]
fn the_word_at_the_caret() {
    let t = "let ttl_secs = self.ttl;";
    assert_eq!(word_at(t, 4), "ttl_secs", "at its start");
    assert_eq!(word_at(t, 8), "ttl_secs", "within it");
    assert_eq!(word_at(t, 12), "ttl_secs", "just after it");
    assert_eq!(word_at(t, 20), "ttl");
    assert_eq!(word_at(t, 13), "", "on a space");
    assert_eq!(word_at("größe = 1", 3), "größe", "beyond ASCII");
}

fn edit(l0: u32, c0: u32, l1: u32, c1: u32, text: &str) -> TextEdit {
    TextEdit {
        range: Range {
            start: Position {
                line: l0,
                character: c0,
            },
            end: Position {
                line: l1,
                character: c1,
            },
        },
        new_text: text.into(),
    }
}

#[test]
fn a_files_edits_as_one_change() {
    let text = "fn refresh() {}\nrefresh();\n";
    let rope = Rope::from(text);
    // Given in any order, as servers do.
    let edits = [edit(1, 0, 1, 7, "renew"), edit(0, 3, 0, 10, "renew")];
    let c = changeset(&rope, &edits).unwrap();
    assert_eq!(
        c.apply(&Text::from(text)).unwrap().to_string(),
        "fn renew() {}\nrenew();\n"
    );
    assert!(changeset(&rope, &[]).is_none());
}

#[test]
// lsp_types keys its edits by Uri, which clippy takes for mutable.
#[allow(clippy::mutable_key_type)]
fn a_workspace_edit_by_file() {
    let uri = |p: &str| p.parse::<Uri>().unwrap();
    let mut changes = std::collections::HashMap::new();
    changes.insert(uri("file:///home/dev/a.rs"), vec![edit(0, 0, 0, 1, "x")]);
    changes.insert(
        uri("file:///home/dev/b%20c.rs"),
        vec![edit(0, 0, 0, 1, "y")],
    );
    let files = by_file(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    });
    let names: Vec<&str> = files.keys().map(String::as_str).collect();
    assert_eq!(names, ["/home/dev/a.rs", "/home/dev/b c.rs"]);
}
