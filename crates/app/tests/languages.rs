use hangar_app::languages::{file_uri, language_id, server_language, uri_path};
use hangar_app::problems::{Row, rows};
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

#[test]
fn files_are_given_their_languages_server() {
    assert_eq!(language_id("/w/src/main.rs"), Some("rust"));
    assert_eq!(language_id("/w/App.TSX"), Some("typescriptreact"));
    assert_eq!(language_id("/w/go.mod"), Some("go.mod"));
    assert_eq!(server_language("go.mod"), "go");
    assert_eq!(language_id("/w/README"), None);
    assert_eq!(language_id("/w/notes.md"), None);
}

#[test]
fn file_uris_are_encoded_as_the_browser_encodes_them_and_read_back() {
    let abs = "/home/dev/my project/naïve#1.rs";
    let uri = file_uri(abs);
    assert_eq!(uri, "file:///home/dev/my%20project/na%C3%AFve%231.rs");
    assert_eq!(uri_path(&uri).as_deref(), Some(abs));
    assert_eq!(uri_path("file://host/a/b").as_deref(), Some("/a/b"));
    assert_eq!(uri_path("https://example.com/a"), None);
}

fn d(line: u32, severity: Option<DiagnosticSeverity>) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(line, 0), Position::new(line, 1)),
        severity,
        message: format!("at {line}"),
        ..Default::default()
    }
}

#[test]
fn problems_are_listed_by_file_errors_first() {
    let files = std::collections::BTreeMap::from([
        (
            "/w/b.rs".to_string(),
            vec![
                d(9, Some(DiagnosticSeverity::WARNING)),
                d(4, None),
                d(1, Some(DiagnosticSeverity::ERROR)),
            ],
        ),
        (
            "/w/a.rs".to_string(),
            vec![d(2, Some(DiagnosticSeverity::HINT))],
        ),
        ("/w/clean.rs".to_string(), vec![]),
    ]);
    let r = rows(&files);
    let summary: Vec<String> = r
        .iter()
        .map(|row| match row {
            Row::File {
                abs,
                errors,
                warnings,
                count,
            } => format!("{abs} {errors}/{warnings}/{count}"),
            Row::Problem { d, .. } => d.message.clone(),
        })
        .collect();
    assert_eq!(
        summary,
        [
            "/w/a.rs 0/0/1",
            "at 2",
            "/w/b.rs 2/1/3",
            "at 1",
            "at 4",
            "at 9"
        ]
    );
}

#[test]
fn a_definition_in_any_form_is_a_list_of_links() {
    use hangar_app::lspedit::links;
    use lsp_types::{GotoDefinitionResponse, Location, LocationLink, Uri};
    let uri: Uri = "file:///w/a.rs".parse().unwrap();
    let at = Range::new(Position::new(3, 4), Position::new(3, 9));
    let one = links(Some(GotoDefinitionResponse::Scalar(Location::new(
        uri.clone(),
        at,
    ))));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].target_uri, uri);
    assert_eq!(one[0].target_selection_range, at);
    let many = links(Some(GotoDefinitionResponse::Array(vec![
        Location::new(uri.clone(), at),
        Location::new(uri.clone(), at),
    ])));
    assert_eq!(many.len(), 2);
    let link = LocationLink {
        origin_selection_range: None,
        target_uri: uri,
        target_range: at,
        target_selection_range: at,
    };
    assert_eq!(
        links(Some(GotoDefinitionResponse::Link(vec![link.clone()]))),
        [link]
    );
    assert!(links(None).is_empty());
}

/// A server's code actions as ⌥↩ lists them: a bare command as an action
/// running it, those it cannot do left out, its preferred fix first, then
/// the other fixes, then the rest.
#[test]
fn code_actions_are_listed_fixes_first() {
    use hangar_app::lspedit::{actions, is_fix};
    use lsp_types::{CodeAction, CodeActionDisabled, CodeActionKind, CodeActionOrCommand, Command};
    let action = |title: &str, kind: Option<CodeActionKind>| CodeAction {
        title: title.into(),
        kind,
        ..Default::default()
    };
    let list = vec![
        CodeActionOrCommand::CodeAction(action("Extract", Some(CodeActionKind::REFACTOR_EXTRACT))),
        CodeActionOrCommand::Command(Command {
            title: "Organise".into(),
            command: "organise".into(),
            arguments: None,
        }),
        CodeActionOrCommand::CodeAction(action("Import it", Some(CodeActionKind::QUICKFIX))),
        CodeActionOrCommand::CodeAction(CodeAction {
            is_preferred: Some(true),
            ..action("Add the semicolon", Some(CodeActionKind::QUICKFIX))
        }),
        CodeActionOrCommand::CodeAction(CodeAction {
            disabled: Some(CodeActionDisabled {
                reason: "no selection".into(),
            }),
            ..action("Inline", Some(CodeActionKind::REFACTOR_INLINE))
        }),
    ];
    let listed = actions(list);
    let titles: Vec<&str> = listed.iter().map(|a| a.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Add the semicolon", "Import it", "Extract", "Organise"]
    );
    assert_eq!(listed[3].command.as_ref().unwrap().command, "organise");
    assert_eq!(listed.iter().filter(|a| is_fix(a)).count(), 2);
}

/// F2 goes to the next problem after the caret, round to the top, and ⇧F2
/// to the one before, round to the bottom; hints are passed over.
#[test]
fn f2_goes_round_the_problems() {
    use hangar_app::problems::next_problem;
    let list = vec![
        d(9, Some(DiagnosticSeverity::WARNING)),
        d(2, Some(DiagnosticSeverity::ERROR)),
        d(5, Some(DiagnosticSeverity::HINT)),
    ];
    let at = |line| Position::new(line, 0);
    let line = |x: Option<&Diagnostic>| x.map(|d| d.range.start.line);
    assert_eq!(line(next_problem(&list, at(0), false)), Some(2));
    assert_eq!(
        line(next_problem(&list, at(2), false)),
        Some(9),
        "past the one at the caret"
    );
    assert_eq!(
        line(next_problem(&list, at(9), false)),
        Some(2),
        "round to the top"
    );
    assert_eq!(line(next_problem(&list, at(9), true)), Some(2));
    assert_eq!(
        line(next_problem(&list, at(1), true)),
        Some(9),
        "round to the bottom"
    );
    assert_eq!(next_problem(&[], at(0), false), None);
}

/// A completion is known to be taken when the text just put in is the one
/// item offered putting it in, and it has edits of its own to make or left
/// for its server to fill in; what was offered is let go once one is.
#[test]
fn the_completion_taken_is_known_by_its_text() {
    use hangar_app::lspedit::{Offered, taken};
    use lsp_types::{CompletionItem, TextEdit};
    let item = |label: &str| CompletionItem {
        label: label.into(),
        ..Default::default()
    };
    let offered = Offered::default();
    let fill = || {
        *offered.lock().unwrap() = vec![
            CompletionItem {
                data: Some(serde_json::json!({ "import": "std::collections::HashMap" })),
                ..item("HashMap")
            },
            item("Hash"),
            CompletionItem {
                additional_text_edits: Some(vec![TextEdit {
                    range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                    new_text: "use a::Vec;\n".into(),
                }]),
                ..item("Vec")
            },
            item("dup"),
            item("dup"),
        ]
    };
    fill();
    assert!(taken(&offered, "H").is_none(), "a key typed");
    assert_eq!(
        taken(&offered, "HashMap").map(|i| i.label),
        Some("HashMap".into())
    );
    assert!(offered.lock().unwrap().is_empty(), "let go once taken");
    fill();
    assert!(taken(&offered, "Hash").is_none(), "nothing more to make");
    fill();
    assert_eq!(taken(&offered, "Vec").map(|i| i.label), Some("Vec".into()));
    fill();
    assert!(taken(&offered, "dup").is_none(), "two put in the same");
}
