//! A connection's liveness: pinged when quiet, given up when the ping goes
//! unanswered, on the wall clock, so a sleep long enough checks at once.

use std::time::{Duration, SystemTime};

use hangar_app::ws::{ANSWER_WITHIN, Liveness, PING_AFTER};

#[test]
fn a_quiet_connection_is_pinged_then_given_up() {
    let mut l = Liveness::default();
    let t0 = SystemTime::now();
    let at = |d: Duration| t0 + d;
    assert_eq!(l.due(at(Duration::from_secs(1))), Ok(false));
    assert_eq!(l.due(at(PING_AFTER)), Ok(true), "pinged once quiet");
    assert_eq!(l.due(at(PING_AFTER + Duration::from_secs(1))), Ok(false));
    assert!(
        l.due(at(PING_AFTER + ANSWER_WITHIN)).is_err(),
        "given up unanswered"
    );
}

#[test]
fn an_answer_keeps_it() {
    let mut l = Liveness::default();
    let t0 = SystemTime::now();
    assert_eq!(l.due(t0 + PING_AFTER), Ok(true));
    l.heard();
    assert_eq!(l.due(SystemTime::now() + Duration::from_secs(1)), Ok(false));
}

/// Waking from an hour's sleep, the connection is pinged straight away,
/// and given up if that goes unanswered.
#[test]
fn a_sleep_is_noticed_on_waking() {
    let mut l = Liveness::default();
    let woke = SystemTime::now() + Duration::from_secs(3600);
    assert_eq!(l.due(woke), Ok(true));
    assert!(l.due(woke + ANSWER_WITHIN).is_err());
}

/// A server restarting or an environment not yet running is tried again; a
/// refusal of the token or of what was asked for is not.
#[test]
fn what_is_tried_again() {
    use hangar_app::ws::ConnectError::{Refused, Unreachable};
    assert!(Unreachable("connection refused".into()).passes());
    for status in [409, 429, 500, 502, 503, 504] {
        assert!(Refused(status, String::new()).passes(), "{status}");
    }
    for status in [0, 400, 401, 403, 404] {
        assert!(!Refused(status, String::new()).passes(), "{status}");
    }
}
