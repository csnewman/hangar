use std::collections::HashSet;

use hangar_app::spell::{Dictionary, misspelt, prose, stretches, words};

fn shown(text: &str, rs: &[std::ops::Range<usize>]) -> Vec<String> {
    rs.iter().map(|r| text[r.clone()].to_string()).collect()
}

#[test]
fn english_and_softwares_words_are_known_in_every_form() {
    let d = Dictionary::get();
    for w in [
        "record",
        "records",
        "recorded",
        "recording",
        "unrecorded",
        "ledger's",
        "Monday",
    ] {
        assert!(d.known(w), "{w}");
    }
    for w in ["kubectl", "rustup", "async", "postgres"] {
        assert!(d.known(w), "{w}");
    }
    assert!(!d.known("recieve"));
    assert!(d.suggest("recieve").contains(&"receive".to_string()));
    assert_eq!(d.suggest("Teh").first().map(String::as_str), Some("The"));
}

#[test]
fn affixes_expand_as_their_conditions_say() {
    let aff =
        "SET UTF-8\nSFX S Y 2\nSFX S y ies [^aeiou]y\nSFX S 0 s [^y]\nPFX U Y 1\nPFX U 0 un .\n";
    let dic = "2\ncity/S\ndo/US\n";
    let d = Dictionary::new(aff, dic, "kubectl\n");
    for w in ["city", "cities", "do", "dos", "undo", "undos", "kubectl"] {
        assert!(d.known(w), "{w}");
    }
    assert!(!d.known("citys"));
    assert!(!d.known("uncity"));
}

#[test]
fn prose_is_told_from_code() {
    // An identifier's parts are words, as IntelliJ checks them; its
    // acronyms and the part touching a digit are not.
    let text = "Parse the `config.toml` at ~/x/y.rs: see https://example.com or parseHTTPRequest_v2 and getUserName, it's fine.";
    let w = shown(text, &words(text, 0..text.len()));
    assert_eq!(
        w,
        [
            "Parse", "the", "see", "parse", "Request", "and", "get", "User", "Name", "it's", "fine"
        ]
    );
}

#[test]
fn markdown_leaves_out_its_code_and_link_targets() {
    let md = "# Titel\n\nSee [the docs](https://x.example/dokz) here.\n\n```\nlet teh = 1;\n```\n    indentd code\nlast\n";
    let r = prose(md);
    let found = misspelt(md, &r, &HashSet::new());
    assert_eq!(shown(md, &found), ["Titel"]);
    let own: HashSet<String> = ["titel".to_string()].into();
    assert!(misspelt(md, &r, &own).is_empty());
}

#[test]
fn a_programs_comments_and_prose_strings_are_checked() {
    let rs = "// Recieve the ledgr\nfn main() {\n    let speling = \"one wrod here\";\n    let k = \"keyy\";\n}\n";
    let found = misspelt(rs, &stretches(rs, "rust"), &HashSet::new());
    assert_eq!(shown(rs, &found), ["Recieve", "ledgr", "wrod"]);
}
