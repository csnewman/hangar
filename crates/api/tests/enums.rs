//! Every enum on the wire reads a value it does not know as its Other: a
//! newer side may send one, and one unknown value must not fail the whole
//! message it is in.

use hangar_api::*;
use serde::de::DeserializeOwned;

fn unknown<T: DeserializeOwned>() -> T {
    serde_json::from_str("\"a-value-from-a-newer-side\"").unwrap()
}

#[test]
fn an_unknown_value_reads_as_other() {
    assert_eq!(unknown::<DesiredState>(), DesiredState::Other);
    assert_eq!(unknown::<Phase>(), Phase::Other);
    assert_eq!(unknown::<Display>(), Display::Other);
    assert_eq!(unknown::<Gpu>(), Gpu::Other);
    assert_eq!(unknown::<Squash>(), Squash::Other);
    assert_eq!(unknown::<Step>(), Step::Other);
    assert_eq!(unknown::<ImageState>(), ImageState::Other);
}

#[test]
fn a_report_with_an_unknown_phase_and_step_reads() {
    let st: WorkerStatus = serde_json::from_value(serde_json::json!({
        "capacity": {"cpus": 4, "memory_mib": 8192},
        "labels": {},
        "environments": [{
            "id": "e",
            "phase": "hibernating",
            "progress": {"step": "warming"},
        }],
        "images": [{
            "ref": "r", "digest": "d", "size_bytes": 1, "state": "verifying",
            "environments": [], "current": true,
        }],
    }))
    .unwrap();
    assert_eq!(st.environments[0].phase, Phase::Other);
    assert_eq!(
        st.environments[0].progress.as_ref().unwrap().step,
        Step::Other
    );
    assert_eq!(st.images[0].state, ImageState::Other);
}

/// Every enum the crate reads has an Other, so one added without it fails
/// here rather than on the wire.
#[test]
fn every_enum_read_has_an_other() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0;
    for f in std::fs::read_dir(&dir).unwrap() {
        let f = f.unwrap().path();
        let src = std::fs::read_to_string(&f).unwrap();
        let mut rest = src.as_str();
        while let Some(at) = rest.find("pub enum ") {
            let before = &rest[..at];
            let derive = before
                .rfind("#[derive(")
                .map(|d| &before[d..])
                .unwrap_or("");
            let body_at = rest[at..].find('{').unwrap() + at;
            let body_end = rest[body_at..].find("\n}").unwrap() + body_at;
            let name = rest[at + 9..body_at].trim();
            if derive.contains("Deserialize") {
                assert!(
                    rest[body_at..body_end].contains("#[serde(other)]"),
                    "{}: {name} has no #[serde(other)] variant",
                    f.display()
                );
                checked += 1;
            }
            rest = &rest[body_end..];
        }
    }
    assert!(checked >= 7, "found {checked} enums");
}
