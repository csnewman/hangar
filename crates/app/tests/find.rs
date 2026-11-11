use hangar_app::find::{
    Match, Row, byte_ranges, dir_of, file_of, fold_key, next_match, rows, rows_by_dir, visible,
};

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
            _ => None,
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

#[test]
fn folded_files_hide_their_lines_and_occurrences_step_through_every_match() {
    let r = rows(&[(
        "/w".to_string(),
        vec![m("a.rs", 1), m("a.rs", 5), m("b.rs", 2)],
    )]);
    // a.rs, its two lines, b.rs, its line.
    assert_eq!(r.len(), 5);
    let mut folded = std::collections::HashSet::new();
    assert_eq!(visible(&r, &folded), [0, 1, 2, 3, 4]);
    folded.insert(("/w".to_string(), "a.rs".to_string()));
    assert_eq!(visible(&r, &folded), [0, 3, 4]);

    // From nothing, the first match; past the last, none; back from none,
    // the last.
    assert_eq!(next_match(&r, None, true), Some(1));
    assert_eq!(next_match(&r, Some(2), true), Some(4));
    assert_eq!(next_match(&r, Some(4), true), None);
    assert_eq!(next_match(&r, None, false), Some(4));
    assert_eq!(next_match(&r, Some(4), false), Some(2));
    assert_eq!(next_match(&r, Some(1), false), None);

    assert_eq!(file_of(&r, 2), Some(0));
    assert_eq!(file_of(&r, 3), Some(3));
    assert_eq!(file_of(&r, 4), Some(3));
}

#[test]
fn grouped_by_directory_each_directory_heads_its_files_and_folds_them() {
    let projects = vec![(
        "/w".to_string(),
        vec![
            m("src/b.rs", 1),
            m("README.md", 3),
            m("src/a.rs", 2),
            m("src/a.rs", 7),
        ],
    )];
    let r = rows_by_dir(&projects);
    let shape: Vec<String> = r
        .iter()
        .map(|row| match row {
            Row::Dir { dir, count, .. } => format!("dir {dir:?} {count}"),
            Row::File { path, count, .. } => format!("file {path} {count}"),
            Row::Line { m, .. } => format!("line {}", m.line),
        })
        .collect();
    // The project's root first, then by name; files as found within each.
    assert_eq!(
        shape,
        [
            "dir \"\" 1",
            "file README.md 1",
            "line 3",
            "dir \"src\" 3",
            "file src/b.rs 1",
            "line 1",
            "file src/a.rs 2",
            "line 2",
            "line 7",
        ]
    );

    // A folded directory hides its files and their lines; a folded file
    // under it, only its lines.
    let mut folded = std::collections::HashSet::new();
    folded.insert(fold_key(&r[3]).unwrap());
    assert_eq!(visible(&r, &folded), [0, 1, 2, 3]);
    folded.clear();
    folded.insert(fold_key(&r[6]).unwrap());
    assert_eq!(visible(&r, &folded), [0, 1, 2, 3, 4, 5, 6]);
    assert!(fold_key(&r[2]).is_none());

    assert_eq!(dir_of(&r, 8), Some(3));
    assert_eq!(dir_of(&r, 2), Some(0));
    assert_eq!(file_of(&r, 8), Some(6));
    assert_eq!(file_of(&r, 3), None);
    assert_eq!(next_match(&r, Some(2), true), Some(5));
}
