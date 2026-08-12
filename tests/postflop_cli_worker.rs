use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_slice(&fs::read(fixture(name)).expect("fixture should be readable"))
        .expect("fixture should contain valid JSON")
}

struct Worker {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Worker {
    fn start() -> Self {
        let executable = std::env::var_os("CARGO_BIN_EXE_postflop-cli")
            .expect("Cargo should build postflop-cli");
        let mut child = Command::new(executable)
            .arg("worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("postflop-cli worker should start");
        let input = BufWriter::new(child.stdin.take().expect("worker stdin should be piped"));
        let output = BufReader::new(child.stdout.take().expect("worker stdout should be piped"));
        Self {
            child,
            input,
            output,
        }
    }

    fn request(&mut self, request: Value) -> Value {
        serde_json::to_writer(&mut self.input, &request).expect("request should serialize");
        self.input
            .write_all(b"\n")
            .expect("worker should accept another request");
        self.input.flush().expect("worker request should flush");
        self.response()
    }

    fn raw_request(&mut self, request: &str) -> Value {
        self.input
            .write_all(request.as_bytes())
            .expect("worker should accept raw request");
        self.input
            .write_all(b"\n")
            .expect("worker should accept a request delimiter");
        self.input.flush().expect("worker request should flush");
        self.response()
    }

    fn response(&mut self) -> Value {
        let mut line = String::new();
        let bytes = self
            .output
            .read_line(&mut line)
            .expect("worker response should be readable");
        assert!(
            bytes > 0,
            "worker exited before returning an NDJSON response"
        );
        assert!(
            line.ends_with('\n'),
            "each worker response must be newline-delimited: {line:?}"
        );
        serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!("worker response should be one JSON object: {error}: {line:?}")
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn solve(worker: &mut Worker, id: &str, request: Value) -> Value {
    worker.request(json!({
        "id": id,
        "command": "solve",
        "request": request,
    }))
}

fn assert_success<'a>(response: &'a Value, id: &str, command: &str) -> &'a Value {
    assert_eq!(response["id"], id);
    assert_eq!(response["ok"], true);
    assert_eq!(response["command"], command);
    response
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("expected a JSON object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn assert_complete_combo_strategy(node: &Value, expected_hands: &[&str]) {
    let actions = node["actions"]
        .as_array()
        .expect("node actions should be an array");
    assert!(
        !actions.is_empty(),
        "decision node should expose its actions"
    );
    let action_names = actions
        .iter()
        .map(|action| action.as_str().expect("action labels should be strings"))
        .collect::<BTreeSet<_>>();

    let hands = &node["hands"];
    assert_eq!(keys(hands), expected_hands.iter().copied().collect());
    for (combo, hand) in hands.as_object().expect("hands should be an object") {
        assert!(
            hand["ev"].is_number(),
            "{combo} should include a numeric EV"
        );
        assert_eq!(
            keys(&hand["actions"]),
            action_names,
            "{combo} should include a frequency for every node action"
        );
        let frequency_sum: f64 = hand["actions"]
            .as_object()
            .expect("combo actions should be an object")
            .values()
            .map(|frequency| {
                frequency
                    .as_f64()
                    .expect("action frequency should be numeric")
            })
            .sum();
        assert!(
            (frequency_sum - 1.0).abs() < 1e-5,
            "{combo} action frequencies should sum to one, got {frequency_sum}"
        );
    }
}

#[test]
fn solve_cache_ignores_inspection_paths_and_query_node_reuses_the_game() {
    let mut worker = Worker::start();
    let mut root_request = fixture_json("tiny_river_baseline.json");
    root_request["solve"]["max_iterations"] = json!(10);
    root_request["solve"]["target_exploitability"] = json!(1000.0);

    let first = solve(&mut worker, "solve-root", root_request.clone());
    assert_success(&first, "solve-root", "solve");
    assert_eq!(first["cache_hit"], false);
    let solution_id = first["solution_id"]
        .as_str()
        .expect("solve should return a stable solution handle")
        .to_owned();

    root_request["inspect_hands"] = json!([{
        "path": ["check"],
        "player": "ip",
        "hands": ["KsKh"]
    }]);
    let cached = solve(&mut worker, "solve-other-inspection", root_request);
    assert_success(&cached, "solve-other-inspection", "solve");
    assert_eq!(cached["cache_hit"], true);
    assert_eq!(cached["solution_id"], solution_id);

    let root = worker.request(json!({
        "id": "query-root",
        "command": "query_node",
        "solution_id": solution_id,
        "path": [],
    }));
    assert_success(&root, "query-root", "query_node");
    assert_eq!(root["actor"], "oop");
    assert_complete_combo_strategy(&root, &["AsAh", "QsQh"]);

    let downstream = worker.request(json!({
        "id": "query-after-check",
        "command": "query_node",
        "solution_id": solution_id,
        "path": ["check"],
    }));
    assert_success(&downstream, "query-after-check", "query_node");
    assert_eq!(downstream["actor"], "ip");
    assert_complete_combo_strategy(&downstream, &["KsKh"]);
}

#[test]
fn query_node_follows_a_chance_path_and_infers_the_next_actor() {
    let mut worker = Worker::start();
    let response = solve(
        &mut worker,
        "solve-turn",
        fixture_json("turn_chance_path.json"),
    );
    assert_success(&response, "solve-turn", "solve");
    let solution_id = response["solution_id"]
        .as_str()
        .expect("solve should return a solution handle");

    let node = worker.request(json!({
        "id": "query-river",
        "command": "query_node",
        "solution_id": solution_id,
        "path": ["check", "check", "chance:7s"],
    }));
    assert_success(&node, "query-river", "query_node");
    assert_eq!(node["actor"], "oop");
    assert_complete_combo_strategy(&node, &["AsAh", "QsQh"]);
}

#[test]
fn health_and_evict_report_and_release_persistent_solutions() {
    let mut worker = Worker::start();
    let initial = worker.request(json!({"id": "health-empty", "command": "health"}));
    assert_success(&initial, "health-empty", "health");
    assert_eq!(initial["status"], "ready");
    assert_eq!(initial["solution_count"], 0);

    let response = solve(
        &mut worker,
        "solve-health",
        fixture_json("tiny_river_baseline.json"),
    );
    let solution_id = response["solution_id"]
        .as_str()
        .expect("solve should return a solution handle")
        .to_owned();

    let populated = worker.request(json!({"id": "health-one", "command": "health"}));
    assert_success(&populated, "health-one", "health");
    assert_eq!(populated["solution_count"], 1);

    let evicted = worker.request(json!({
        "id": "evict-one",
        "command": "evict",
        "solution_id": solution_id,
    }));
    assert_success(&evicted, "evict-one", "evict");
    assert_eq!(evicted["evicted"], true);
    assert_eq!(evicted["solution_count"], 0);

    let missing = worker.request(json!({
        "id": "query-evicted",
        "command": "query_node",
        "solution_id": solution_id,
        "path": [],
    }));
    assert_eq!(missing["id"], "query-evicted");
    assert_eq!(missing["ok"], false);
    assert_eq!(missing["command"], "query_node");
    assert_eq!(missing["error"]["code"], "unknown_solution");
}

#[test]
fn malformed_ndjson_returns_an_error_without_terminating_the_worker() {
    let mut worker = Worker::start();
    let malformed = worker.raw_request(r#"{"id":"broken","command": }"#);
    assert_eq!(malformed["ok"], false);
    assert_eq!(malformed["command"], "worker");
    assert_eq!(malformed["error"]["code"], "invalid_json");

    let health = worker.request(json!({"id": "still-alive", "command": "health"}));
    assert_success(&health, "still-alive", "health");
    assert_eq!(health["status"], "ready");
}
