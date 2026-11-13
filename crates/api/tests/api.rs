//! The shapes read from and written as internal/api's JSON: tests/data's
//! lines are what the Go package writes for the same values.

use std::time::{Duration, UNIX_EPOCH};

use hangar_api::*;
use serde_json::Value;

fn go() -> Vec<Value> {
    include_str!("data/go.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Whether two JSON values say the same, null standing for an empty list or
/// map as the server writes them.
fn same(a: &Value, b: &Value) -> bool {
    let empty = |v: &Value| match v {
        Value::Null => true,
        Value::Array(x) => x.is_empty(),
        Value::Object(x) => x.is_empty(),
        _ => false,
    };
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().all(|k| match (x.get(k), y.get(k)) {
                (Some(p), Some(q)) => same(p, q),
                (Some(p), None) | (None, Some(p)) => empty(p),
                (None, None) => true,
            })
        }
        // Go writes a float that is whole without its point.
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        _ if empty(a) && empty(b) => true,
        _ => a == b,
    }
}

fn round<T: serde::de::DeserializeOwned + serde::Serialize>(v: &Value) -> T {
    let t: T = serde_json::from_value(v.clone()).unwrap();
    let back = serde_json::to_value(&t).unwrap();
    assert!(same(v, &back), "\ngo:   {v}\nrust: {back}");
    t
}

#[test]
fn desired_set() {
    let g = go();
    let ds: DesiredSet = round(&g[0]);
    assert_eq!(ds.environments.len(), 2);
    let e = &ds.environments[0];
    assert_eq!(e.desired, DesiredState::Running);
    assert_eq!(e.pin_digest.as_deref(), Some("sha256:abc"));
    assert_eq!(e.spec.display, Display::Desktop);
    assert_eq!(e.spec.gpu, Gpu::Virtual);
    assert_eq!(e.spec.swap_mib, Some(2048));
    assert!(e.spec.access.no_sensitive_files && !e.spec.access.no_ssh_keys);
    assert_eq!(e.spec.placement["runtime"], "krun");
    assert_eq!(ds.environments[1].desired, DesiredState::Suspended);
    assert_eq!(ds.environments[1].spec.display, Display::None);

    // The server's empty lists are null.
    let empty: DesiredSet = round(&g[1]);
    assert!(empty.environments.is_empty() && empty.remove_images.is_empty());
}

#[test]
fn worker_status() {
    let g = go();
    let ws: WorkerStatus = round(&g[2]);
    assert_eq!(
        ws.environments[0].progress.as_ref().unwrap().step,
        Step::Download
    );
    // As Go writes a time.
    let at = UNIX_EPOCH + Duration::new(1_794_558_121, 123_456_000);
    assert_eq!(
        rfc3339(at),
        ws.environments[0].image_rollback.as_ref().unwrap().at
    );
    assert_eq!(
        rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)),
        "2000-02-29T00:00:00Z"
    );
}

#[test]
fn files() {
    let f: EnvironmentFiles = round(&go()[3]);
    assert_eq!(f.squash["b"], Squash::Root);
}

#[test]
fn unknown_values() {
    let e: EnvironmentSpec = serde_json::from_str(
        r#"{"id":"x","desired":"hibernated","spec":{"display":"","gpu":"quantum","repos":null}}"#,
    )
    .unwrap();
    assert_eq!(e.desired, DesiredState::Other);
    assert_eq!(e.spec.display, Display::Other);
    assert_eq!(e.spec.gpu, Gpu::Other);
}

#[test]
fn code_trust() {
    let g = go();
    let ds: DesiredSet = serde_json::from_value(g[0].clone()).unwrap();
    let mut spec = ds.environments[0].spec.clone();
    assert_eq!(serde_json::to_value(spec.code_trust()).unwrap(), g[4]);
    assert_eq!(
        serde_json::to_value(Spec::default().code_trust()).unwrap(),
        g[5]
    );
    spec.access.no_editor_trust = false;
    assert_eq!(spec.code_trust(), ["~/b", "p", "/opt"]);
}
