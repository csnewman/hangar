use hangar_app::commit::{Repo, Target, amendable, commits, repos, sections, within};
use serde_json::json;

fn status() -> serde_json::Value {
    json!({
        "repo": true,
        "changes": [],
        "repos": [
            {"path": "", "name": "app", "kind": "folder", "branch": "main", "ahead": 1,
             "changes": [
                {"path": "src/main.rs", "index": "M", "worktree": "."},
                {"path": "src/lib.rs", "index": "M", "worktree": "M"},
                {"path": "README.md", "index": ".", "worktree": "M"},
                {"path": "notes.txt", "index": "?", "worktree": "?"},
                {"path": "both.rs", "index": "U", "worktree": "U"},
             ]},
            {"path": "vendor/lib", "name": "lib", "kind": "submodule", "parent": "",
             "changes": [{"path": "vendor/lib/x.c", "index": "A", "worktree": "."}]},
            {"path": "vendor/gone", "name": "gone", "kind": "submodule", "uninitialised": true,
             "changes": []},
        ]
    })
}

#[test]
fn a_repositorys_changes_are_split_as_the_web_ui_splits_them() {
    let rs = repos(&status());
    assert_eq!(rs.len(), 3);
    let (staged, changed, unversioned) = sections(&rs[0]);
    let paths = |cs: Vec<&hangar_app::commit::Change>| {
        cs.into_iter().map(|c| c.path.clone()).collect::<Vec<_>>()
    };
    assert_eq!(paths(staged), ["src/main.rs", "src/lib.rs"]);
    assert_eq!(paths(changed), ["src/lib.rs", "README.md", "both.rs"]);
    assert_eq!(paths(unversioned), ["notes.txt"]);
    assert_eq!(rs[0].branch.as_deref(), Some("main"));
    assert_eq!(rs[0].ahead, Some(1));
    assert!(rs[2].uninitialised);
}

#[test]
fn a_submodules_paths_are_shown_from_its_own_folder() {
    let rs = repos(&status());
    assert_eq!(within(&rs[1], "vendor/lib/x.c"), "x.c");
    assert_eq!(within(&rs[0], "src/main.rs"), "src/main.rs");
}

#[test]
fn a_submodule_is_committed_before_the_repository_naming_it() {
    let projects = vec![("/w/app".to_string(), repos(&status()))];
    let order: Vec<String> = commits(&projects, false)
        .unwrap()
        .into_iter()
        .map(|t| t.repo)
        .collect();
    assert_eq!(order, ["vendor/lib", ""]);
}

#[test]
fn commits_span_every_project_with_something_staged() {
    let staged = |name: &str| Repo {
        path: String::new(),
        name: name.into(),
        changes: vec![hangar_app::commit::Change {
            path: "f".into(),
            index: "M".into(),
            worktree: ".".into(),
            from: None,
            submodule: false,
        }],
        ..Default::default()
    };
    let clean = Repo {
        name: "clean".into(),
        ..Default::default()
    };
    let projects = vec![
        ("/w/a".to_string(), vec![staged("a")]),
        ("/w/b".to_string(), vec![clean.clone()]),
        ("/w/c".to_string(), vec![staged("c")]),
    ];
    let roots: Vec<String> = commits(&projects, false)
        .unwrap()
        .into_iter()
        .map(|t| t.root)
        .collect();
    assert_eq!(roots, ["/w/a", "/w/c"]);

    // Two repositories staged: which to amend is not clear.
    assert_eq!(amendable(&projects), None);
    assert!(commits(&projects, true).is_err());

    // One staged: that one.
    let one = vec![
        ("/w/a".to_string(), vec![staged("a")]),
        ("/w/b".to_string(), vec![clean.clone()]),
    ];
    assert_eq!(
        amendable(&one),
        Some(Target {
            root: "/w/a".into(),
            repo: String::new(),
            name: "a".into()
        })
    );

    // Nothing staged, and only one repository: that one.
    let alone = vec![("/w/b".to_string(), vec![clean])];
    assert_eq!(amendable(&alone).map(|t| t.name), Some("clean".into()));
}

#[test]
fn a_repository_two_projects_hold_is_committed_once() {
    let ledger = repos(&json!({"repos": [{"path": "", "name": "ledger",
        "changes": [{"path": "a", "index": "M", "worktree": "."}]}]}));
    let home = repos(
        &json!({"repos": [{"path": "ledger", "name": "ledger", "kind": "clone",
        "changes": [{"path": "ledger/a", "index": "M", "worktree": "."}]}]}),
    );
    let projects = vec![
        ("/home/dev/ledger".to_string(), ledger),
        ("/home/dev".to_string(), home),
    ];
    let targets = commits(&projects, false).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].root, "/home/dev/ledger");
    assert_eq!(
        amendable(&projects).map(|t| t.root),
        Some("/home/dev/ledger".into())
    );
}
