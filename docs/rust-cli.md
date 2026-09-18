---
title: Rust CLI (the degenbot console)
category: cli
tags:
  - cli
  - rust-owned
  - database
related_files:
  - ../rust/crates/degenbot-cli/src/argv.rs
  - ../rust/crates/degenbot-cli-core/src/command.rs
  - ../rust/crates/degenbot-cli-core/src/error.rs
  - ../rust/crates/degenbot-config/src/resolvers.rs
complexity: standard
---

# Rust CLI (the degenbot console)

The `degenbot` console is a **Rust binary** (ADR-051). One argv declaration
exists — the clap v4 tree in `degenbot-cli`
(`rust/crates/degenbot-cli/src/argv.rs`) over the clap-free command model in
`degenbot-cli-core` (`rust/crates/degenbot-cli-core/src/command.rs`). The
Python console script is a five-line passthrough to the same binary
(`degenbot._ffi.cli_main`), so Python and Rust operators run the same program
with the same vocabulary. Rendering, prompting, progress, and SIGINT are the
façade's job; execution returns typed reports and one `CliError → ExitCode`
mapping (ADR-051 D1/D2/Q1).

This page is the authoritative command/flag/exit-code reference. Per-domain
background lives in the sibling pages: [database](cli/database.md),
[pool](cli/pool.md), [aave](cli/aave.md).

## Global options

Accepted before **or** after the subcommand (clap `global = true`).

| Option | Resolved value / cascade |
|---|---|
| `--database <PATH>` | SQLite database path: `--database` > `DEGENBOT_DB_PATH` > `~/.local/state/degenbot/db/degenbot.db` (XDG state home) |
| `--chain-id <CHAIN_ID>` | Session chain id: `--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID` |
| `--node-http <URI>` | HTTP RPC endpoint: `--node-http` > `DEGENBOT_RPC_HTTP_CHAINID_<id>` |
| `--node-ws <URI>` | WebSocket RPC endpoint: `--node-ws` > `DEGENBOT_RPC_WS_CHAINID_<id>` |
| `-h, --help` | Print help and exit 0. |
| `-V, --version` | Print the workspace version plus the shared build receipt and exit 0. |

The four value options are the **driver-domain resolvers** (ADR-051 D8), owned
by `degenbot-config` (`rust/crates/degenbot-config/src/resolvers.rs`): each is a
CLI-over-env cascade with a provenance tag, and the retired
`[rpc]`/`[ws]`/`[database]`/`default_chain_id` file keys are deliberately
**not** consulted (see [config-migration](config-migration.md)).

## Commands

### `degenbot database`

| Command | Flags | Behaviour |
|---|---|---|
| `database backup` | — | Write the `.db.bak` sibling; prompts for confirmation only when the backup target already exists. |
| `database reset` | `--force` | Remove and recreate the database at the current schema (prompts unless `--force`). |
| `database upgrade` | `--force` | **RETIRED** (ADR-052 D4): prints `the database upgrades itself at open; for an explicit repair, run \`degenbot database heal\`` and exits 1. The flag is accepted for argv parity only. |
| `database compact` | — | `VACUUM` the database; never prompts. |
| `database cutover` | `--dry-run`, `--force` | One-way flip of an Alembic-marker DB into Rust schema ownership (ADR-010). Refuses foreign/no-history DBs; prompts unless `--force`. |
| `database heal` | `--dry-run`, `--force` | Out-of-place dump-and-restore rebuild into Rust ownership (ADR-011); accepts a legacy Alembic-marker DB, refuses a foreign file; prompts unless `--force`. |
| `database inspect` | — | Read-only schema-state report (`legacy_alembic` / `fresh_standalone` / `rust_owned` / `unrecognized`); never writes. |

### `degenbot exchange`

| Command | Flags | Behaviour |
|---|---|---|
| `exchange activate` | `--chain <CHAIN>` `--name <NAME>` (both required) | Activate a DEX deployment: resolve `(chain, name)` through the `degenbot-uniswap` deployments registry and write the discovery row. Idempotent: an already-active pair reports `Exchange is already activated.` |
| `exchange deactivate` | `--chain <CHAIN>` `--name <NAME>` (both required) | Deactivate a DEX deployment. Idempotent: an already-inactive pair reports `Exchange is already deactivated.` |

`CHAIN` is a chain slug (`base`, `ethereum`) or a numeric chain id; `NAME` is the
DEX name slug stored in the database (`aerodrome_v2`, `uniswap_v3`,
`uniswap_v4`, …). ADR-051 D5 collapses the retired 34 click verbs onto this
one data-driven pair.

### `degenbot pool`

| Command | Flags | Behaviour |
|---|---|---|
| `pool update` | `--chunk <BLOCKS>` (default `10000`), `--to-block <BLOCK>` (default `latest:-64`), `--verify-chunk` / `--no-verify-chunk` (default: on), `--verify-all` / `--no-verify-all` (default: off), `--verify-all-interval <BLOCKS>` (default `1000000`) | Advance liquidity-pool state for every activated exchange, committing per chunk. |
| `pool verify` | `--rpc-url <URL>`, `--chain <CHAIN_ID>`, `--block <BLOCK>`, `--pool <POOL>`, `--family <v3\|v4>` (all required), `--pool-manager <ADDRESS>` (V4 only) | Compare a pool's committed liquidity map against on-chain truth at `--block`; prints GREEN/RED with named divergences. |

`BLOCK` is a block tag (`earliest`, `finalized`, `safe`, `latest`, `pending`,
`<number>`) with an optional `:offset`, e.g. `latest:-64` or `safe:128`.

### `degenbot aave`

| Command | Flags | Behaviour |
|---|---|---|
| `aave activate` | — | Activate the Aave V3 market for the session chain (Ethereum default 1 when no chain layer supplied a value). |
| `aave deactivate` | `--name <MARKET>` (default `Aave Ethereum Market`) | Deactivate a market. |
| `aave update` | `--chunk`, `--to-block`, the verify group, `--verify-all-interval`, `--one-chunk`, `--dry-run`, `--backup` / `--no-backup` | Advance Aave V3 position state for active markets. |
| `aave position show` | `<ADDRESS>` (positional), `--market <MARKET>` (default `Aave Ethereum Market`) | Print a user's collateral and debt positions. |

### `degenbot fleet` / `degenbot path` (live operator channel)

These are a JSON-lines Unix-domain-socket client (ADR-051 D6); the running
bot's `OperatorServer` is the authority. `--socket` resolves through
`--socket` > `DEGENBOT_OPERATOR_SOCKET` > `~/.config/degenbot/operator.sock`.

| Command | Flags | Behaviour |
|---|---|---|
| `fleet posture show` | `--socket <PATH>` | Echo the live cordon posture (thresholds + Nominal/Cordoned). |
| `fleet posture set` | `--socket <PATH>`, `--cordon-enter-events <COUNT>`, `--cordon-duty-percent <PERCENT>`, `--cordon-enter-window-ms <MS>`, `--cordon-duty-window-ms <MS>`, `--cordon-exit-clean-ms <MS>`, `--cordon-sim-intake-floor <COUNT|null>` | Apply a partial patch to the live cordon thresholds. |
| `path add` | `--hop <FAMILY:ADDRESS[:HASH]>` (repeatable, required), `--direction <zfo\|ozf>`, `--socket <PATH>` | Add one specific path to the live bot mid-run. |
| `path discover` | `--bound <COUNT>`, `--socket <PATH>` | Trigger a bounded on-demand discovery sweep. |

## Exit codes

Execution returns typed results and cli-core declares the one
`CliError → ExitCode` site (`rust/crates/degenbot-cli-core/src/error.rs`); the
façade returns the code (`exit = "deny"` — a library never aborts the host).

| Code | Meaning |
|---|---|
| `0` | Success — the command completed, including a `--dry-run` (writes nothing) and a cooperatively cancelled updater run (committed chunks stay durable). `--help`/`--version` also exit 0. |
| `1` | A typed command failure: a declined confirmation prompt, a schema refusal (stale/foreign/nothing-to-do), a database op failure, or the retired `database upgrade`. |
| `2` | argv/usage error: unknown or missing subcommand (clap's usage exit), or a refused typed config at boot. |
| `78` | `EX_CONFIG` (sysexits): the typed fleet boot refusal — the host cannot host the fleet configuration (FF-T1). |

## Interaction policy

Prompt policy is declared data per command (ADR-051 D4): `--force` skips the
unless-force confirmations, and `database backup` confirms only when the target
backup exists. SIGINT is Rust-owned on both entry paths (ADR-051 D7): the first
Ctrl+C sets the cooperative cancel flag the updaters poll at chunk boundaries
(committed chunks stay durable); a second Ctrl+C restores the default
disposition and aborts.

## The no-Python gate

The console is proven Python-free by a dedicated CI job and a local recipe:

```bash
just ci-no-python-cli-gate
```

`.github/workflows/cli-no-python-gate.sh` builds `degenbot-cli` (Rust only),
runs `--help`, `--version`, `database inspect` over committed Alembic-stamped
fixtures, and `exchange activate`/`deactivate` idempotence on a fresh DB copy,
then diffs the machine-checkable stdout against the checked-in oracle
`.github/workflows/cli-no-python-expected.txt`. Two `DEGENBOT_CLI_GATE_SEED`
modes prove the comparator can fail (a mutated oracle line, and a live
Rust-owned-state divergence). The CI job `cli-no-python` provisions no Python
toolchain and never invokes `uv`.
