# ADR-051: The degenbot console is a Rust binary — one command model, an argv passthrough for Python

**Status: accepted** (2026-09-14; settled in a grilling session. Implementation epics: `degenbot-cli` + `rust-db-robustness`, created as ergo **drafts** pending review).

## Context

The operator surface (`degenbot pool update`, `degenbot aave update`, `degenbot database|exchange|fleet|path …`) lives in a Python click tree (`src/degenbot/cli/`) that delegates execution to the Rust cores through typed PyO3 seams (`degenbot.updater`, `degenbot.database.operations`). The work was already Rust; Python owned argv declaration, prompts, error→exit-code mapping, and the UX copy. A pure-Rust consumer rebuilding the console therefore re-implemented it by hand: the settlement-bot parity example carries a hand-rolled argv parser plus mirrored cascades (`cascade_rpc_uri`, `resolve_db_path`), and the parity ledger (`docs/architecture/rust-settlement-bot-parity.md`) records rows 1/3/4 as DRIVER-POLICY — re-implemented per consumer. AGENTS.md requires the pure-Rust bot to be buildable from `cargo add degenbot`; a console that only exists in Python is the visible gap.

## Decision

**D1 — Command semantics live in a clap-free crate, `degenbot-cli-core`.** A `Command` enum (constructor-parsed), one execution entry returning typed reports, and `CliError → ExitCode` declared once (the `exit = "deny"` workspace lint stands — `run` returns codes). The typed fleet-boot-refusal → EX_CONFIG 78 mapping lifts out of `DegenbotCLI.invoke` into this crate.

**D2 — One argv façade crate, `degenbot-cli`** (clap, `[[bin]] name = "degenbot"`, publishable — joins the ADR-009 lockstep bump and the publish-dry-run set). It owns argv→Command mapping, report/prompt rendering, and the tracing-sink install (the `telemetry.rs` "the pure-Rust binary installs sinks" role finally has an owner). A dep-graph gate in the justfile (mirroring `check-no-pyo3-in-cores`) decrees the façade may depend only on clap, cli-core, degenbot-config, `indicatif`, and the sink crates — never the core implementation crates directly.

**D3 — The Python console script becomes a passthrough, not a tree.** `_cli.py` is five lines raising `SystemExit` over `_ffi.cli_main(sys.argv[1:])`; the click tree and `src/degenbot/cli/` are deleted and `click` leaves the runtime deps. The typed driver seams (`degenbot.updater`, `degenbot.database.operations` — Tier 2) stay public and unchanged: Tier 1 (argv) is a thin door in front of Tier 2, not a second definition. Q1 policy: execution returns typed results; rendering is the CLI's job; no byte-exact output parity is required between the two entry surfaces.

**D4 — Interactive policy is declared data, ported, never reinvented.** Each command carries a `PromptPlan` (none / unless-`--force` / on-condition, e.g. backup-target-exists) realized through a `Prompter` trait. Every command's prompt mirrors its current click behavior exactly; conditions were audited, not redesigned.

**D5 — The 34 exchange activate/deactivate handlers collapse to one data-driven command:** `degenbot exchange activate --chain base --name aerodrome_v2`, resolving `(chain, name)` through the `degenbot-uniswap` deployments registry (single byte-compared source) and writing via `degenbot-db` discovery functions. Retired flat names map deterministically; a coverage test pins every Python-era verb pair.

**D6 — `fleet`/`path` become a JSON-lines UDS client in Rust.** The versioned wire protocol documented in `src/degenbot/operator/operator_channel.py` is read-only contract; server-side validation (`PosturePolicyPatch::validate`) stays the authority.

**D7 — SIGINT is Rust-owned on both entry paths.** First Ctrl+C sets the cooperative cancel flag the update loops poll at chunk boundaries (chunk atomicity contract unchanged); a second Ctrl+C restores the default disposition (abort).

**D8 — Driver-domain resolvers land in `degenbot-config`** with the loader's `Source` tagging and provenance: database path (`--database` > `DEGENBOT_DB_PATH` > `~/.config/degenbot/degenbot.db`), chain id (`--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID`), node URIs (`--node-http`/`--node-ws` > `DEGENBOT_RPC_{HTTP,WS}_CHAINID_<id>`). The retired `[rpc]`/`[ws]`/`[database]`/`default_chain_id` file keys stay refused (the config-migration Option-B cutover stands); these resolvers deliberately do not re-add file vocabulary.

**D9 — Progress renders at the façade.** `indicatif` paints on a TTY; the existing throttled `op_info!` lines (Q5IKHX) remain the non-TTY surface. Cores' `ProgressSink` shape is untouched.

**D10 — Gates.** A no-Python CI job builds the binary and runs the smoke set (database inspect/backup/compact/heal, exchange activate idempotence, updater progress) against the frozen `parity.db` fixture with a seeded-divergence oracle, `boot_gate.rs`-style. `rust-settlement-bot-parity.md` rows 1/3/4 reclassify DRIVER-POLICY → REACHABLE with evidence links.

## Consequences

One argv declaration exists (clap). Python and Rust operators run the same program; on a machine holding both installs, both spell `degenbot` and behave identically. The parity example sheds its hand-rolled parser/cascades in favor of the shared resolvers. The ADR registry's numbering note: ADR-051 deliberately follows 050 despite the ADR-037 duplicate in the index.
