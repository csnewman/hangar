//! EditorConfig: its files read, its globs matched as the specification
//! has them, and the indentation they say for a file, nearer files and
//! later sections winning, up to one that says it is the root.

use hangar_app::editorconfig::{folders, indent, matches, parse};
use hangar_app::lineops::Indent;

#[test]
fn globs_match_as_the_specification_says() {
    assert!(matches("*", "a.rs"));
    assert!(matches("*.rs", "src/deep/a.rs"), "no slash: any folder");
    assert!(!matches("/*.rs", "src/a.rs"), "a slash: from the folder");
    assert!(matches("src/**/*.rs", "src/a.rs"), "**/ is no folders too");
    assert!(matches("src/**/*.rs", "src/x/y/a.rs"));
    assert!(matches("*.{js,ts}", "a.ts"));
    assert!(!matches("*.{js,ts}", "a.rs"));
    assert!(matches("file{1..3}.txt", "file2.txt"));
    assert!(!matches("file{1..3}.txt", "file4.txt"));
    assert!(matches("[Mm]akefile", "makefile"));
    assert!(matches("*.[!o]", "a.c"));
    assert!(!matches("*.[!o]", "a.o"));
    assert!(matches("?.md", "a.md"));
}

#[test]
fn the_nearest_says_last_up_to_the_root() {
    let top = parse(
        "root = true\n[*]\nindent_style = space\nindent_size = 4\n[*.go]\nindent_style = tab\n",
    );
    let near = parse("[*.yaml]\nindent_size = 2\n");
    let files = |near: Option<_>| {
        vec![
            ("/w/app".to_string(), near),
            ("/w".to_string(), Some(top.clone())),
            ("/".to_string(), Some(parse("[*]\nindent_size = 8\n"))),
        ]
    };
    assert!(top.root);
    assert_eq!(indent("/w/app/a.rs", &files(None)), Some(Indent::Spaces(4)));
    assert_eq!(indent("/w/app/a.go", &files(None)), Some(Indent::Tab));
    assert_eq!(
        indent("/w/app/c.yaml", &files(Some(near))),
        Some(Indent::Spaces(2))
    );
    assert_eq!(indent("/x.rs", &[("/".into(), None)]), None);
    assert_eq!(folders("/w/app/a.rs"), ["/w/app", "/w", "/"]);
}
