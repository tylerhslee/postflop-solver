use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn cli() -> Command {
    let executable = std::env::var_os("CARGO_BIN_EXE_postflop-cli").unwrap_or_else(|| {
        panic!(
            "Cargo did not provide CARGO_BIN_EXE_postflop-cli; add a binary target named postflop-cli"
        )
    });
    Command::new(executable)
}

fn run_file(name: &str, pretty: bool) -> Output {
    let mut command = cli();
    command.arg("solve").arg("--input").arg(fixture(name));
    if pretty {
        command.arg("--pretty");
    }
    command.output().expect("postflop-cli should run")
}

fn run_stdin(name: &str) -> Output {
    let input = fs::read(fixture(name)).expect("fixture should be readable");
    let mut child = cli()
        .arg("solve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("postflop-cli should start");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(&input)
        .expect("fixture should be written to stdin");
    child.wait_with_output().expect("postflop-cli should exit")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be UTF-8 JSON")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be UTF-8")
}

/// Removes insignificant JSON whitespace without changing whitespace inside strings.
/// This keeps the tests independent of a particular JSON crate and supports --pretty.
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

/// Checks that stdout contains exactly one top-level JSON object, with no logs or
/// second response before/after it. Full syntax validation remains the CLI's job.
fn assert_single_json_object(text: &str) {
    let text = text.trim();
    assert!(
        text.starts_with('{'),
        "stdout must start with a JSON object: {text}"
    );
    assert!(
        text.ends_with('}'),
        "stdout must end with a JSON object: {text}"
    );

    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut closed_at = None;
    for (index, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                assert!(depth >= 0, "stdout has an unmatched closing brace: {text}");
                if depth == 0 {
                    closed_at = Some(index + ch.len_utf8());
                    break;
                }
            }
            _ => {}
        }
    }

    assert_eq!(
        closed_at,
        Some(text.len()),
        "stdout must contain one JSON object and nothing else: {text}"
    );
}

fn assert_contains(compact: &str, fragment: &str) {
    assert!(
        compact.contains(fragment),
        "expected JSON fragment {fragment}, got: {compact}"
    );
}

fn number_after(compact: &str, object_marker: &str, key: &str) -> f64 {
    let object_start = compact
        .find(object_marker)
        .unwrap_or_else(|| panic!("missing object marker {object_marker}: {compact}"));
    let tail = &compact[object_start + object_marker.len()..];
    let key_start = tail
        .find(key)
        .unwrap_or_else(|| panic!("missing numeric key {key} after {object_marker}: {compact}"));
    let value = &tail[key_start + key.len()..];
    let end = value
        .find(|ch: char| !matches!(ch, '-' | '+' | '.' | '0'..='9' | 'e' | 'E'))
        .unwrap_or(value.len());
    value[..end]
        .parse()
        .unwrap_or_else(|_| panic!("invalid number for {key}: {value}"))
}

fn assert_approx(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-5,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn capabilities_describe_the_agent_facing_solver_contract() {
    let output = cli()
        .arg("capabilities")
        .output()
        .expect("postflop-cli should run");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let raw = stdout(&output);
    assert_single_json_object(&raw);
    let json = compact_json(&raw);
    assert_contains(&json, r#""ok":true"#);
    assert_contains(&json, r#""command":"capabilities""#);
    assert_contains(&json, r#""schema_version":"1""#);
    assert_contains(&json, r#""postflop_only":true"#);
    assert_contains(&json, r#""initial_streets":["flop","turn","river"]"#);
    assert_contains(&json, r#""custom_bet_sizes":true"#);
    assert_contains(&json, r#""max_bet_sizes_per_node":null"#);
    assert_contains(
        &json,
        r#""canonical_action_labels":["check","fold","call","bet:<chips>","raise:<chips>","all-in"]"#,
    );
    assert_contains(
        &json,
        r#""nodelocking":{"supported":true,"partial":true,"downstream_paths":true}"#,
    );
    assert_contains(&json, r#""compression":true"#);
    assert_contains(&json, r#""deadlines":true"#);
}

#[test]
fn solves_a_tiny_river_baseline_from_a_file_and_keeps_progress_off_stdout() {
    let output = run_file("tiny_river_baseline.json", true);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let raw = stdout(&output);
    assert!(
        raw.contains('\n'),
        "--pretty should pretty-print the JSON response"
    );
    assert_single_json_object(&raw);
    let json = compact_json(&raw);
    assert_contains(&json, r#""ok":true"#);
    assert_contains(&json, r#""command":"solve""#);
    assert_contains(&json, r#""status":"solved""#);
    assert_contains(&json, r#""iterations":"#);
    assert_contains(&json, r#""exploitability":"#);
    assert_contains(&json, r#""target_exploitability":0.001"#);
    assert_contains(&json, r#""root_actions":["check","all-in"]"#);
    assert_contains(&json, r#""path":[],"player":"oop""#);
    assert_contains(&json, r#""AsAh":{"actions":{"check":"#);

    let progress = stderr(&output).to_ascii_lowercase();
    assert!(
        progress.contains("iter") || progress.contains("solv"),
        "solve progress belongs on stderr, got: {progress}"
    );
}

#[test]
fn solve_reads_the_same_request_from_stdin_when_input_is_omitted() {
    let output = run_stdin("tiny_river_baseline.json");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let raw = stdout(&output);
    assert_single_json_object(&raw);
    let json = compact_json(&raw);
    assert_contains(&json, r#""ok":true"#);
    assert_contains(&json, r#""status":"solved""#);
    assert_contains(&json, r#""root_actions":["check","all-in"]"#);
}

#[test]
fn invalid_board_returns_a_nonzero_exit_and_structured_json_error() {
    let output = run_file("invalid_board.json", false);
    assert!(!output.status.success(), "invalid input should fail");

    let raw = stdout(&output);
    assert_single_json_object(&raw);
    let json = compact_json(&raw);
    assert_contains(&json, r#""ok":false"#);
    assert_contains(&json, r#""command":"solve""#);
    assert_contains(&json, r#""error":{"code":"invalid_board""#);
    assert_contains(&json, r#""field":"board""#);
}

#[test]
fn unknown_request_fields_are_rejected_instead_of_silently_ignored() {
    let output = run_file("unknown_field.json", false);
    assert!(!output.status.success(), "unknown fields should fail");

    let raw = stdout(&output);
    assert_single_json_object(&raw);
    let json = compact_json(&raw);
    assert_contains(&json, r#""ok":false"#);
    assert_contains(&json, r#""error":{"code":"unknown_field""#);
    assert_contains(&json, r#""field":"unexpected""#);
}

#[test]
fn applies_a_partial_combo_nodelock_at_the_root() {
    let output = run_file("partial_root_nodelock.json", false);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let raw = stdout(&output);
    assert_single_json_object(&raw);
    let json = compact_json(&raw);
    assert_contains(&json, r#""status":"solved""#);
    assert_contains(
        &json,
        r#""nodelocks":[{"path":[],"player":"oop","applied":true"#,
    );
    assert_contains(&json, r#""unlocked_hands_preserved":true"#);
    assert_contains(&json, r#""path":[],"player":"oop""#);

    assert_approx(
        number_after(&json, r#""JsJh":{"actions":{"#, r#""check":"#),
        0.8,
    );
    assert_approx(
        number_after(&json, r#""JsJh":{"actions":{"#, r#""all-in":"#),
        0.2,
    );
}

#[test]
fn resolves_and_applies_a_nodelock_at_a_canonical_downstream_path() {
    let output = run_file("downstream_nodelock.json", false);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let raw = stdout(&output);
    assert_single_json_object(&raw);
    let json = compact_json(&raw);
    assert_contains(&json, r#""status":"solved""#);
    assert_contains(
        &json,
        r#""nodelocks":[{"path":["check","all-in"],"player":"oop","applied":true"#,
    );
    assert_contains(&json, r#""path":["check","all-in"],"player":"oop""#);

    assert_approx(
        number_after(&json, r#""QsQh":{"actions":{"#, r#""fold":"#),
        0.25,
    );
    assert_approx(
        number_after(&json, r#""QsQh":{"actions":{"#, r#""call":"#),
        0.75,
    );
}

#[test]
fn deadline_returns_a_successful_best_so_far_strategy() {
    let output = run_file("deadline_river.json", false);
    assert!(
        output.status.success(),
        "deadline is not an input error: {}",
        stderr(&output)
    );

    let raw = stdout(&output);
    assert_single_json_object(&raw);
    let json = compact_json(&raw);
    assert_contains(&json, r#""ok":true"#);
    assert_contains(&json, r#""status":"deadline_reached""#);
    assert_contains(&json, r#""best_so_far":true"#);
    assert_contains(&json, r#""iterations":"#);
    assert_contains(&json, r#""root_actions":["check","all-in"]"#);
    assert_contains(&json, r#""AsAh":{"actions":{"check":"#);
}
