# JSON CLI

`postflop-cli` exposes the heads-up postflop solver as a strict, single-request JSON interface for agents and other local programs. It supports flop, turn, and river starting states; arbitrary OOP/IP ranges; custom action trees; partial and downstream nodelocks; deadlines; and compact strategy inspection.

The CLI is postflop-only. Supply your own preflop ranges or ranges selected from a precomputed library.

## Build and run

Build the optimized binary:

```sh
cargo build --release --bin postflop-cli
```

Inspect the machine-readable contract:

```sh
./target/release/postflop-cli capabilities
```

Solve a request from a file or standard input:

```sh
./target/release/postflop-cli solve --input request.json --pretty
./target/release/postflop-cli solve < request.json
```

`--pretty` changes formatting only. For repeatable resource use, set Rayon's worker count before starting the process:

```sh
RAYON_NUM_THREADS=8 ./target/release/postflop-cli solve --input request.json
```

On PowerShell, use `$env:RAYON_NUM_THREADS = "8"` before invoking the binary.

## Process contract

Each invocation accepts one request and writes exactly one JSON object to standard output. Solver progress is written to standard error, never standard output. A successful solve exits with code 0. Invalid JSON, invalid game configuration, invalid paths, and other request errors produce a structured JSON error on standard output and a nonzero exit code.

Request objects are strict: unknown fields are rejected instead of ignored. `solve` reads standard input when `--input <path>` is omitted.

## Request schema

A solve request has this shape:

```json
{
  "board": "2s3h4d6c7c",
  "oop_range": "AsAh,QsQh,JsJh",
  "ip_range": "KsKh",
  "pot": 20,
  "effective_stack": 10,
  "bet_sizes": {
    "flop": {
      "oop": { "bet": "", "raise": "" },
      "ip": { "bet": "", "raise": "" }
    },
    "turn": {
      "oop": { "bet": "", "raise": "" },
      "ip": { "bet": "", "raise": "" }
    },
    "river": {
      "oop": { "bet": "a", "raise": "" },
      "ip": { "bet": "a", "raise": "" }
    }
  },
  "thresholds": {
    "add_allin": 1.5,
    "force_allin": 0.15,
    "merge": 0.1
  },
  "solve": {
    "max_iterations": 1000,
    "target_exploitability": 0.001,
    "compressed": false,
    "deadline_ms": 2000
  },
  "rake": {
    "rate": 0.0,
    "cap": 0.0
  },
  "nodelocks": [
    {
      "path": [],
      "player": "oop",
      "strategy": {
        "JsJh": { "check": 0.8, "all-in": 0.2 }
      }
    }
  ],
  "inspect_hands": [
    { "path": [], "player": "oop", "hands": ["AsAh", "JsJh"] }
  ]
}
```

Fields:

- `board`: three, four, or five distinct cards, with no separators. Its length selects a flop, turn, or river starting state.
- `oop_range`, `ip_range`: solver range strings. Exact combos and standard range notation are supported by the underlying `Range` parser.
- `pot`, `effective_stack`: positive integer chip amounts at the initial node.
- `bet_sizes`: sizing configuration for both players on every street. Streets before the initial state may be left empty.
- `thresholds.add_allin`: add all-in when the largest configured bet is no more than this multiple of the pot.
- `thresholds.force_allin`: force all-in when the remaining stack-to-pot ratio after a call is no more than this value.
- `thresholds.merge`: merge sufficiently similar bet sizes using this threshold.
- `solve.max_iterations`: positive iteration limit.
- `solve.target_exploitability`: non-negative absolute chip EV target, not a percentage of the pot.
- `solve.compressed`: use the solver's compressed strategy storage to reduce memory.
- `solve.deadline_ms`: optional wall-clock budget. `0` returns the initialized best-so-far strategy without running an iteration.
- `rake`: optional; defaults to zero. `rate` is between 0 and 1, and `cap` is a non-negative chip amount.
- `nodelocks`: optional list of locked strategies.
- `inspect_hands`: optional list of decision nodes and exact combos to return. This keeps responses compact instead of serializing the complete tree.

## Bet-size syntax

The `bet` and `raise` strings contain comma-separated sizes. There is no fixed CLI limit on the number of configured sizes, but every additional action expands the tree and can materially increase time and memory.

- `70%`: 70 percent of the pot.
- `2.5x`: 2.5 times the previous bet; valid for raises only.
- `100c`: exactly 100 chips.
- `20c3r`: 20 chips with a three-raise cap; intended for fixed-limit configurations and valid for raises only.
- `e`: geometric sizing over the streets remaining.
- `2e`: geometric sizing over exactly two streets.
- `3e200%`: three-street geometric sizing capped at 200 percent of the pot.
- `a`: all-in.
- `""`: no configured size.

For example:

```json
{ "bet": "33%, 75%, 120c, a", "raise": "2.5x, a" }
```

The response reports resolved actions in chips, such as `bet:120` or `raise:300`.

## Canonical action paths

Paths begin at the configured root and use these labels:

- `check`, `fold`, `call`
- `bet:<chips>` and `raise:<chips>`
- `all-in`
- `chance:<card>`, such as `chance:7s`

A turn-to-river inspection might use:

```json
{
  "path": ["check", "check", "chance:7s"],
  "player": "oop",
  "hands": ["QsQh"]
}
```

Every label must match an available action at that node. Chance cards are validated against the board and both ranges. Paths must finish at a decision node for the declared player.

## Nodelocking

Nodelocks are applied after solver memory allocation and before solving. A lock identifies one decision node by canonical path and acting player. Only one lock may target a given path/player pair.

For each listed exact combo, provide frequencies for available canonical actions. Frequencies must be finite, between 0 and 1, and sum to 1 for that combo. Combos not listed in a partial lock remain free for the solver to optimize. Exact hole cards may be supplied in either order; responses use the solver's canonical spelling.

```json
{
  "path": ["check", "all-in"],
  "player": "oop",
  "strategy": {
    "QsQh": { "fold": 0.25, "call": 0.75 }
  }
}
```

The response echoes whether each lock was applied and whether unlocked hands were preserved.

## Solve response

A successful response includes:

- `status`: `solved`, or `deadline_reached` when the deadline stops the run.
- `converged`: whether final exploitability reached the requested target.
- `best_so_far`: true when the returned strategy has not reached the target.
- `stop_reason`: `target_exploitability`, `max_iterations`, or `deadline`.
- `iterations`, `exploitability`, and `target_exploitability`.
- `memory_bytes` and `compressed`.
- `root_actions` in canonical form.
- `nodelocks` application metadata.
- `inspections`, containing action frequencies and EV only for requested hands and nodes.

Reaching `max_iterations` is not an input error: the command exits successfully with `status: "solved"`, `converged: false`, `best_so_far: true`, and `stop_reason: "max_iterations"`.

## License

This fork remains licensed under the GNU Affero General Public License, version 3 or later. In particular, if a modified version is offered for users to interact with over a network, review the AGPL source-availability obligations before deployment. See the repository's `LICENSE` file for the governing terms; this note is not legal advice.
