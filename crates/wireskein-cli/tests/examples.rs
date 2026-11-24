//! The repository's example captures analyzed by the Rust command line: each bus their truth names claimed (the
//! same protocol on the same lines), confirmed; the protocol examples read and claimed.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn analyze(file: &PathBuf) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_wireskein-rs"))
        .arg("analyze")
        .arg(file)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn lines(roles: &Value) -> BTreeSet<String> {
    roles
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_examples_buses_are_claimed() {
    for name in ["mixed-0", "mixed-1", "rvswd-v203-target-info", "swio-v003-target-info"] {
        let dir = repo().join("tests/data/examples");
        let doc = analyze(&dir.join(format!("{name}.wireskein")));
        assert_eq!(doc["format"], "wireskein-analysis/1");
        let truth: Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{name}.truth.json"))).unwrap()).unwrap();
        let claims = doc["parts"][0]["claims"].as_array().unwrap();
        for bus in truth["buses"].as_array().unwrap() {
            let want = lines(&bus["roles"]);
            let hit = claims
                .iter()
                .find(|c| c["protocol"] == bus["protocol"] && lines(&c["roles"]) == want);
            let hit = hit.unwrap_or_else(|| panic!("{name}: {} on {want:?} not claimed", bus["protocol"]));
            assert_eq!(hit["verdict"], "confirmed", "{name}: {hit}");
            assert!(!hit["result"].as_object().unwrap().is_empty(), "{name}: no result");
        }
    }
}

#[test]
fn the_protocol_examples_are_read() {
    let dir = repo().join("examples/protocols");
    for name in [
        "demo_cmd_p4",
        "demo_link_p4",
        "demo_telemetry_p4_can",
        "demo_telemetry_p4_lin",
    ] {
        let doc = analyze(&dir.join(format!("{name}.wireskein")));
        assert_eq!(doc["capture"]["complete"], true);
        assert!(
            !doc["parts"][0]["claims"].as_array().unwrap().is_empty(),
            "{name}: no claim"
        );
    }
}
