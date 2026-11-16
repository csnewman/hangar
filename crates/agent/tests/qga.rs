//! The guest agent protocol as Proxmox speaks it: requests one after
//! another, split anywhere, a 0xFF dropping what came before it, and
//! guest-sync-delimited's answer marked with one.

use hangar_agent::qga::Session;
use serde_json::{Value, json};

fn answers(s: &mut Session, input: &[u8]) -> Vec<Value> {
    let mut buf = input.to_vec();
    s.take(&mut buf)
        .into_iter()
        .map(|a| serde_json::from_slice(a.strip_prefix(&[0xff]).unwrap_or(&a)).unwrap())
        .collect()
}

#[test]
fn a_sync_is_marked_and_requests_follow_it() {
    let mut s = Session::default();
    let mut buf =
        br#"{"execute":"guest-sync-delimited","arguments":{"id":7}}{"execute":"guest-ping"}"#
            .to_vec();
    let out = s.take(&mut buf);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0][0], 0xff, "the sync's answer starts with 0xFF");
    assert_eq!(
        serde_json::from_slice::<Value>(&out[0][1..]).unwrap(),
        json!({"return": 7})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out[1]).unwrap(),
        json!({"return": {}})
    );
    assert!(buf.is_empty());
}

#[test]
fn a_request_split_across_reads_is_answered_once_whole() {
    let mut s = Session::default();
    let mut buf = br#"{"execute":"guest-sy"#.to_vec();
    assert!(s.take(&mut buf).is_empty(), "nothing yet");
    buf.extend_from_slice(br#"nc","arguments":{"id":3}}"#);
    let out = s.take(&mut buf);
    assert_eq!(
        serde_json::from_slice::<Value>(&out[0]).unwrap(),
        json!({"return": 3})
    );
}

#[test]
fn a_reset_drops_what_came_before_it() {
    let mut s = Session::default();
    let mut input = br#"{"execute":"guest-pi"#.to_vec();
    input.push(0xff);
    input.extend_from_slice(br#"{"execute":"guest-ping"}"#);
    assert_eq!(answers(&mut s, &input), [json!({"return": {}})]);
}

#[test]
fn no_file_opens_but_the_secret() {
    let mut s = Session::default();
    let out = answers(
        &mut s,
        br#"{"execute":"guest-file-open","arguments":{"path":"/etc/shadow","mode":"r"}}{"execute":"guest-file-open","arguments":{"path":"/root/.ssh/authorized_keys","mode":"w"}}"#,
    );
    for a in out {
        assert_eq!(a["error"]["class"], "GenericError", "{a}");
    }
}

#[test]
fn info_lists_what_it_answers_and_unknowns_are_refused() {
    let mut s = Session::default();
    let out = answers(
        &mut s,
        br#"{"execute":"guest-info"}{"execute":"guest-exec","arguments":{"path":"/bin/sh"}}"#,
    );
    let names: Vec<&str> = out[0]["return"]["supported_commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"guest-file-write") && names.contains(&"guest-shutdown"));
    assert!(!names.contains(&"guest-exec"));
    assert_eq!(out[1]["error"]["class"], "CommandNotFound");
}

#[test]
fn base64_broken_into_lines_is_taken() {
    let mut s = Session::default();
    // A handle no file was opened with: the base64 is read before it
    // matters, so its error is about the handle, not the newline.
    let out = answers(
        &mut s,
        br#"{"execute":"guest-file-write","arguments":{"handle":99,"buf-b64":"aGVs\nbG8=\n"}}"#,
    );
    assert_eq!(out[0]["error"]["desc"], "no such file handle", "{}", out[0]);
}
