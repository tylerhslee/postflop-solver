use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn wildcard_inspection_returns_every_reachable_private_combo_at_the_node() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny_river_baseline.json");
    let mut request: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture).expect("fixture should be readable"))
            .expect("fixture should be valid JSON");
    request["inspect_hands"][0]["hands"] = serde_json::json!(["*"]);

    let executable =
        std::env::var_os("CARGO_BIN_EXE_postflop-cli").expect("Cargo should build postflop-cli");
    let mut child = Command::new(executable)
        .arg("solve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("postflop-cli should start");
    child
        .stdin
        .as_mut()
        .expect("stdin should be available")
        .write_all(&serde_json::to_vec(&request).expect("request should serialize"))
        .expect("request should be written");
    let output = child.wait_with_output().expect("postflop-cli should exit");

    assert!(
        output.status.success(),
        "wildcard inspection should solve: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be one JSON response");
    let hands = response["inspections"][0]["hands"]
        .as_object()
        .expect("inspection hands should be an object");
    let mut returned = hands.keys().map(String::as_str).collect::<Vec<_>>();
    returned.sort_unstable();
    assert_eq!(returned, ["AsAh", "QsQh"]);
    assert!(
        !hands.contains_key("*"),
        "wildcard is a selector, not a hand key"
    );
    for hand in hands.values() {
        let actions = hand["actions"]
            .as_object()
            .expect("each combo needs a strategy");
        assert!(
            !actions.is_empty(),
            "each reachable combo needs action frequencies"
        );
        let total: f64 = actions
            .values()
            .map(|frequency| frequency.as_f64().expect("frequency should be numeric"))
            .sum();
        assert!(
            (total - 1.0).abs() < 1e-5,
            "frequencies should sum to one, got {total}"
        );
        assert!(hand["ev"].is_number(), "each reachable combo needs an EV");
    }
}
