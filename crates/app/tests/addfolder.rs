use hangar_app::addfolder::{offered, split, wanted};
use hangar_app::project::branch_label;
use serde_json::json;

#[test]
fn a_path_typed_is_its_folder_and_the_start_of_a_name() {
    assert_eq!(split("/home/dev/pro"), ("/home/dev/".into(), "pro".into()));
    assert_eq!(split("/home/dev/"), ("/home/dev/".into(), "".into()));
    assert_eq!(split("/"), ("/".into(), "".into()));
    assert_eq!(split("dev"), ("/".into(), "dev".into()));
}

#[test]
fn folders_are_offered_by_their_start_hidden_ones_once_a_dot_is_typed() {
    let dirs: Vec<String> = [".cargo", "Hello-World", "hangar", "shop"]
        .map(String::from)
        .to_vec();
    assert_eq!(offered(&dirs, ""), ["Hello-World", "hangar", "shop"]);
    assert_eq!(offered(&dirs, "h"), ["Hello-World", "hangar"]);
    assert_eq!(offered(&dirs, "."), [".cargo"]);
    assert!(offered(&dirs, "x").is_empty());
}

#[test]
fn a_folder_added_is_absolute_and_not_in_the_workspace() {
    let have = vec!["/home/dev".to_string()];
    assert_eq!(
        wanted("/home/dev/shop/", &have),
        Ok("/home/dev/shop".into())
    );
    assert_eq!(wanted(" / ", &have), Ok("/".into()));
    assert!(wanted("shop", &have).is_err());
    assert_eq!(
        wanted("/home/dev/", &have),
        Err("/home/dev is in the workspace already.".into())
    );
}

#[test]
fn a_projects_branch_is_its_folders_repository_with_how_far_it_has_moved() {
    let status = json!({ "repos": [
        { "path": "", "kind": "folder", "branch": "main", "ahead": 2, "behind": 0 },
        { "path": "vendor/x", "kind": "clone", "branch": "dev" },
    ]});
    assert_eq!(branch_label(&status).as_deref(), Some("main ↑2"));
    // No repository of the folder's own: its only one, else none.
    let one =
        json!({ "repos": [{ "path": "shop", "kind": "clone", "branch": "dev", "behind": 3 }] });
    assert_eq!(branch_label(&one).as_deref(), Some("dev ↓3"));
    let two = json!({ "repos": [
        { "path": "a", "kind": "clone", "branch": "x" },
        { "path": "b", "kind": "clone", "branch": "y" },
    ]});
    assert_eq!(branch_label(&two), None);
    assert_eq!(branch_label(&json!({ "repos": [] })), None);
}
