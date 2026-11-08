use hangar_app::find::{Match, Row, byte_ranges, rows};

fn m(path: &str, line: u32) -> Match {
    Match {
        path: path.into(),
        line,
        text: "post(entry)".into(),
        ranges: vec![[0, 4]],
    }
}

#[test]
fn matches_are_grouped_by_file_and_a_nested_projects_found_once() {
    let projects = vec![
        (
            "/home/dev/ledger".to_string(),
            vec![
                m("src/post.rs", 2),
                m("src/post.rs", 9),
                m("src/main.rs", 1),
            ],
        ),
        (
            "/home/dev".to_string(),
            vec![m("ledger/src/post.rs", 2), m("notes.md", 4)],
        ),
    ];
    let r = rows(&projects);
    let files: Vec<(&str, &str, usize)> = r
        .iter()
        .filter_map(|r| match r {
            Row::File { root, path, count } => Some((root.as_str(), path.as_str(), *count)),
            Row::Line { .. } => None,
        })
        .collect();
    assert_eq!(
        files,
        [
            ("/home/dev/ledger", "src/post.rs", 2),
            ("/home/dev/ledger", "src/main.rs", 1),
            ("/home/dev", "notes.md", 1),
        ]
    );
    assert_eq!(r.len(), 3 + 4);
}

#[test]
fn utf16_ranges_become_byte_ranges() {
    // "é" is one UTF-16 unit and two bytes; "😀" two units and four bytes.
    let text = "é😀 post";
    assert_eq!(byte_ranges(text, &[[3, 4]]), vec![6..7]);
    assert_eq!(byte_ranges(text, &[[1, 3]]), vec![2..6]);
    assert_eq!(byte_ranges("abc", &[[0, 3], [5, 5]]), vec![0..3]);
}

/// A search covers every project, one, or a folder in one.
#[test]
fn a_scope_picks_the_projects_searched() {
    use hangar_app::find::Scope;
    let roots = ["/home/dev", "/home/dev/shop", "/srv/api"];
    assert_eq!(Scope::Projects.targets(&roots).len(), 3);
    assert_eq!(
        Scope::Project("/srv/api".into()).targets(&roots),
        vec![("/srv/api".to_string(), None)]
    );
    assert_eq!(
        Scope::Folder {
            root: "/home/dev/shop".into(),
            dir: "src".into()
        }
        .targets(&roots),
        vec![("/home/dev/shop".to_string(), Some("src".to_string()))]
    );
}

/// A folder typed from / is found in the innermost project holding it; one
/// typed from a project is in the first project.
#[test]
fn a_folder_typed_is_read_within_its_project() {
    use hangar_app::find::Scope;
    let roots = ["/home/dev", "/home/dev/shop"];
    assert_eq!(
        Scope::folder("/home/dev/shop/src/", &roots, None),
        Ok(Scope::Folder {
            root: "/home/dev/shop".into(),
            dir: "src".into()
        })
    );
    assert_eq!(
        Scope::folder("/home/dev/shop", &roots, None),
        Ok(Scope::Project("/home/dev/shop".into()))
    );
    assert_eq!(
        Scope::folder("./notes", &roots, None),
        Ok(Scope::Folder {
            root: "/home/dev".into(),
            dir: "notes".into()
        })
    );
    assert!(Scope::folder("/etc", &roots, None).is_err());
    assert!(Scope::folder(" ", &roots, None).is_err());
}
