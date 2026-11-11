//! Parameter Info: when typing asks a language server for a call's
//! signatures, and how its answer is shown, the parameter the caret is at
//! found by offsets or by its text.

use hangar_app::sighelp::{Ask, Line, about, asks, lines};
use lsp_types::{
    Documentation, ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
};
use serde_json::json;

fn strings(all: &[&str]) -> Vec<String> {
    all.iter().map(|s| s.to_string()).collect()
}

#[test]
fn typing_asks_at_the_servers_characters_and_again_while_shown() {
    let (t, r) = (strings(&["(", ","]), strings(&[")"]));
    assert_eq!(asks("record(", &t, &r, false), Some(Ask::Typed("(".into())));
    assert_eq!(
        asks("a", &t, &r, false),
        None,
        "nothing shown, nothing asked"
    );
    assert_eq!(asks(")", &t, &r, false), None);
    assert_eq!(asks("1", &t, &r, true), Some(Ask::Again));
    assert_eq!(asks(")", &t, &r, true), Some(Ask::Typed(")".into())));
    assert_eq!(asks(",", &t, &r, true), Some(Ask::Typed(",".into())));

    let shown = SignatureHelp {
        signatures: vec![],
        active_signature: None,
        active_parameter: None,
    };
    assert_eq!(
        Ask::Typed(",".into()).context(Some(&shown)),
        json!({
            "triggerKind": 2, "triggerCharacter": ",", "isRetrigger": true,
            "activeSignatureHelp": { "signatures": [] },
        })
    );
    assert_eq!(
        Ask::Invoked.context(None),
        json!({ "triggerKind": 1, "isRetrigger": false })
    );
}

fn sig(label: &str, params: Vec<ParameterLabel>) -> SignatureInformation {
    SignatureInformation {
        label: label.into(),
        documentation: None,
        parameters: Some(
            params
                .into_iter()
                .map(|label| ParameterInformation {
                    label,
                    documentation: None,
                })
                .collect(),
        ),
        active_parameter: None,
    }
}

#[test]
fn the_parameter_at_the_caret_is_found_in_each_signature() {
    let help = SignatureHelp {
        signatures: vec![
            // Offsets count UTF-16: é is one, 😀 two.
            sig(
                "fn é(a: i64, 😀: &str)",
                vec![
                    ParameterLabel::LabelOffsets([5, 11]),
                    ParameterLabel::LabelOffsets([13, 21]),
                ],
            ),
            sig(
                "log(a, a2)",
                vec![
                    ParameterLabel::Simple("a".into()),
                    ParameterLabel::Simple("a2".into()),
                ],
            ),
        ],
        active_signature: Some(0),
        active_parameter: Some(1),
    };
    let shown = lines(&help);
    let first = &shown[0];
    assert!(first.active);
    assert_eq!(&first.label[first.param.clone().unwrap()], "😀: &str");
    assert_eq!(
        shown[1],
        Line {
            label: "log(a, a2)".into(),
            param: Some(7..9),
            active: false,
        },
        "a parameter's text is found after the parenthesis"
    );
}

#[test]
fn what_is_said_of_the_parameter_comes_first() {
    let mut s = sig("f(x)", vec![ParameterLabel::Simple("x".into())]);
    s.documentation = Some(Documentation::String("Makes an f.\n\nMore.".into()));
    let mut help = SignatureHelp {
        signatures: vec![s],
        active_signature: None,
        active_parameter: Some(0),
    };
    assert_eq!(about(&help).as_deref(), Some("Makes an f."));
    help.signatures[0].parameters.as_mut().unwrap()[0].documentation =
        Some(Documentation::String("How far.".into()));
    assert_eq!(about(&help).as_deref(), Some("How far."));
}
