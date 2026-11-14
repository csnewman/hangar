//! Generates Rust for Hangar's two HTTP APIs from their OpenAPI specs, with
//! progenitor, into OUT_DIR: the specs stay the source of truth, as they
//! are for the Go server (oapi-codegen) and the web UI (openapi-typescript).
//!
//! Two things are changed for the generator alone, the specs read as they
//! are:
//!
//! - progenitor takes one success body per operation, and a few operations
//!   answer two ways (SECOND_ANSWERS). The second answer is dropped here and
//!   read back by src/answers.rs from the response progenitor did not expect.
//! - The client API is versioned, and a client "treats an enum value it does
//!   not know as the description says" (its compatibility rule). progenitor's
//!   enums refuse an unknown value, so the client API's string enums are
//!   generated as strings, their values kept in the docs. The web UI's API
//!   ships with its UI and keeps strict enums.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The client API's operations that answer two ways, and the second answer.
const CLIENT_SECOND_ANSWERS: &[(&str, &str)] =
    &[("finishSignIn", "202"), ("deleteEnvironment", "204")];

/// The web UI API's.
const FRONTEND_SECOND_ANSWERS: &[(&str, &str)] = &[("deleteEnvironment", "204")];

fn main() {
    let specs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../internal");
    generate(
        &specs.join("clientapi/openapi.yaml"),
        "clientapi.rs",
        CLIENT_SECOND_ANSWERS,
        true,
    );
    generate(
        &specs.join("frontendapi/openapi.yaml"),
        "frontendapi.rs",
        FRONTEND_SECOND_ANSWERS,
        false,
    );
}

fn generate(spec: &Path, out: &str, second: &[(&str, &str)], open_enums: bool) {
    println!("cargo:rerun-if-changed={}", spec.display());
    let name = spec.display();
    let text = std::fs::read_to_string(spec).unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut doc: Value = serde_saphyr::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    drop_second_answers(&mut doc, second, &name.to_string());
    if open_enums {
        open(&mut doc);
    }
    let api: openapiv3::OpenAPI =
        serde_json::from_value(doc).unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut settings = progenitor::GenerationSettings::default();
    settings
        .with_interface(progenitor::InterfaceStyle::Positional)
        .with_derive("PartialEq");
    let tokens = progenitor::Generator::new(&settings)
        .generate_tokens(&api)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let file = syn::parse2(tokens).unwrap_or_else(|e| panic!("{name}: {e}"));
    let dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    std::fs::write(Path::new(&dir).join(out), prettyplease::unparse(&file))
        .unwrap_or_else(|e| panic!("{out}: {e}"));
}

/// Drops each operation's second answer, refusing a spec where an operation
/// answers two ways that `second` does not name, or where `second` names an
/// answer the spec does not have: either is src/answers.rs out of step.
fn drop_second_answers(doc: &mut Value, second: &[(&str, &str)], name: &str) {
    let shared = doc["components"]["responses"].clone();
    let mut seen = Vec::new();
    let Some(paths) = doc["paths"].as_object_mut() else {
        panic!("{name}: no paths");
    };
    for item in paths.values_mut() {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        for op in item.values_mut() {
            let Some(id) = op["operationId"].as_str().map(str::to_string) else {
                continue;
            };
            let Some(responses) = op["responses"].as_object_mut() else {
                continue;
            };
            for (op_id, status) in second {
                if *op_id == id && responses.remove(*status).is_some() {
                    seen.push((*op_id, *status));
                }
            }
            let success: Vec<&String> = responses.keys().filter(|s| s.starts_with('2')).collect();
            let bodies: BTreeSet<String> = responses
                .iter()
                .filter(|(status, _)| status.starts_with('2'))
                .map(|(_, r)| body(r, &shared))
                .collect();
            if bodies.len() > 1 {
                panic!(
                    "{name}: {id} answers {success:?} with different bodies, and progenitor \
                     takes one: name the second in build.rs and read it back in src/answers.rs"
                );
            }
        }
    }
    for entry in second {
        if !seen.contains(entry) {
            panic!("{name}: build.rs names {entry:?}, which the spec does not answer");
        }
    }
}

/// What a response carries, to tell two apart: its schema's reference, or
/// nothing.
fn body(response: &Value, shared: &Value) -> String {
    let response = match response["$ref"].as_str() {
        Some(r) => &shared[r.rsplit('/').next().unwrap_or(r)],
        None => response,
    };
    match response["content"].as_object() {
        Some(content) => content
            .values()
            .map(|c| c["schema"].to_string())
            .collect::<Vec<_>>()
            .join(","),
        None => String::new(),
    }
}

/// Makes every string enum in doc a string, its values said in its
/// description.
fn open(doc: &mut Value) {
    match doc {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("string")
                && let Some(Value::Array(values)) = map.remove("enum")
            {
                let values: Vec<String> = values
                    .iter()
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                    .collect();
                let known = format!(
                    "One of {}, or a value a newer server adds.",
                    values.join(", ")
                );
                let description = match map.get("description").and_then(Value::as_str) {
                    Some(d) if !d.trim().is_empty() => format!("{}\n\n{known}", d.trim_end()),
                    _ => known,
                };
                map.insert("description".into(), Value::String(description));
            }
            for v in map.values_mut() {
                open(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                open(v);
            }
        }
        _ => {}
    }
}
