//! What programs say they are doing (OSC 7501), kept as the protocol asks.

use hangar_app::terminal::{ProgramStatus, StatusKind, StatusState};
use hangar_app::termstatus::Records;

fn report(id: &str, state: StatusState, app: &str) -> ProgramStatus {
    ProgramStatus {
        state,
        id: id.into(),
        app: app.into(),
        ..ProgramStatus::default()
    }
}

#[test]
fn what_most_needs_the_user_is_shown() {
    let mut r = Records::default();
    assert_eq!(r.shown(), None);
    r.report(report("", StatusState::Working, "cargo"));
    assert_eq!(r.shown().unwrap().state, StatusState::Working);
    r.report(ProgramStatus {
        kind: Some(StatusKind::Question),
        ..report("ask", StatusState::Blocked, "")
    });
    let s = r.shown().unwrap();
    assert_eq!(s.state, StatusState::Blocked);
    assert_eq!(s.kind, Some(StatusKind::Question));
    assert_eq!(s.app, "cargo", "its parent's app");
    // A report replaces its record whole.
    r.report(report("ask", StatusState::Done, ""));
    assert_eq!(r.shown().unwrap().state, StatusState::Working);
}

#[test]
fn a_clear_takes_the_records_beneath() {
    let mut r = Records::default();
    r.report(report("build", StatusState::Working, "make"));
    r.report(report("build/test", StatusState::Blocked, ""));
    r.report(report("deploy", StatusState::Working, "make"));
    assert!(r.report(report("build", StatusState::Clear, "")));
    let s = r.shown().unwrap();
    assert_eq!(s.state, StatusState::Working, "deploy is left");
    assert!(r.report(report("", StatusState::Clear, "")));
    assert_eq!(r.shown(), None, "an empty id clears every record");
}

#[test]
fn a_prompt_ends_the_work_and_seeing_ends_the_rest() {
    let mut r = Records::default();
    r.report(report("a", StatusState::Working, "x"));
    r.report(report("b", StatusState::Blocked, "x"));
    r.report(report("c", StatusState::Done, "x"));
    assert!(r.ended());
    assert_eq!(r.shown().unwrap().state, StatusState::Done);
    assert!(r.seen());
    assert_eq!(r.shown(), None);
    assert!(!r.seen(), "nothing changed");
}

#[test]
fn the_oldest_goes_past_the_limit() {
    let mut r = Records::default();
    r.report(report("first", StatusState::Blocked, "x"));
    for i in 0..256 {
        r.report(report(&format!("n{i}"), StatusState::Idle, "x"));
    }
    assert_ne!(
        r.shown().map(|s| s.state),
        Some(StatusState::Blocked),
        "the first, updated longest ago, is gone"
    );
}
