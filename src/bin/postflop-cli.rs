use postflop_solver::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const SCHEMA_VERSION: &str = "1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SolveRequest {
    board: String,
    oop_range: String,
    ip_range: String,
    pot: i32,
    effective_stack: i32,
    bet_sizes: StreetBetSizes,
    thresholds: Thresholds,
    solve: SolveOptions,
    #[serde(default)]
    nodelocks: Vec<NodeLock>,
    #[serde(default)]
    inspect_hands: Vec<Inspection>,
    #[serde(default)]
    rake: Rake,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StreetBetSizes {
    flop: PlayerBetSizes,
    turn: PlayerBetSizes,
    river: PlayerBetSizes,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayerBetSizes {
    oop: BetSizes,
    ip: BetSizes,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BetSizes {
    bet: String,
    raise: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Thresholds {
    add_allin: f64,
    force_allin: f64,
    merge: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SolveOptions {
    max_iterations: u32,
    target_exploitability: f32,
    compressed: bool,
    #[serde(default)]
    deadline_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rake {
    #[serde(default)]
    rate: f64,
    #[serde(default)]
    cap: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeLock {
    path: Vec<String>,
    player: Player,
    strategy: BTreeMap<String, BTreeMap<String, f32>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inspection {
    path: Vec<String>,
    player: Player,
    hands: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Player {
    Oop,
    Ip,
}

impl Player {
    fn index(self) -> usize {
        match self {
            Self::Oop => 0,
            Self::Ip => 1,
        }
    }
}

#[derive(Debug, Serialize)]
struct CliError {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    ok: bool,
    command: &'static str,
    error: CliError,
}

#[derive(Debug, Serialize)]
struct LockResult<'a> {
    path: &'a [String],
    player: Player,
    applied: bool,
    unlocked_hands_preserved: bool,
}

#[derive(Debug, Serialize)]
struct InspectionResult<'a> {
    path: &'a [String],
    player: Player,
    hands: Map<String, Value>,
}

#[derive(Debug, Serialize)]
struct SolveResponse<'a> {
    ok: bool,
    command: &'static str,
    schema_version: &'static str,
    status: &'static str,
    best_so_far: bool,
    converged: bool,
    stop_reason: &'static str,
    iterations: u32,
    exploitability: f32,
    target_exploitability: f32,
    memory_bytes: u64,
    compressed: bool,
    root_actions: Vec<String>,
    nodelocks: Vec<LockResult<'a>>,
    inspections: Vec<InspectionResult<'a>>,
}

struct StoredSolution {
    game: PostFlopGame,
    response: Value,
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        return fail(
            "cli",
            "missing_command",
            "expected `capabilities` or `solve`",
            None,
        );
    };

    match command.as_str() {
        "capabilities" => {
            if args.next().is_some() {
                return fail(
                    "cli",
                    "invalid_arguments",
                    "capabilities takes no arguments",
                    None,
                );
            }
            emit(&capabilities(), false);
            ExitCode::SUCCESS
        }
        "solve" => run_solve(args.collect()),
        "worker" => {
            if args.next().is_some() {
                return fail(
                    "worker",
                    "invalid_arguments",
                    "worker takes no arguments",
                    None,
                );
            }
            run_worker()
        }
        _ => fail(
            "cli",
            "unknown_command",
            &format!("unknown command `{command}`"),
            None,
        ),
    }
}

fn capabilities() -> Value {
    json!({
        "ok": true,
        "command": "capabilities",
        "schema_version": SCHEMA_VERSION,
        "postflop_only": true,
        "initial_streets": ["flop", "turn", "river"],
        "custom_bet_sizes": true,
        "max_bet_sizes_per_node": null,
        "canonical_action_labels": [
            "check", "fold", "call", "bet:<chips>", "raise:<chips>", "all-in"
        ],
        "nodelocking": {
            "supported": true,
            "partial": true,
            "downstream_paths": true
        },
        "compression": true,
        "deadlines": true
    })
}

fn run_solve(args: Vec<String>) -> ExitCode {
    let mut input_path = None;
    let mut pretty = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--input" => {
                index += 1;
                if index == args.len() {
                    return fail(
                        "solve",
                        "invalid_arguments",
                        "--input requires a path",
                        None,
                    );
                }
                input_path = Some(args[index].clone());
            }
            "--pretty" => pretty = true,
            other => {
                return fail(
                    "solve",
                    "invalid_arguments",
                    &format!("unknown argument `{other}`"),
                    None,
                )
            }
        }
        index += 1;
    }

    let raw = match read_request(input_path.as_deref()) {
        Ok(raw) => raw,
        Err(error) => return fail_error("solve", error),
    };
    let request = match serde_json::from_str::<SolveRequest>(&raw) {
        Ok(request) => request,
        Err(error) => return fail_error("solve", json_error(error)),
    };

    match solve_request(&request) {
        Ok(response) => {
            emit(&response, pretty);
            ExitCode::SUCCESS
        }
        Err(error) => fail_error("solve", error),
    }
}

fn read_request(path: Option<&str>) -> Result<String, CliError> {
    if let Some(path) = path {
        fs::read_to_string(path).map_err(|error| CliError {
            code: "input_io".into(),
            message: format!("could not read `{path}`: {error}"),
            field: Some("input".into()),
        })
    } else {
        let mut raw = String::new();
        io::stdin()
            .read_to_string(&mut raw)
            .map_err(|error| CliError {
                code: "input_io".into(),
                message: format!("could not read stdin: {error}"),
                field: Some("input".into()),
            })?;
        Ok(raw)
    }
}

fn json_error(error: serde_json::Error) -> CliError {
    let message = error.to_string();
    if let Some(rest) = message.strip_prefix("unknown field `") {
        let field = rest.split('`').next().unwrap_or_default().to_owned();
        CliError {
            code: "unknown_field".into(),
            message,
            field: Some(field),
        }
    } else {
        CliError {
            code: "invalid_json".into(),
            message,
            field: None,
        }
    }
}

fn run_worker() -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    let mut solutions = HashMap::<String, StoredSolution>::new();
    let mut cache = HashMap::<String, String>::new();
    let mut next_solution = 1_u64;

    for line in BufReader::new(stdin.lock()).lines() {
        let response = match line {
            Ok(line) => match serde_json::from_str::<Value>(&line) {
                Ok(envelope) => {
                    handle_worker_request(envelope, &mut solutions, &mut cache, &mut next_solution)
                }
                Err(error) => worker_error(None, "worker", json_error(error)),
            },
            Err(error) => worker_error(
                None,
                "worker",
                CliError {
                    code: "input_io".into(),
                    message: error.to_string(),
                    field: None,
                },
            ),
        };
        if serde_json::to_writer(&mut output, &response).is_err()
            || output.write_all(b"\n").is_err()
            || output.flush().is_err()
        {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn handle_worker_request(
    envelope: Value,
    solutions: &mut HashMap<String, StoredSolution>,
    cache: &mut HashMap<String, String>,
    next_solution: &mut u64,
) -> Value {
    let id = envelope.get("id").cloned();
    let command = match envelope.get("command").and_then(Value::as_str) {
        Some(command) => command.to_owned(),
        None => {
            return worker_error(
                id,
                "worker",
                field_error("invalid_request", "command must be a string", "command"),
            );
        }
    };

    let result: Result<Value, CliError> = match command.as_str() {
        "health" => Ok(json!({
            "status": "ready",
            "solution_count": solutions.len(),
        })),
        "solve" => worker_solve(envelope.get("request"), solutions, cache, next_solution),
        "query_node" => worker_query_node(&envelope, solutions),
        "evict" => (|| {
            let solution_id = required_string(&envelope, "solution_id")?;
            let evicted = solutions.remove(solution_id).is_some();
            if evicted {
                cache.retain(|_, cached_id| cached_id != solution_id);
            }
            Ok(json!({
                "evicted": evicted,
                "solution_count": solutions.len(),
            }))
        })(),
        _ => Err(field_error(
            "unknown_command",
            &format!("unknown worker command `{command}`"),
            "command",
        )),
    };

    match result {
        Ok(mut body) => {
            body["id"] = id.unwrap_or(Value::Null);
            body["ok"] = json!(true);
            body["command"] = json!(command);
            body
        }
        Err(error) => worker_error(id, &command, error),
    }
}

fn worker_solve(
    request_value: Option<&Value>,
    solutions: &mut HashMap<String, StoredSolution>,
    cache: &mut HashMap<String, String>,
    next_solution: &mut u64,
) -> Result<Value, CliError> {
    let request_value = request_value
        .ok_or_else(|| field_error("invalid_request", "solve requires a request", "request"))?;
    let request: SolveRequest =
        serde_json::from_value(request_value.clone()).map_err(json_error)?;
    let mut key_value = request_value.clone();
    if let Some(object) = key_value.as_object_mut() {
        object.remove("inspect_hands");
    }
    let cache_key = serde_json::to_string(&key_value).map_err(|error| CliError {
        code: "internal_error".into(),
        message: error.to_string(),
        field: None,
    })?;

    if let Some(solution_id) = cache.get(&cache_key) {
        if let Some(stored) = solutions.get_mut(solution_id) {
            let mut response = stored.response.clone();
            let inspections = inspect_requested_hands(&mut stored.game, &request.inspect_hands)?;
            response["inspections"] =
                serde_json::to_value(inspections).map_err(|error| CliError {
                    code: "internal_error".into(),
                    message: error.to_string(),
                    field: None,
                })?;
            response["cache_hit"] = json!(true);
            response["solution_id"] = json!(solution_id);
            return Ok(response);
        }
    }

    let (response, game) = solve_request_with_game(&request)?;
    let mut response = serde_json::to_value(response).map_err(|error| CliError {
        code: "internal_error".into(),
        message: error.to_string(),
        field: None,
    })?;
    let solution_id = format!("solution-{next_solution}");
    *next_solution += 1;
    response["cache_hit"] = json!(false);
    response["solution_id"] = json!(solution_id);
    solutions.insert(
        solution_id.clone(),
        StoredSolution {
            game,
            response: response.clone(),
        },
    );
    cache.insert(cache_key, solution_id);
    Ok(response)
}

fn worker_query_node(
    envelope: &Value,
    solutions: &mut HashMap<String, StoredSolution>,
) -> Result<Value, CliError> {
    let solution_id = required_string(envelope, "solution_id")?;
    let path: Vec<String> = serde_json::from_value(
        envelope
            .get("path")
            .cloned()
            .ok_or_else(|| field_error("invalid_request", "path is required", "path"))?,
    )
    .map_err(json_error)?;
    let stored = solutions.get_mut(solution_id).ok_or_else(|| {
        field_error(
            "unknown_solution",
            &format!("solution `{solution_id}` is not available"),
            "solution_id",
        )
    })?;
    stored.game.back_to_root();
    follow_path(&mut stored.game, &path)?;
    let player = if stored.game.current_player() == 0 {
        Player::Oop
    } else {
        Player::Ip
    };
    let (actions, hands) = inspect_current_node(&mut stored.game, player)?;
    Ok(json!({
        "solution_id": solution_id,
        "path": path,
        "actor": player,
        "actions": actions,
        "hands": hands,
    }))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, CliError> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        field_error(
            "invalid_request",
            &format!("{field} must be a string"),
            field,
        )
    })
}

fn worker_error(id: Option<Value>, command: &str, error: CliError) -> Value {
    json!({
        "id": id.unwrap_or(Value::Null),
        "ok": false,
        "command": command,
        "error": error,
    })
}

fn solve_request(request: &SolveRequest) -> Result<SolveResponse<'_>, CliError> {
    solve_request_with_game(request).map(|(response, _)| response)
}

fn solve_request_with_game(
    request: &SolveRequest,
) -> Result<(SolveResponse<'_>, PostFlopGame), CliError> {
    validate_numbers(request)?;
    let (card_config, initial_state) = card_config(request)?;
    let tree_config = TreeConfig {
        initial_state,
        starting_pot: request.pot,
        effective_stack: request.effective_stack,
        rake_rate: request.rake.rate,
        rake_cap: request.rake.cap,
        flop_bet_sizes: parse_street_sizes(&request.bet_sizes.flop, "bet_sizes.flop")?,
        turn_bet_sizes: parse_street_sizes(&request.bet_sizes.turn, "bet_sizes.turn")?,
        river_bet_sizes: parse_street_sizes(&request.bet_sizes.river, "bet_sizes.river")?,
        turn_donk_sizes: None,
        river_donk_sizes: None,
        add_allin_threshold: request.thresholds.add_allin,
        force_allin_threshold: request.thresholds.force_allin,
        merging_threshold: request.thresholds.merge,
    };
    let action_tree = ActionTree::new(tree_config).map_err(|message| CliError {
        code: "invalid_tree".into(),
        message,
        field: Some("bet_sizes".into()),
    })?;
    let mut game =
        PostFlopGame::with_config(card_config, action_tree).map_err(|message| CliError {
            code: "invalid_game".into(),
            message,
            field: None,
        })?;

    let (memory, compressed_memory) = game.memory_usage();
    game.allocate_memory(request.solve.compressed);
    let lock_results = apply_nodelocks(&mut game, &request.nodelocks)?;
    game.back_to_root();
    let root_actions = action_labels(&game.available_actions());

    eprintln!(
        "solving: max {} iterations, target exploitability {}",
        request.solve.max_iterations, request.solve.target_exploitability
    );
    let started = Instant::now();
    let deadline = request.solve.deadline_ms.map(Duration::from_millis);
    let mut iterations = 0;
    let mut deadline_reached = deadline == Some(Duration::ZERO);
    let mut converged = false;

    while iterations < request.solve.max_iterations && !deadline_reached {
        solve_step(&game, iterations);
        iterations += 1;

        if iterations % 10 == 0 || iterations == request.solve.max_iterations {
            let exploitability = compute_exploitability(&game);
            eprintln!("iteration {iterations}: exploitability {exploitability:.6}");
            if exploitability <= request.solve.target_exploitability {
                converged = true;
                break;
            }
        }
        if deadline.is_some_and(|limit| started.elapsed() >= limit) {
            deadline_reached = true;
        }
    }
    finalize(&mut game);
    let exploitability = compute_exploitability(&game);
    converged |= exploitability <= request.solve.target_exploitability;
    game.cache_normalized_weights();

    let inspections = inspect_requested_hands(&mut game, &request.inspect_hands)?;
    game.back_to_root();
    let status = if deadline_reached {
        "deadline_reached"
    } else {
        "solved"
    };
    let stop_reason = if deadline_reached {
        "deadline"
    } else if converged {
        "target_exploitability"
    } else {
        "max_iterations"
    };

    let response = SolveResponse {
        ok: true,
        command: "solve",
        schema_version: SCHEMA_VERSION,
        status,
        best_so_far: !converged,
        converged,
        stop_reason,
        iterations,
        exploitability,
        target_exploitability: request.solve.target_exploitability,
        memory_bytes: if request.solve.compressed {
            compressed_memory
        } else {
            memory
        },
        compressed: request.solve.compressed,
        root_actions,
        nodelocks: lock_results,
        inspections,
    };
    Ok((response, game))
}

fn validate_numbers(request: &SolveRequest) -> Result<(), CliError> {
    if request.pot <= 0 {
        return Err(field_error("invalid_value", "pot must be positive", "pot"));
    }
    if request.effective_stack <= 0 {
        return Err(field_error(
            "invalid_value",
            "effective_stack must be positive",
            "effective_stack",
        ));
    }
    if request.solve.max_iterations == 0 {
        return Err(field_error(
            "invalid_value",
            "max_iterations must be positive",
            "solve.max_iterations",
        ));
    }
    if !request.solve.target_exploitability.is_finite() || request.solve.target_exploitability < 0.0
    {
        return Err(field_error(
            "invalid_value",
            "target_exploitability must be finite and non-negative",
            "solve.target_exploitability",
        ));
    }
    Ok(())
}

fn card_config(request: &SolveRequest) -> Result<(CardConfig, BoardState), CliError> {
    if !matches!(request.board.len(), 6 | 8 | 10) {
        return Err(field_error(
            "invalid_board",
            "board must contain three, four, or five cards",
            "board",
        ));
    }
    let mut cards = Vec::new();
    for chunk in request.board.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(chunk)
            .map_err(|_| field_error("invalid_board", "board must contain ASCII cards", "board"))?;
        let card = card_from_str(text)
            .map_err(|message| field_error("invalid_board", &message, "board"))?;
        cards.push(card);
    }
    let unique: HashSet<_> = cards.iter().copied().collect();
    if unique.len() != cards.len() {
        return Err(field_error(
            "invalid_board",
            "board contains duplicate cards",
            "board",
        ));
    }

    let oop_range = request
        .oop_range
        .parse()
        .map_err(|message: String| field_error("invalid_range", &message, "oop_range"))?;
    let ip_range = request
        .ip_range
        .parse()
        .map_err(|message: String| field_error("invalid_range", &message, "ip_range"))?;
    let initial_state = match cards.len() {
        3 => BoardState::Flop,
        4 => BoardState::Turn,
        5 => BoardState::River,
        _ => unreachable!(),
    };
    Ok((
        CardConfig {
            range: [oop_range, ip_range],
            flop: [cards[0], cards[1], cards[2]],
            turn: cards.get(3).copied().unwrap_or(NOT_DEALT),
            river: cards.get(4).copied().unwrap_or(NOT_DEALT),
        },
        initial_state,
    ))
}

fn parse_street_sizes(
    sizes: &PlayerBetSizes,
    field: &str,
) -> Result<[BetSizeOptions; 2], CliError> {
    Ok([
        parse_sizes(&sizes.oop, &format!("{field}.oop"))?,
        parse_sizes(&sizes.ip, &format!("{field}.ip"))?,
    ])
}

fn parse_sizes(sizes: &BetSizes, field: &str) -> Result<BetSizeOptions, CliError> {
    BetSizeOptions::try_from((sizes.bet.as_str(), sizes.raise.as_str()))
        .map_err(|message| field_error("invalid_bet_sizes", &message, field))
}

fn apply_nodelocks<'a>(
    game: &mut PostFlopGame,
    locks: &'a [NodeLock],
) -> Result<Vec<LockResult<'a>>, CliError> {
    let mut seen = HashSet::with_capacity(locks.len());
    for lock in locks {
        if !seen.insert((lock.path.clone(), lock.player.index())) {
            return Err(field_error(
                "duplicate_nodelock",
                "only one nodelock is allowed for each path and player",
                "nodelocks",
            ));
        }
    }

    let mut results = Vec::with_capacity(locks.len());
    for lock in locks {
        game.back_to_root();
        follow_path(game, &lock.path)?;
        require_player(game, lock.player, "nodelocks.player")?;

        let actions = game.available_actions();
        let labels = action_labels(&actions);
        let hand_names =
            holes_to_strings(game.private_cards(lock.player.index())).map_err(|message| {
                CliError {
                    code: "internal_error".into(),
                    message,
                    field: None,
                }
            })?;
        let num_hands = hand_names.len();
        let mut strategy = vec![0.0; labels.len() * num_hands];

        for (hand, frequencies) in &lock.strategy {
            let canonical_hand = canonical_hole(hand, "nodelocks.strategy")?;
            let hand_index = hand_names
                .iter()
                .position(|name| name == &canonical_hand)
                .ok_or_else(|| {
                    field_error(
                        "unknown_hand",
                        &format!("hand `{hand}` is not in the player's range at this path"),
                        "nodelocks.strategy",
                    )
                })?;
            let mut sum = 0.0_f32;
            for (label, frequency) in frequencies {
                if !frequency.is_finite() || *frequency < 0.0 || *frequency > 1.0 {
                    return Err(field_error(
                        "invalid_frequency",
                        "nodelock frequencies must be between zero and one",
                        "nodelocks.strategy",
                    ));
                }
                let action_index = labels
                    .iter()
                    .position(|candidate| candidate == label)
                    .ok_or_else(|| {
                        field_error(
                            "unknown_action",
                            &format!("action `{label}` is not available at the nodelock path"),
                            "nodelocks.strategy",
                        )
                    })?;
                strategy[action_index * num_hands + hand_index] = *frequency;
                sum += frequency;
            }
            if (sum - 1.0).abs() > 1e-5 {
                return Err(field_error(
                    "invalid_frequency",
                    &format!("nodelock frequencies for `{hand}` must sum to one"),
                    "nodelocks.strategy",
                ));
            }
        }
        game.lock_current_strategy(&strategy);
        results.push(LockResult {
            path: &lock.path,
            player: lock.player,
            applied: true,
            unlocked_hands_preserved: lock.strategy.len() < num_hands,
        });
    }
    Ok(results)
}

fn inspect_requested_hands<'a>(
    game: &mut PostFlopGame,
    inspections: &'a [Inspection],
) -> Result<Vec<InspectionResult<'a>>, CliError> {
    let mut output = Vec::with_capacity(inspections.len());
    for inspection in inspections {
        game.back_to_root();
        follow_path(game, &inspection.path)?;
        require_player(game, inspection.player, "inspect_hands.player")?;
        game.cache_normalized_weights();

        let actions = action_labels(&game.available_actions());
        let hand_names =
            holes_to_strings(game.private_cards(inspection.player.index())).map_err(|message| {
                CliError {
                    code: "internal_error".into(),
                    message,
                    field: None,
                }
            })?;
        let num_hands = hand_names.len();
        let strategy = game.strategy();
        let evs = game.expected_values(inspection.player.index());
        let normalized_weights = game.normalized_weights(inspection.player.index());
        let mut hands = Map::new();

        let wildcard = inspection.hands.iter().any(|hand| hand == "*");
        let mut requested_hands = Vec::new();
        if wildcard {
            requested_hands.extend(
                hand_names
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| normalized_weights[*index] > 0.0)
                    .map(|(index, hand)| (index, hand.clone())),
            );
        } else {
            for requested in &inspection.hands {
                let canonical_hand = canonical_hole(requested, "inspect_hands.hands")?;
                let hand_index = hand_names
                    .iter()
                    .position(|name| name == &canonical_hand)
                    .ok_or_else(|| {
                        field_error(
                            "unknown_hand",
                            &format!(
                                "hand `{requested}` is not in the player's range at this path"
                            ),
                            "inspect_hands.hands",
                        )
                    })?;
                requested_hands.push((hand_index, canonical_hand));
            }
        }

        for (hand_index, canonical_hand) in requested_hands {
            let mut action_values = Map::new();
            for (action_index, label) in actions.iter().enumerate() {
                action_values.insert(
                    label.clone(),
                    json!(strategy[action_index * num_hands + hand_index]),
                );
            }
            let mut hand_value = json!({
                "actions": action_values,
                "ev": evs[hand_index]
            });
            if wildcard {
                hand_value["normalized_weight"] = json!(normalized_weights[hand_index]);
            }
            hands.insert(canonical_hand, hand_value);
        }
        output.push(InspectionResult {
            path: &inspection.path,
            player: inspection.player,
            hands,
        });
    }
    Ok(output)
}

fn inspect_current_node(
    game: &mut PostFlopGame,
    player: Player,
) -> Result<(Vec<String>, Map<String, Value>), CliError> {
    game.cache_normalized_weights();
    let actions = action_labels(&game.available_actions());
    let hand_names =
        holes_to_strings(game.private_cards(player.index())).map_err(|message| CliError {
            code: "internal_error".into(),
            message,
            field: None,
        })?;
    let num_hands = hand_names.len();
    let strategy = game.strategy();
    let evs = game.expected_values(player.index());
    let normalized_weights = game.normalized_weights(player.index());
    let mut hands = Map::new();
    for (hand_index, hand) in hand_names.into_iter().enumerate() {
        if normalized_weights[hand_index] <= 0.0 {
            continue;
        }
        let mut action_values = Map::new();
        for (action_index, label) in actions.iter().enumerate() {
            action_values.insert(
                label.clone(),
                json!(strategy[action_index * num_hands + hand_index]),
            );
        }
        hands.insert(
            hand,
            json!({
                "actions": action_values,
                "ev": evs[hand_index],
                "normalized_weight": normalized_weights[hand_index],
            }),
        );
    }
    Ok((actions, hands))
}

fn follow_path(game: &mut PostFlopGame, path: &[String]) -> Result<(), CliError> {
    for label in path {
        if game.is_terminal_node() {
            return Err(field_error(
                "invalid_path",
                "path reaches a terminal node before it ends",
                "path",
            ));
        }
        if game.is_chance_node() {
            let card_text = label.strip_prefix("chance:").ok_or_else(|| {
                field_error(
                    "invalid_path",
                    "chance nodes require a `chance:<card>` path label",
                    "path",
                )
            })?;
            let card = card_from_str(card_text)
                .map_err(|message| field_error("invalid_path", &message, "path"))?;
            if game.possible_cards() & (1_u64 << card) == 0 {
                return Err(field_error(
                    "invalid_path",
                    &format!("card `{card_text}` cannot be dealt at this chance node"),
                    "path",
                ));
            }
            game.play(card as usize);
        } else {
            let actions = game.available_actions();
            let index = actions
                .iter()
                .position(|action| action_label(action) == *label)
                .ok_or_else(|| {
                    field_error(
                        "invalid_path",
                        &format!("action `{label}` is not available at this path"),
                        "path",
                    )
                })?;
            game.play(index);
        }
    }
    if game.is_terminal_node() || game.is_chance_node() {
        return Err(field_error(
            "invalid_path",
            "path must identify a player decision node",
            "path",
        ));
    }
    Ok(())
}

fn canonical_hole(hand: &str, field: &str) -> Result<String, CliError> {
    if hand.len() != 4 {
        return Err(field_error(
            "invalid_hand",
            &format!("hand `{hand}` must contain exactly two cards"),
            field,
        ));
    }
    let first = card_from_str(&hand[..2])
        .map_err(|message| field_error("invalid_hand", &message, field))?;
    let second = card_from_str(&hand[2..])
        .map_err(|message| field_error("invalid_hand", &message, field))?;
    if first == second {
        return Err(field_error(
            "invalid_hand",
            "a hand cannot repeat a card",
            field,
        ));
    }
    hole_to_string((first, second)).map_err(|message| field_error("invalid_hand", &message, field))
}

fn require_player(game: &PostFlopGame, player: Player, field: &str) -> Result<(), CliError> {
    if game.current_player() == player.index() {
        Ok(())
    } else {
        Err(field_error(
            "player_mismatch",
            "declared player does not act at this path",
            field,
        ))
    }
}

fn action_labels(actions: &[Action]) -> Vec<String> {
    actions.iter().map(action_label).collect()
}

fn action_label(action: &Action) -> String {
    match action {
        Action::Fold => "fold".into(),
        Action::Check => "check".into(),
        Action::Call => "call".into(),
        Action::Bet(amount) => format!("bet:{amount}"),
        Action::Raise(amount) => format!("raise:{amount}"),
        Action::AllIn(_) => "all-in".into(),
        Action::Chance(card) => card_to_string(*card).unwrap_or_else(|_| format!("chance:{card}")),
        Action::None => "none".into(),
    }
}

fn field_error(code: &str, message: &str, field: &str) -> CliError {
    CliError {
        code: code.into(),
        message: message.into(),
        field: Some(field.into()),
    }
}

fn emit<T: Serialize>(value: &T, pretty: bool) {
    let rendered = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .expect("CLI responses must be serializable");
    println!("{rendered}");
}

fn fail(command: &'static str, code: &str, message: &str, field: Option<&str>) -> ExitCode {
    fail_error(
        command,
        CliError {
            code: code.into(),
            message: message.into(),
            field: field.map(str::to_owned),
        },
    )
}

fn fail_error(command: &'static str, error: CliError) -> ExitCode {
    emit(
        &ErrorResponse {
            ok: false,
            command,
            error,
        },
        false,
    );
    ExitCode::FAILURE
}
