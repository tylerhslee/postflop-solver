# MCP server

The MCP adapter exposes `postflop-cli` to local agents over standard input/output. It uses only the Python 3 standard library and newline-delimited JSON-RPC 2.0.

## Architecture and prerequisites

Build the optimized Rust CLI first:

```sh
cargo build --release --bin postflop-cli
```

The launcher starts a small Python protocol process. That process validates MCP messages, runs the Rust CLI as a child, forwards each solve request unchanged as JSON on the child's standard input, and converts the CLI result into MCP text plus structured content. MCP framing is the only output written to standard output; solver progress is forwarded to standard error.

```text
Codex or MCP client -> Python stdio adapter -> Rust postflop-cli -> solver core
```

Run it directly with:

```sh
mcp/run_server.sh
```

## Tools

- `solver_capabilities` returns the CLI's machine-readable feature contract.
- `solve_postflop` accepts the strict request documented in `docs/json-cli.md`, including custom action trees, deadlines, selective inspection, and partial or downstream nodelocks.

The server accepts one in-flight `solve_postflop` call. It continues reading protocol traffic while that solve runs, so `ping` and `notifications/cancelled` remain responsive. Cancelling the request terminates its CLI child and returns a `solver_cancelled` tool error. A second simultaneous solve is rejected; it is not queued implicitly.

## Resource settings

All settings are optional:

- `POSTFLOP_SOLVER_CLI`: executable path; defaults to `target/release/postflop-cli` relative to this repository.
- `POSTFLOP_SOLVER_TIMEOUT_MS`: child-process timeout; default `300000`.
- `POSTFLOP_MCP_MAX_REQUEST_BYTES`: maximum JSON-RPC line size; default `4194304`.
- `POSTFLOP_SOLVER_MAX_STDOUT_BYTES`: maximum captured solver result; default `16777216`.
- `POSTFLOP_SOLVER_MAX_STDERR_BYTES`: maximum captured solver progress/error output; default `1048576`.
- `RAYON_NUM_THREADS`: optional solver worker count.

Oversized requests receive a JSON-RPC error. Oversized solver output, timeouts, cancellation, malformed output, and unavailable binaries become structured MCP tool errors without stopping the server.

## Codex Desktop on Windows with WSL

Copy `mcp/codex-config.example.toml` into `%USERPROFILE%\.codex\config.toml`, or merge its table with the existing file. The example launches `wsl.exe`, selects `Ubuntu-24.04`, changes to the repository, and executes the server. Adjust the distribution name and every `/home/tylerhyun/...` path for your machine.

The essential configuration is:

```toml
[mcp_servers.postflopSolver]
command = "wsl.exe"
args = [
  "-d",
  "Ubuntu-24.04",
  "--",
  "bash",
  "-lc",
  "cd /home/tylerhyun/_projects/PokerCoach/Apps/postflop-solver && exec mcp/run_server.sh",
]
startup_timeout_sec = 10
tool_timeout_sec = 310

[mcp_servers.postflopSolver.env]
POSTFLOP_SOLVER_CLI = "/home/tylerhyun/_projects/PokerCoach/Apps/postflop-solver/target/release/postflop-cli"
POSTFLOP_SOLVER_TIMEOUT_MS = "300000"
```

Restart or open a new Codex Desktop session after changing its configuration, then confirm that `solver_capabilities` and `solve_postflop` appear.

Codex also supports registering stdio servers with `codex mcp add`. The equivalent general form is:

```powershell
codex mcp add postflopSolver --env POSTFLOP_SOLVER_CLI=/home/you/path/postflop-solver/target/release/postflop-cli -- wsl.exe -d Ubuntu-24.04 -- bash -lc "cd /home/you/path/postflop-solver && exec mcp/run_server.sh"
```

The TOML form above is the checked and recommended Windows-to-WSL example for this repository. The local Codex executable was not runnable from this development sandbox, so verify the exact `codex mcp add --help` option spelling in your installed Codex version before using the CLI alternative.

## Direct handshake smoke test

This verifies framing without an MCP client library:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | mcp/run_server.sh
```

Expect exactly two JSON response lines: the initialize result and the tool list. Notifications do not receive responses.
