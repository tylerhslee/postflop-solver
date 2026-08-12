use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn run_file(name: &str) -> Output {
    let executable = std::env::var_os("CARGO_BIN_EXE_postflop-cli")
        .expect("Cargo should build a binary target named postflop-cli");
    Command::new(executable)
        .arg("solve")
        .arg("--input")
        .arg(fixture(name))
        .output()
        .expect("postflop-cli should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be UTF-8 JSON")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be UTF-8")
}

fn compact_json(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in input.chars() {
        if in_string {
            result.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
            result.push(ch);
        } else if !ch.is_whitespace() {
            result.push(ch);
        }
    }
    result
}

fn assert_json(output: &Output) -> String {
    let raw = stdout(output);
    let trimmed = raw.trim();
    assert!(
        trimmed.starts_with('{') && trimmed.ends_with('}'),
        "{trimmed}"
    );
    compact_json(trimmed)
}

fn assert_contains(json: &str, fragment: &str) {
    assert!(json.contains(fragment), "expected {fragment}, got: {json}");
}

fn number_after(json: &str, object_marker: &str, key: &str) -> f64 {
    let object = json
        .find(object_marker)
        .expect("object marker should exist");
    let tail = &json[object + object_marker.len()..];
    let key = tail.find(key).expect("numeric key should exist") + key.len();
    let value = &tail[key..];
    let end = value
        .find(|ch: char| !matches!(ch, '-' | '+' | '.' | '0'..='9' | 'e' | 'E'))
        .unwrap_or(value.len());
    value[..end].parse().expect("value should be numeric")
}

#[test]
fn iteration_limit_returns_an_unconverged_best_so_far_strategy() {
    let output = run_file("max_iterations_river.json");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let json = assert_json(&output);
    assert_contains(&json, r#""status":"solved""#);
    assert_contains(&json, r#""best_so_far":true"#);
    assert_contains(&json, r#""converged":false"#);
    assert_contains(&json, r#""stop_reason":"max_iterations""#);
    assert_contains(&json, r#""iterations":1"#);
}

#[test]
fn duplicate_nodelocks_for_the_same_path_and_player_are_rejected() {
    let output = run_file("duplicate_nodelock.json");
    assert!(!output.status.success(), "duplicate nodelocks should fail");
    let json = assert_json(&output);
    assert_contains(&json, r#""ok":false"#);
    assert_contains(&json, r#""error":{"code":"duplicate_nodelock""#);
    assert_contains(&json, r#""field":"nodelocks""#);
}

#[test]
fn canonical_chance_card_labels_reach_downstream_decision_nodes() {
    let output = run_file("turn_chance_path.json");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let json = assert_json(&output);
    assert_contains(
        &json,
        r#""path":["check","check","chance:7s"],"player":"oop""#,
    );
    assert_contains(&json, r#""QsQh":{"actions":{"check":"#);
}

#[test]
fn reversed_hole_card_spelling_is_accepted_and_returned_canonically() {
    let output = run_file("reversed_combo.json");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let json = assert_json(&output);
    assert_contains(&json, r#""AsAh":{"actions":{"check":"#);
    assert!(!json.contains(r#""AhAs":{"actions"#), "{json}");
    let marker = r#""AsAh":{"actions":{"#;
    assert!((number_after(&json, marker, r#""check":"#) - 0.2).abs() < 1e-5);
    assert!((number_after(&json, marker, r#""all-in":"#) - 0.8).abs() < 1e-5);
}
