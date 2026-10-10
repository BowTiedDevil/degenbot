# NNTETO (PC7, epic GZPPS3): the updater shell takes an injected provider (ADR-068 D5)

STATUS: **implementation complete, scoped gates GREEN.** Manager ladder (`just pre-push`,
whole-workspace `just test-python`) **PENDING** — not this worker's lane.

Baseline cited: `docs/ergo-results/hqylek-result.md` **run 2 (green)**, counts table row C3
(7 `AlloyProvider::new(rpc_url, ...)` sites under `rust/crates/shells/degenbot-python/src`; 11
under the broader `AlloyProvider::new(` spelling). Landed chunks not re-opened: 937153a29,
4a03f5e04, 345a8c118, 231334a8f, ad035c48a, 5de576784, d18c49b8d, c2f35dafd.

## What changed (shapes read from the diff)

1. **New** `rust/crates/shells/degenbot-python/src/provider_seam.rs` — the shell's provider
   seam edge (registered in `lib.rs`, `#[cfg(feature = "rpc")]`; every consumer feature
   (`pool`, `aave-updater`, `bot`, `price`, `submission`, `simulation`) implies `rpc`):
   - `pub const UPDATER_MAX_RETRIES: u32 = 5` — the updater family's ONE retry budget.
   - `pub async fn live_provider(rpc_url, max_retries) -> ProviderResult<AlloyProvider>` — the
     **only** `AlloyProvider::new` call under `degenbot-python/src`.
   - `pub fn drive<F: Future>(fut: F) -> F::Output` — the **ONE** runtime-mode decision: an
     ambient tokio handle (the verify seams, entered from Python by
     `call_on_ambient_runtime`) drives with `block_in_place(|| handle.block_on(fut))`; a bare
     worker thread (the updater entries) drives `get_runtime().block_on(fut)`. Both branches are
     byte-for-byte the two landed spellings, chosen in one place.
   - `pub fn require_ambient_runtime() -> Result<(), io::Error>` — the **ONE** spelling of the
     verify seams' ambient-only policy (the message that used to be duplicated in `pool/mod.rs`
     and `aave_updater.rs`).
2. `pool/mod.rs` — `run_pool_update` (the operator's `rpc_url` adapter) now builds the live
   transport through the seam edge and lands in a new provider-injected entry
   `fn run_pool_update_injected(database_path: &Path, chain_id, to_block, chunk_size, provider:
   AlloyProvider, cancel, verify_chunk, verify_all_interval, verify_all_at_completion)` — the
   entry a `CassetteReplayTransport`-backed provider drives. Both verify pyfunctions take the
   adapter + `drive`; `no_ambient_runtime_err` deleted.
3. `aave_updater.rs` — the same transform at `run_aave_update`, `verify_touched_positions_on_chain`,
   `verify_all_positions_on_chain`, `activate_aave_market`; `no_ambient_runtime_err` deleted.
4. `rpc/contract.rs`, `rpc/async_contract.rs`, `bot/engine/verify.rs` (2 sites) — routed to the
   same adapter; their retry budgets are unchanged (`DEFAULT_MAX_RETRIES` for the contract
   wrappers; the `async_contract` caller's `max_retries` parameter; the two `bot/engine` sites
   keep the same value, `3 == crate::provider::DEFAULT_MAX_RETRIES`, now spelled by name instead
   of as a literal).
5. `src/degenbot/_ffi/pool.pyi` — **regenerated** (stub drift). Python-visible surface moved in
   **docstring text only**, two defs: `verify_v3_liquidity_map`, `verify_v4_liquidity_map` (the
   stale `[`no_ambient_runtime_err`]` reference replaced by the seam-edge one). No signature,
   parameter, or `__all__` change; 27 stub files regenerated, **1** byte-differs.

Nothing else in the tree was touched; `git status` shows only the 7 files above plus the new
`provider_seam.rs`.

## Per-site disposition table (baseline 11 sites under the shell; the seven-site shrink target)

| baseline site | disposition |
|---|---|
| `aave_updater.rs:173` `run_aave_update` | **moved** → adapter (`drive` + `UPDATER_MAX_RETRIES`) |
| `aave_updater.rs:343` `verify_touched_positions_on_chain` | **moved** → adapter + ambient guard |
| `aave_updater.rs:449` `verify_all_positions_on_chain` | **moved** → adapter + ambient guard |
| `aave_updater.rs:597` `activate_aave_market` | **moved** → adapter |
| `pool/mod.rs:154` `run_pool_update` | **moved** → adapter; new injected entry `run_pool_update_injected` |
| `pool/mod.rs:245` `verify_v3_liquidity_map` | **moved** → adapter + ambient guard |
| `pool/mod.rs:320` `verify_v4_liquidity_map` | **moved** → adapter + ambient guard |
| `rpc/contract.rs:41` `PyContract::new` | **routed** to the same adapter (retries unchanged) |
| `rpc/async_contract.rs:40` `PyAsyncContract::create` | **routed** (caller's `retries` unchanged) |
| `bot/engine/verify.rs:54` `verify_v3_pool` | **routed** (literal `3` → `DEFAULT_MAX_RETRIES`, same value) |
| `bot/engine/verify.rs:137` `verify_v4_pool` | **routed** (same) |

Why the four non-updater sites moved: the task's acceptance is a literal
`rg "AlloyProvider::new" rust/crates/shells/degenbot-python/src` = ONE adapter site, and they are
the remaining shell-side construction sites. They are not behaviour changes — retry budgets and
error messages are preserved verbatim.

**Deliberately kept**: `degenbot-cli-core`'s own thin `rpc_url` wrappers (5 construction sites +
its per-module `RPC_MAX_RETRIES: u32 = 5` constants). ADR-068 D5 sanctions exactly that shape for
the CLI shell ("thin wrappers that build the live provider from `rpc_url`"), and they landed in a
foreign lane — consolidating them is a different chunk's scope.

## Counts delta (measured now, foreground, read-only)

| # | measure | baseline | now | delta |
|---|---|---|---|---|
| D1 | `rg "AlloyProvider::new" rust/crates/shells/degenbot-python/src` | 11 lines (7 in the `(rpc_url, 5)` form) | **1 line** — `provider_seam.rs:64`, the adapter | −10 (−7 in the exact form) |
| D2 | retry-count literal `5` at a shell construction site | 7 | **1** (`UPDATER_MAX_RETRIES`) | −6 |
| D3 | runtime-mode spellings at the provider seam | 7 (`get_runtime().block_on` ×3, `Handle::try_current()` ×4) | **1** decision (`drive`) + 1 preserved policy guard (`require_ambient_runtime`) | −5 |
| D4 | `no_ambient_runtime_err` definitions / message strings | 2 | **1** (seam edge) | −1 |

Verification one-liners (verbatim):

```bash
rg -n 'AlloyProvider::new' rust/crates/shells/degenbot-python/src
# -> rust/crates/shells/degenbot-python/src/provider_seam.rs:64:    AlloyProvider::new(rpc_url, max_retries).await

rg -c 'AlloyProvider::new' rust/crates/shells/degenbot-python/src
# -> rust/crates/shells/degenbot-python/src/provider_seam.rs:1

rg -n ', 5\)' rust/crates/shells/degenbot-python/src ; echo EXIT:$?
# -> (no output) EXIT:1

rg -n 'Handle::try_current|block_in_place' rust/crates/shells/degenbot-python/src
# -> only provider_seam.rs (the decision + the policy guard) and the two
#    `try_current().is_err()` test preambles
```

Note on D1: the surviving line *is* the adapter, whose own parameter is named `rpc_url`, so
`rg 'AlloyProvider::new\(rpc_url'` also returns exactly that one line.

## Cassette replay: the seam is injectable, and the red path is demonstrated

**New seam-level pin** (in `pool/mod.rs`'s test module, offline, no node, no socket):

```
        PASS (1/2) degenbot_rs pool::tests::run_pool_update_entry_refuses_a_cassette_ledger_gap
        PASS (2/2) degenbot_rs pool::tests::run_pool_update_entry_accepts_a_cassette_backed_provider
     Summary [   0.035s] 2 tests run: 2 passed, 4753 skipped
EXIT:0
```

- GREEN (`run_pool_update_entry_accepts_a_cassette_backed_provider`): the committed corpus
  `tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json` (drift-gate checked first)
  is wrapped by `CassetteReplayTransport::new(cassette).as_alloy_provider()` and drives
  `run_pool_update_injected` — the **same** entry the operator's `rpc_url` pyfunction lands in.
  The run commits one chunk, one pool, the pinned span, and the transport's serving stats are
  `requests == served == 2` (every request served from the ledger; no dial exists).
- RED (`run_pool_update_entry_refuses_a_cassette_ledger_gap`): **one ledger key removed in
  memory** (the first `eth_getLogs` entry). The same entry returns the typed refusal
  (`RunError::Provider(_)`); **no pool rows** were written and `exchanges.last_update_block` is
  still `from - 1` (the restart cursor did not move). The corpus file is never written: its
  keccak256 digest is asserted equal before/after, so the mutation is reverted byte-for-byte by
  construction (nothing on disk ever changed).

**Existing harness probes cited and re-run** (not re-invented — they are the corpus-level red
paths this seam now feeds):

```
        PASS (1/3) degenbot-rpc cassette::tests::mutated_response_byte_fails_the_drift_gate
        PASS (2/3) degenbot-pool-updater::verification_gate_probes probe_a_mutated_verification_answer_rolls_the_chunk_back
        PASS (3/3) degenbot-pool-updater::verification_gate_probes probe_c_mutated_apply_input_turns_the_dump_golden_red
     Summary [   0.030s] 3 tests run: 3 passed, 4752 skipped
EXIT:0
```

and the full-corpus replay pins from the whole-workspace run (`just test-rust-nextest`, EXIT 0):

```
PASS degenbot-pool-updater::cassette_replay_run run_pool_update_commits_the_recorded_span_offline_and_restarts_clean
PASS degenbot-pool-updater::verification_gate_probes probe_b_dropped_log_turns_the_run_outcome_and_dump_red
PASS degenbot-pool-updater::provider_lifecycle_contract injected_provider_serves_a_multi_chunk_run_and_a_no_work_run
PASS degenbot-aave::aave_cassette_replay aave_chunk_replay_commits_the_recorded_span_offline_and_restarts_clean
PASS degenbot-aave::aave_config_cassette_replay aave_config_chunk_replay_commits_the_recorded_span_offline_and_restarts_clean
```

## Gate exits (foreground, direct — no pipes)

| # | gate | command | exit |
|---|---|---|---|
| G0 | build receipt | `just rebuild-if-stale` | **0** (first run: STALE `2212 (c61d981f8d5ef09e)` → rebuilt → fresh; re-run after the comment fix: fresh `2216 (ce9987d0918fb4ad)`) |
| G1 | Rust lint | `just lint-rust-check` | **0** (first run **101**: `clippy::doc_markdown` on an unbackticked block range in a new doc comment — fixed; re-run on the frozen tree: **0**) |
| G2 | Rust tests (FULL) | `just test-rust-nextest` | **0** — `4726 tests run: 4726 passed, 29 skipped` |
| G3 | Rust fmt (touched .rs) | `rustfmt --check --edition 2021 --config skip_children=true -- <7 files>` | **0** (rustfmt 1.9.0; two formatting diffs found and applied first) |
| G4 | Python fmt (touched .py) | `uv run --no-sync ruff format --check -- src/degenbot/_ffi/pool.pyi` | **0** (no `.py` touched; the regenerated stub was checked and is pipeline-formatted) |
| G5 | Python lint | `just lint-python-check` | **0** — `ruff check` + `ty check` both "All checks passed!" (re-run after the stub regen) |
| G6 | FFI boundary | `uv run --no-sync pytest tests/test_ffi_boundary.py -q` | **0** — 8 passed |
| G7 | stub surface | `uv run --no-sync pytest tests/ffi -q` | **0** — 52 passed (incl. the stub-drift test) |
| G8 | stub drift | `just gen-stubs --check` | **0** after regen (first run **1**: drift — regenerated, then "committed stubs match generator output") |
| G9 | golden replay | `uv run --no-sync pytest tests/golden -q` | **0** — 141 passed, 3 skipped |
| G10 | ambient-runtime policy | `uv run --no-sync pytest tests/test_ambient_runtime.py -q` | **0** — 2 passed |
| G11 | Aave verify-seam parity | `uv run --no-sync pytest tests/aave/writer_parity/test_verify_positions_on_chain_truth.py -q` | **0** — 3 passed |
| G12 | Aave precommit + rust path | `uv run --no-sync pytest tests/aave/writer_parity/test_verify_chunk_precommit.py tests/aave/writer_parity/test_rust_path_smoke.py -q` | **0** — 3 passed |
| G13 | comment hygiene | `just lint-comment-hygiene` | **0** after fix (first run **1**: a bare six-char token in a new doc comment — reworded, lexicon untouched) |
| G14 | seam pins (2 new) | `just test-rust-nextest -p degenbot_rs run_pool_update_entry` | **0** |
| G15 | cited red probes | `just test-rust-nextest mutated` | **0** |

**Updater suites that exist (named, per the brief)**: `tests/golden` (offline replay reachable from
Python), `tests/test_ambient_runtime.py`, `tests/aave/writer_parity/test_verify_positions_on_chain_truth.py`,
`tests/aave/writer_parity/test_verify_chunk_precommit.py`, `tests/aave/writer_parity/test_rust_path_smoke.py`.
There is **no** `tests/updater` directory. The Rust-side updater suites
(`cassette_replay_run`, `verification_gate_probes`, `wave2_capture_replay`, `wave3/4/5` aave
replays, `sql_golden_replay`, `chunk_atomicity_contract`, `provider_lifecycle_contract`,
`no_duplicate_writer_contract`) are all covered by G2 and passed.

## Guards: what was preserved, and the one deliberate consequence

- **Preserved**: the verify seams still refuse a missing ambient runtime with the same typed
  error and the same message (`tests/test_ambient_runtime.py` green, both Rust no-ambient pins
  green); the one-build-per-entry transport count is unchanged (`provider_lifecycle_contract`
  green); the committed cassette/ledger corpora are untouched.
- **Consequence** (reported, not hidden): for the *updater* entries, `drive`'s ambient branch
  would drive the transport build through `block_in_place` where the old inline
  `get_runtime().block_on(...)` panicked immediately. The seam's documented constraint is
  unchanged — nesting still fails hard, just in the core's own `block_on` rather than at the
  shell's — and the CLI shell keeps its typed pre-refusal. No Python driver reaches that path
  (the console drives the updater entries from a worker thread with no runtime context).

## Inventory

- `rust/crates/shells/degenbot-python/src/provider_seam.rs` (new, 106 lines)
- `rust/crates/shells/degenbot-python/src/lib.rs` (module registration)
- `rust/crates/shells/degenbot-python/src/pool/mod.rs` (adapter + injected entry + 2 pins)
- `rust/crates/shells/degenbot-python/src/aave_updater.rs`
- `rust/crates/shells/degenbot-python/src/rpc/contract.rs`, `rpc/async_contract.rs`,
  `bot/engine/verify.rs`
- `src/degenbot/_ffi/pool.pyi` (regenerated)

No commits, no `.ergo/plans.jsonl` edits, no ergo state changes.

## PENDING (manager ladder)

- `just test-python` (whole Python suite), `just pre-push`, and any ladder run the manager owns.
- The next Rust compile advances `.build-number` past 2216 (a comment-only edit moved the receipt
  twice in this run) — re-run `just rebuild-if-stale` before trusting any Python `.so` gate.

## Manager review — verdict PASS (2026-10-10)

Verified against the tree, not the story: `AlloyProvider::new` under `degenbot-python/src` = exactly
ONE site (`provider_seam.rs:64`); `UPDATER_MAX_RETRIES` = one constant; `drive` = one runtime-mode
decision; `require_ambient_runtime` = the one preserved verify-seam policy; `alloy_provider()` survives
(ADR-023 A5). Cassette-replay record accepted (green + the in-memory ledger-key RED with corpus
keccak256 unchanged before/after). The 4 "routed" sites were required by the acceptance's one-site
rule and are behaviour-preserving forwarding — disposition table accepted as honest. Manager battery
`/tmp/pc7-gates/`: pre-push 0, gen-stubs 0, comment-hygiene 0. Workspace was quiet (no foreign
sessions) for this chunk — first fully unshared run of the epic.
