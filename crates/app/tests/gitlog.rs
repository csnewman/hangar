use chrono::{Local, TimeZone};
use hangar_app::gitlog::{Commit, Edge, EdgeKind, layout, when};

fn c(hash: &str, parents: &[&str]) -> Commit {
    Commit {
        hash: hash.into(),
        parents: parents.iter().map(|p| p.to_string()).collect(),
        author: "a".into(),
        email: String::new(),
        time: 0.,
        subject: hash.into(),
        refs: vec![],
    }
}

fn kinds(edges: &[Edge]) -> Vec<(EdgeKind, usize, usize)> {
    edges.iter().map(|e| (e.kind, e.from, e.to)).collect()
}

#[test]
fn a_straight_history_is_one_lane() {
    let (rows, lanes) = layout(&[c("c", &["b"]), c("b", &["a"]), c("a", &[])]);
    assert_eq!(lanes, 1);
    assert!(rows.iter().all(|r| r.col == 0 && r.color == 0));
    assert_eq!(kinds(&rows[0].edges), [(EdgeKind::Out, 0, 0)]);
    assert_eq!(
        kinds(&rows[1].edges),
        [(EdgeKind::In, 0, 0), (EdgeKind::Out, 0, 0)]
    );
    assert_eq!(kinds(&rows[2].edges), [(EdgeKind::In, 0, 0)]);
}

#[test]
fn a_merged_branch_takes_a_lane_of_its_own_until_it_forks() {
    // m merges f into main; f and b both come from a.
    let (rows, lanes) = layout(&[
        c("m", &["b", "f"]),
        c("f", &["a"]),
        c("b", &["a"]),
        c("a", &[]),
    ]);
    assert_eq!(lanes, 2);
    assert_eq!(
        kinds(&rows[0].edges),
        [(EdgeKind::Out, 0, 0), (EdgeKind::Out, 0, 1)]
    );
    // f in the second lane, its own colour, its first parent carrying it on.
    assert_eq!((rows[1].col, rows[1].color), (1, 1));
    assert_eq!(
        kinds(&rows[1].edges),
        [
            (EdgeKind::Through, 0, 0),
            (EdgeKind::In, 1, 1),
            (EdgeKind::Out, 1, 1)
        ]
    );
    // b back in the first lane while f's lane passes, both leading to a.
    assert_eq!((rows[2].col, rows[2].color), (0, 0));
    // a: both lanes come in.
    assert_eq!(rows[3].col, 0);
    assert_eq!(
        kinds(&rows[3].edges),
        [(EdgeKind::In, 0, 0), (EdgeKind::In, 1, 0)]
    );
}

#[test]
fn a_second_tip_starts_a_new_colour_in_a_free_lane() {
    let (rows, _) = layout(&[c("x", &["a"]), c("y", &["a"]), c("a", &[])]);
    assert_eq!((rows[0].col, rows[0].color), (0, 0));
    assert_eq!((rows[1].col, rows[1].color), (1, 1));
}

#[test]
fn times_are_shown_by_how_long_ago() {
    let now = Local.with_ymd_and_hms(2026, 11, 8, 15, 0, 0).unwrap();
    let today = Local.with_ymd_and_hms(2026, 11, 8, 9, 5, 0).unwrap();
    let earlier = Local.with_ymd_and_hms(2026, 3, 2, 9, 5, 0).unwrap();
    let last_year = Local.with_ymd_and_hms(2025, 3, 2, 9, 5, 0).unwrap();
    assert_eq!(when(today.timestamp() as f64, now), "09:05");
    assert_eq!(when(earlier.timestamp() as f64, now), "2 Mar");
    assert_eq!(when(last_year.timestamp() as f64, now), "2 Mar 2025");
}
