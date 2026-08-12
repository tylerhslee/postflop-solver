use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn fixture(name: &str) -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name) }
fn fixture_json(name: &str) -> Value { serde_json::from_slice(&fs::read(fixture(name)).unwrap()).unwrap() }
struct Worker { child: Child, input: BufWriter<ChildStdin>, output: BufReader<ChildStdout> }
impl Worker {
    fn start(max_solutions: usize) -> Self {
        let executable = std::env::var_os("CARGO_BIN_EXE_postflop-cli").expect("Cargo should build CLI");
        let mut child = Command::new(executable).arg("worker")
            .env("POSTFLOP_WORKER_MAX_SOLUTIONS", max_solutions.to_string())
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let input = BufWriter::new(child.stdin.take().unwrap());
        let output = BufReader::new(child.stdout.take().unwrap()); Self { child, input, output }
    }
    fn request(&mut self, value: Value) -> Value {
        serde_json::to_writer(&mut self.input, &value).unwrap(); self.input.write_all(b"\n").unwrap(); self.input.flush().unwrap();
        let mut line = String::new(); assert!(self.output.read_line(&mut line).unwrap() > 0); serde_json::from_str(&line).unwrap()
    }
    fn solve(&mut self, id: &str, request: Value) -> Value { self.request(json!({"id":id,"command":"solve","request":request})) }
    fn query(&mut self, id: &str, solution_id: &str, path: Value) -> Value {
        self.request(json!({"id":id,"command":"query_node","solution_id":solution_id,"path":path}))
    }
}
impl Drop for Worker { fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); } }

#[test]
fn worker_solution_cache_is_bounded_and_evicts_least_recently_used() {
    let mut worker = Worker::start(2);
    let mut first_request = fixture_json("tiny_river_baseline.json"); first_request["solve"]["max_iterations"] = json!(10);
    first_request["solve"]["target_exploitability"] = json!(1000.0);
    let first = worker.solve("first", first_request.clone()); let first_id = first["solution_id"].as_str().unwrap().to_owned();
    let mut second_request = first_request.clone(); second_request["pot"] = json!(21);
    let second = worker.solve("second", second_request); let second_id = second["solution_id"].as_str().unwrap().to_owned();
    assert_eq!(worker.query("touch-first", &first_id, json!([]))["ok"], true, "querying must refresh LRU recency");
    let mut third_request = first_request; third_request["pot"] = json!(22); worker.solve("third", third_request);
    assert_eq!(worker.query("kept-first", &first_id, json!([]))["ok"], true);
    let evicted = worker.query("evicted-second", &second_id, json!([]));
    assert_eq!(evicted["ok"], false); assert_eq!(evicted["error"]["code"], "unknown_solution");
    let health = worker.request(json!({"id":"health","command":"health"})); assert_eq!(health["solution_count"], 2);
}

#[test]
fn query_node_exposes_canonical_decision_children_for_tree_browsing() {
    let mut worker = Worker::start(4); let mut request = fixture_json("tiny_river_baseline.json");
    request["solve"]["max_iterations"] = json!(10); request["solve"]["target_exploitability"] = json!(1000.0);
    let solved = worker.solve("solve", request); let id = solved["solution_id"].as_str().unwrap().to_owned();
    let root = worker.query("root", &id, json!([])); assert_eq!(root["node_kind"], "decision");
    let actions = root["actions"].as_array().unwrap(); let children = root["children"].as_array().expect("decision children");
    assert_eq!(children.len(), actions.len());
    for (index, child) in children.iter().enumerate() {
        assert_eq!(child["action"], actions[index]); assert_eq!(child["path"], json!([actions[index].clone()]));
        assert!(child["node_kind"].is_string(), "child must advertise its destination kind");
    }
}

#[test]
fn query_node_describes_chance_and_terminal_nodes_without_strategy_hands() {
    let mut worker = Worker::start(4); let response = worker.solve("turn", fixture_json("turn_chance_path.json"));
    let id = response["solution_id"].as_str().unwrap().to_owned();
    let chance = worker.query("chance", &id, json!(["check", "check"]));
    assert_eq!(chance["ok"], true); assert_eq!(chance["node_kind"], "chance"); assert!(chance["actor"].is_null());
    let children = chance["children"].as_array().expect("chance card children"); assert!(!children.is_empty());
    for child in children { assert!(child["action"].as_str().unwrap().starts_with("chance:")); assert!(child["probability"].is_number()); }

    let mut river = fixture_json("tiny_river_baseline.json"); river["solve"]["max_iterations"] = json!(10);
    river["solve"]["target_exploitability"] = json!(1000.0); let solved = worker.solve("river", river);
    let terminal = worker.query("terminal", solved["solution_id"].as_str().unwrap(), json!(["check", "check"]));
    assert_eq!(terminal["ok"], true); assert_eq!(terminal["node_kind"], "terminal"); assert!(terminal["actor"].is_null());
    assert_eq!(terminal["children"], json!([])); assert!(terminal["terminal"]["reason"].is_string());
}
