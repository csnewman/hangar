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
