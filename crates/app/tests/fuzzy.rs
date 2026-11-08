use hangar_app::fuzzy::score;

fn positions(q: &str, t: &str) -> Option<Vec<usize>> {
    score(q, t).map(|(_, p)| p)
}

#[test]
fn the_query_is_found_in_order_case_ignored() {
    assert_eq!(positions("rl", "RateLimit"), Some(vec![0, 4]));
    assert_eq!(positions("lr", "RateLimit"), None);
    assert_eq!(positions("", "anything"), Some(vec![]));
}

#[test]
fn a_run_together_beats_scattered_letters() {
    let (run, at) = score("rate", "src/ratelimit.rs").unwrap();
    assert_eq!(at, vec![4, 5, 6, 7]);
    let (scattered, _) = score("rate", "src/read_attempt.rs").unwrap();
    assert!(run > scattered);
}

#[test]
fn word_starts_count() {
    let (starts, _) = score("pb", "post_batch").unwrap();
    let (inside, _) = score("pb", "upbeat").unwrap();
    assert!(starts > inside);
}
