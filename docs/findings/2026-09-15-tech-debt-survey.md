> **Tracking.** This survey is the evidence base for ergo epic **CLWT5L**
> ("Tech-debt survey cleanup: boundary contracts, loud unmatched arms, test
> determinism"), 8 sequenced tasks. Task bodies live in
> `.ergo/plan-tech-debt-survey.md` and cite these finding IDs.
>
> Two material corrections found while scoping the epic:
>
> 1. **C8/R2/R3 are narrower than stated here.** A comment-hygiene gate
>    already exists (`scripts/hooks/comment-hygiene.sh`, `just
>    lint-comment-hygiene`) and the tree it covers is effectively swept — 6
>    residual anchored citations in `src/ rust/crates tests/`, all matcher
>    false positives. The real residue is **285 tracker tokens in `run_bot.sh`
>    and `scripts/`**, which are outside the gate's scan scope. The fix is
>    scope extension, not a new cleanup.
> 2. **The repo already bans this class in policy.** `docs/comments.md` bans
>    task-system IDs, gate narration and provenance narration, and documents
>    the pre-rule baseline (237 in-code citations, zero resolvable). So the
>    debt is an *enforcement* gap, and the cleanup must extend the gate or it
>    regenerates.
>
> Note also `docs/findings/` is the house location for findings docs (cited by
> the gate's waiver header). Epic task 1 promotes this file there.

---

# Technical debt survey — degenbot

Scope: `src/degenbot/` (Python driver, 179 files), `rust/crates/**` (Rust core,
924 files), `tests/`, `scripts/`, `run_bot.sh`, `executor/`. Generated/vendored
trees (`**/artifacts/**`, `src/degenbot/**/abi.py`, `**/*.pyi`, `executor/.venv`)
were excluded from debt attribution; findings cite them only where the *shape*
is a symptom.

Ranking is **payoff**: what a single change buys you (correctness risk removed,
N call sites collapsed, a whole class of future drift made unrepresentable),
divided by how contained the change is.

---

## Code

### C1 — Three near-identical `*_on_conn` sibling pairs per write arm (HIGH)
`rust/crates/foundation/degenbot-db/src/write/{aave,gho,positions,pools,erc20}.rs`

Every writer exposes `apply_x(&self, ...)` and `apply_x_on_conn(&self, ..., conn)`.
The pairs are near-copies: 48 `_on_conn` sites in `aave.rs` alone, 151 across
the `write/` module. Each pair also re-states the same §3.4 atomicity
boilerplate in its doc comment (8 separate "the §3.4 atomicity fix" strings in
`aave.rs`).

Payoff: a one-line macro/helper (own the conn once; `&self` overloads just open
and delegate) collapses ~50 duplicated bodies and, more importantly, makes the
chunk-atomicity invariant structural instead of re-documented per arm. The
repeated comment *is* the signal the helper is missing.

### C2 — `Py*RowInput` mirror structs re-key core fields as strings (HIGH)
`rust/crates/shells/degenbot-python/src/db/discovery.rs`
`rust/crates/foundation/degenbot-db/src/discovery.rs`

`PyV2PoolRowInput` / `PyV3PoolRowInput` / `PyV4PoolRowInput` mirror the core's
`V2PoolRowInput` etc. field-for-field — except addresses are `String` instead of
`Address`, so each `to_input()` re-runs `parse_address(...).map_err(...)` three
or four times with copy-pasted error mapping. Same for `pool_hash: String` vs
the core's `B256`.

Related boundary drift: `build_path_graph` returns a **flattened dict**
(`raw["candidate_tokens"]`, `raw["v2v3_addresses"]`, `raw["v4_lookups"]`,
`raw["edges"]`) that `_prepare_graph()` un-flattens into a `_PreparedGraph`
dataclass (`src/degenbot/pathfinding/_pathfinding.py:63`), while the core
already has a typed `PathGraphData` with exactly those four fields
(`degenbot-db/src/pathfinding.rs:96`). Three representations of one shape.

Payoff: make the PyO3 structs `Address`-typed at construction (or derive them
from the core struct via one `From`), and return the typed
`PathGraphData`-shaped pyclass instead of a dict. Removes the duplicate field
lists, the hand-written address parse/err maps, and the dict-key wire contract
that `tests/test_db_public_mirror.py` + `tests/ffi/test_companion_alias_identity.py`
currently pin by name.

### C3 — Four parallel pool-family taxonomies with string projection at the edges (HIGH)
- `degenbot-pathfinding::graph::PoolKind` (V2/V3/V4) — `KNOWN_KINDS: [(&str, Self)]`
- `degenbot-pathfinding` shell `PoolKind` (pyclass) — `from_u8`, manual `to_core`
- `degenbot-cli-core::operator::PathFamily` (V2/V3/V4) — `from_kebab`/`as_str`
- `degenbot-db::rows::pool::PoolKindRow` (V2/V3/V4/**Lfj**)
- Python `PoolFamily` / `PoolVariant` / `PoolProbe` (`src/degenbot/types/pool_type.py`)

Each has its own `ALL` array and its own `parse(&str) -> Option<Self>` with a
silent `_ => None` arm. Lfj exists in the DB row type but in **no** graph/tag
vocabulary, so a new family is a four-place edit with no compile-time tripwire.

Payoff: one canonical family enum in `degenbot-core`/`degenbot-db`, plus
`TryFrom<&str>` returning `Err(UnknownFamily { raw, known })`. Kills the
string-projection tables and makes "added a family" a build error.

### C4 — String-typed pool kind crosses the Python seam (HIGH)
`src/degenbot/runner/_registration_ledger.py:102,125`,
`src/degenbot/builders/type_resolution.py:115`,
`src/degenbot/registry/deployment_records.py:87`

`pool_memo_key(step, pool_type: str)` and `classify_build_refusal(..., pool_type: str)`
take raw strings and branch on `== "V4"` / `in {"V2","V3"}`. The typed
`PoolKind` **already exists** on the FFI (`degenbot._ffi.PoolKind`) and is what
`PathStep.type` carries — but `_registration_ledger` reads `step.type` and then
compares the *string*.

The unmatched case here is the worst form: `pool_memo_key` returns `None`
("not memoizable") for an unrecognized `pool_type`, i.e. **an unknown family is
indistinguishable from a legitimate no-identity hop**. `classify_build_refusal`
likewise falls through to `"transient"` for anything it doesn't recognize.

Payoff: change the two signatures to `PoolKind`; add an explicit
`UNRECOGNIZED`/raise arm. This is the single highest-value silent-failure fix
in the codebase.

### C5 — Silent `_ => None` parsers on closed enums (MEDIUM-HIGH)
`dex_identity.rs:146`, `failure_policy.rs:80`, `cli-core/src/block.rs:60`,
`cli-core/src/operator.rs:134,176`, `graph.rs:120`, `abi_types/type_.rs:71`

Closed enums with an exhaustive `as_str()`/`ALL` still expose
`parse(&str) -> Option<Self>` where an unrecognized input collapses to `None`.
`PathFamily::parse`, `PathDirection::parse`, the block-tag `parse` and
`DexIdentity::from_kebab` all do this; `DexIdentity` is an 8-variant closed set
with an `ALL` array right below the silent arm.

Where the caller then does `.unwrap_or(default)` (or, as in C4, `None` means
"no data"), the failure is invisible. Contrast with the project's own stated
rule in `src/degenbot/dispatch/records.py`: *"an unrecognized kind/reason or
missing payload keys is Rust/Python wire drift and RAISES — the driver can no
longer silently drop a submission event inside a string `if` chain."* That rule
is not applied uniformly.

Payoff: `TryFrom<&str> for X` with `Err(UnknownVariant { raw, known: X::ALL })`
across all six; one audit pass.

### C6 — Hand-rolled ABI encoding in the simulation harness (MEDIUM)
`rust/crates/engine/degenbot-simulation/src/harness/mod.rs:736-810`

`init_pair`, `mint_to`, `sync_selector`, `approve_data`, `balance_of_data`,
`pm_balance_of_data` each do `keccak256(sig)[..4]` + `pad32(...)` by hand while
`degenbot_rpc::abi` / `degenbot_executor::composers::encode_execute_call` exist
in-workspace. The comment even records that this class of bug already bit:
*"The prior hand-rolled encoding wrote `config` at the END of the calldata … the
contract read `config = payload.len()` (a silent no-op config)"*.

Payoff: route the six helpers through `degenbot_rpc::abi`. ~40 lines deleted,
and the latent-silent-wrong-calldata class is removed.

### C7 — Dangling `docs/migration-guides/...` and numeric § anchors (MEDIUM)
**This is the "stale reference to an unknown document" class, in code.**

`§4.2` appears **151 times** across `src/`, `rust/crates/`, `tests/`
and `§3.4` **59 times**, `§3.3` 24 times. The natural home —
`docs/adr/ADR-005-polars-inspired-three-layer-architecture.md` — has **no §3.3,
§3.4 or §4.2 headings** at all. `docs/migration-guides/` no longer exists in the
tree, yet 6 code sites still name specific files in it:

| dangling reference | cites |
|---|---|
| `docs/migration-guides/dex-subclass-collapse.md` | `tests/pancakeswap/test_pools.py:5`, `tests/builders/test_from_chain.py:5`, `tests/registry/test_pool_subclass_selection.py:5` |
| `docs/migration-guides/pool-updater-chunk-atomicity.md` | `rust/crates/integrations/degenbot-pool-updater/tests/chunk_atomicity_contract.rs:4`, `.../no_duplicate_writer_contract.rs:6` |
| `docs/architecture/in_process_sim_served_slots.md` | `degenbot-simulation/src/sim/evm/divergence_probe.rs`, `degenbot-pools/src/v3_storage_slots.rs` |
| `docs/fixtures/v2_v3_v3_solver_divergence_25641093.md` | `degenbot-pools/src/lib.rs` |
| `docs/migration-guides/chain-bootstrap-tick-map.md` | `degenbot-pools/src/tick_fetch/mod.rs` |
| `docs/architecture/snapshot-store-removal-scoping.md` | `degenbot-db/tests/wal_snapshot_isolation.rs` |

Several already admit it inline ("removed in the stale-docs cleanup `71ec78b2`")
but keep the dead citation as the justification for a whole test module
(`tests/registry/test_pool_subclass_selection.py` docstring rests entirely on a
deleted guide).

Payoff: either re-anchor these to a surviving doc/ADR section or inline the
three-sentence rule each is gesturing at. Cheap; and it stops the next reader
from hunting a doc that isn't there.

### C8 — Ephemeral task/epic/sprint labels used as the *only* explanation (MEDIUM)
`run_bot.sh`, `src/degenbot/runner/build_paths.py`, `src/degenbot/_ffi/__init__.pyi`,
`rust/crates/**`, `tests/`

Labels observed: `RSP-8/9/16`, `PRG-1..5`, `LW-T9`, `MROOY7`, `XR62VX`,
`IRUMXD`, `KAHU5W`, `Z4KQXF`, `B4GX7C`, `YI5NGB`, `CVURM7`, `TTANQJ`,
`JXCAR4`, `GOQWCL`, `2UVG3E`, `ergo V6SUQO`, `PLRGIN`, `IUGFLH`, `64ZQLA`,
`I4EJ4N`, `71ec78b2`, `slice A`, `Phase C slice C4`, `phase 4c`, `RSP-8 ... ergo 23DLCY`.

Worst offenders:
- `run_bot.sh:44-60` — the *entire* rationale for a retired env key lives in
  `"the ADR-021 per-solve solver-state tripwire is GONE with MROOY7 task
  2UVG3E and the key no longer exists"`. A reader who must restore/verify that
  key cannot, because the epic tracker is external and unversioned.
- `rust/crates/engine/degenbot-bot/src/bot_core/construction_io/mod.rs:33` —
  `# Scope (slice A)`.
- `rust/crates/integrations/degenbot-aave/src/updater/verify.rs:872` —
  `"the local helper + its test were deleted in slice A"`.
- `tests/arbitrage/test_strategy_host_verbs.py:1` — module docstring is
  `Strategy-host operator verbs (Phase C slice C4).`

Payoff: replace the label with the *rule* it introduced (e.g. "the fleet
executor is the only stance; `DEGENBOT_FLEET` is rejected at config load"). The
label can stay as provenance, but never as the explanation.

### C9 — Numbered temporal-sequencing comments that should be helper structure (MEDIUM)
`rust/crates/integrations/degenbot-aave/src/updater/operations_parser/mod.rs:259-380`

A 120-line function narrated by `Step 1` … `Step 5` with a visibly
repeated pattern in steps 4b/4c/4d/4e/4f: build ops → `assigned_log_indices.extend(...)`
→ `operations.extend(...)`. Five copies of the same extend-and-advance dance
against `next_op_id`. The comment ladder is compensating for missing helpers.

Similar: `rust/crates/foundation/degenbot-pools/src/simulate_swap.rs:444-477`
(`Step 1..5`), `rust/crates/shells/degenbot-cli/src/sinks.rs:57-87`
(`Step 1/2/3`), `rust/crates/foundation/degenbot-db/src/heal.rs` (`Step 8`).

Payoff: extract `push_ops(assigned, operations, next_op_id, new_ops)` and drop
five comments.

### C10 — Driver modules that re-own logic the Rust core owns (MEDIUM)
- `src/degenbot/runner/_registration_ledger.py` correctly declares itself a
  "thin adapter … No memo, tag, or rule is defined here", yet `pool_memo_key`
  reimplements the V2/V3-vs-V4 identity split that the core ledger already
  knows (and is the site of C4).
- `src/degenbot/aave/analysis/orchestrator.py` — the *bucket sorter* stayed
  Python ("it's trivial Python, not math") after `core.py` was retired; a
  second place where result-shape ordering is decided.
- `src/degenbot/runner/config.py` builds a `RetryPolicy` while the FFI
  `RetryPolicy` already validates through `RetryPolicy::validate`.
- Declared-but-unused dependencies: `pydantic-settings` has **zero** code
  references; `tenacity` appears only in a doc comment and in
  `rust/crates/foundation/degenbot-rpc/src/provider.rs` prose; `tomlkit` is
  used only by `scripts/bump_python_deps.py` (not the package). Either use
  them or drop them from `pyproject.toml`.

### C11 — Oversized modules (LOW-MEDIUM)
| file | lines | responsibilities visible in the def list |
|---|---|---|
| `src/degenbot/runner/build_paths.py` | 1272 | permutation parsing, `RegistrationUnitOutcome`, `ConstructionContext`, `PathRegistrationPipeline` (860 lines), `BuildPathsOptions`, `build_paths` |
| `src/degenbot/bot/_bot.py` | 1212 | `Bot` facade: provider enforcement, tracker registry, ERC20 + pool build dispatch, V3/V4 tick-fetcher factories, managed-pool facade, update routing, builder resolution |
| `rust/crates/engine/degenbot-solvers/src/profit_envelope.rs` | 4821 | |
| `rust/crates/shells/degenbot-python/src/bot/mod.rs` | 3656 | |
| `rust/crates/integrations/degenbot-aave/src/updater/transaction_processor.rs` | 3411 | |

---

## Tests

### T1 — Arbitrary wall-clock timeouts standing in for sequencing (HIGH)
`tests/arbitrage/test_arbitrage_session.py:1988`

    _done, pending = await asyncio.wait({watchdog}, timeout=0.1)
    assert watchdog in pending, "a fake live pump must park the watchdog"

`0.1s` is the assertion: it proves "did not finish within 100ms on this
machine", not "is parked". Under load this is a flake source; the real property
(a parked watchdog resolves only on pump-finished) is expressible as an
injected gate the watchdog awaits.

Related: `tests/arbitrage/test_engine_registry_claims_are_core_owned.py:51,57`
use `await asyncio.sleep(0.005)` inside a *counting* fake purely to make
workers overlap — the claim-table property is being inferred from scheduling.
`tests/arbitrage/test_solvers/test_shared_state_topology.py:899,971,1022`
use `thread.join(timeout=30.0/60.0)` as the only completion evidence.

Note the project already knows the right pattern: `test_arbitrage_session.py`
elsewhere writes `await asyncio.sleep(0)  # yield stands in for one dispatch
hot-loop await` — deterministic yields. The `0.1` / `0.005` sites are the
exceptions.

### T2 — Test-global mutable state shared across tests (HIGH)
`tests/arbitrage/test_arbitrage_session.py:1457`

    _POOL_ID_LOCK = threading.Lock()
    _POOL_IDS: dict[str, int] = {}
    def _pool_id_for(address: str) -> int: ...

Module-level cache, mutated by every test in the module, never reset. Ids
depend on test execution order. Move it into a fixture (or the fake's
constructor).

The repo has a dedicated audit for this class
(`docs/test-global-state-audit.md`) — it covers **Rust** `static`/`LazyLock`
sites only. The Python side is unaudited and this is exactly the shape the
audit describes.

### T3 — Duplicated helper across test modules (MEDIUM-HIGH)
`def _rpc_env(monkeypatch)` is copy-pasted in **6** modules
(`test_diag_config.py`, `test_cockpit_session.py`, `test_cockpit_session_owner.py`,
`test_cockpit_phase.py`, `test_engine_fake_parity.py`, `test_posture_driven_boot.py`)
with two different URL spellings (`https://eth.example.com` vs
`http://localhost:8545`) — i.e. the copies have already drifted.

Also duplicated:
- `TOKEN_AMOUNT_MULTIPLIERS` in 4 modules (`tests/balancer/test_pools.py:192`,
  `test_pools_expanded.py:129`, `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py:81`,
  `tests/uniswap/v4/test_uniswap_v4_liquidity_pool.py:46`)
- `_BALANCERQUERIES_ABI` verbatim in `tests/balancer/test_balancer_v2_onchain_parity.py:96`
  and `test_balancer_stable_onchain_parity.py:98`
- `def _payload(...)` in `tests/dispatch/test_batch_executor_seam.py:74` and
  `tests/arbitrage/test_simpipe2_payload_merge.py:50`; `def _failure` in
  `tests/arbitrage/test_eth_arbitrage_helpers.py:19`; `def _build_chunk_logs()`
  in 3 modules.

`tests/helpers/` already exists and is well-organized (`bot_factory`,
`erc20_factory`, `database`, `identity_env`, `verdict_probe`, …). These are the
stragglers.

### T4 — Test bodies that need a fixture (CORRECTED: the original figures were measurement artifacts)

> **Correction, applied after the epic was scoped.** The table this section
> originally carried was wrong. It was produced by a span heuristic that
> measured from a `def test_` line to the *next* `def test_` line, which
> silently attributed any module-level code in between — data tables, helper
> classes, comment blocks — to the preceding test. Re-measured with `ast`
> (`FunctionDef.end_lineno`), the real bodies are much smaller, and only one
> of the six was genuinely oversized.

| survey said | true (ast) | site |
|---|---|---|
| 252 | **65** | `tests/aave/writer_parity/test_verify_positions_on_chain_truth.py:236` |
| 250 | **21** | `tests/balancer/test_stable_pools.py:685` `test_given_out_ankreth_for_weth` |
| 211 | **10** | `tests/builders/test_pybot_io.py:63` `test_pybot_io_satisfies_pool_io_protocol` |
| 191 | **60** | `tests/standalone_anvil/test_ipc_transport.py:138` |
| 187 | **76** | `tests/arbitrage/test_arbitrage_session.py` (largest body) |
| 169 | **167** | `tests/uniswap/v3/test_uniswap_v3_snapshot.py:177` |

Only `test_uniswap_v3_snapshot.py` needed work, and it was done: the inline
six-pool expected-state table moved to module level and the test body fell to
about 30 lines. The other two cases were already fed by per-pool fixtures
(`ankreth_weth_data` wrapping `_build_pool_data`; a parametrised `method`
probe). Three files were nevertheless refactored on the strength of the bad
figures; the change was behaviour-neutral (assertions verbatim, re-indented),
but it was churn that should not have happened.

**Takeaway for anyone reusing this survey:** measure function bodies with
`ast`, not by scanning for `def`. The same caveat applies to any line-count
claim in this document.

### T5 — Monkeypatching a production module attribute where a seam exists (MEDIUM)
- `tests/telemetry/test_shutdown.py:39-40,65-66,114-115` —
  `monkeypatch.setattr(telemetry_mod, "flush_telemetry", recorder.flush)`:
  the recorder is a fake, but it is installed *by replacing production names*,
  so the test cannot detect a rename and does not exercise the drainer
  interface. `tests/fakes/` exists (`FakeEngine`, `FakeEngineRegistry`,
  `FakePipelineContext`, `FakeFleetHostedBot`) — the telemetry seam just hasn't
  been given one.
- `tests/aerodrome/test_aerodrome_solidly_math_routing.py:117-119` —
  `monkeypatch.setattr(calc_mod, "_rs_calc_exact_in_stable_solidly", …)`
  patches the **FFI wrapper aliases** on the calc module. That is an
  implementation detail of the `degenbot.aerodrome` shim layer, and the test
  pins the alias names.

### T6 — Tests pinning an implementation detail of a dependency (MEDIUM)
- `tests/ffi/test_pyclass_module_annotation.py` — asserts every `#[pyclass]`
  carries `module = "..."`, i.e. a specific PyO3 attribute spelling. The stated
  goal ("`repr(type)` / pickle / IDE introspection must not leak `degenbot_rs`")
  is a behavior; the test enforces the source attribute. A PyO3 version that
  defaults this would still fail the test.
- `tests/ffi/stubtest_allowlist.txt` (89 lines) — enumerates **PyO3
  introspection emission gaps** ("PERMANENT RESIDUAL", `__getattr__` getattro-slot
  invisibility). Each entry is a hardcoded dependency quirk that must be
  re-triaged on a PyO3 bump. Documented as such (good) but it is exactly the
  class: the suite fails when the *dependency's* introspection changes.
- `tests/test_import_is_cheap.py` — asserts `/proc/self/task` does not grow on
  `import degenbot`. That is an implementation detail of the tokio runtime
  boot, not a contract anyone can rely on across Rust versions.

### T7 — Live/idempotent external services instead of golden captures (MEDIUM)
50 tests carry `@pytest.mark.online_rpc`, 21 `@pytest.mark.onchain_oracle`, 20
`ethereum`, 12 `base`. The offline gate exists (`tests/offline`, `--offline`
CI default) and `tests/golden/` + cassette fixtures exist
(`tests/fixtures/chain_data/`, `scripts/record_curve_tripool_cassette.py`) —
but the onchain-parity modules still reach a fork for their truth
(`tests/uniswap/{v2,v3,v4}/*_onchain_parity.py`, `tests/balancer/*_onchain_parity.py`,
`tests/curve/test_curve_onchain_parity.py`).

Payoff: for the 21 `onchain_oracle` tests specifically, record cassettes once
(`.scratch/golden-capture-feasibility.md` in this repo already scoped this) and
run the recorded form in CI; keep the live run as an opt-in refresh.

### T8 — Assertion-light tests (LOW-MEDIUM)
290 test functions end with a bare `assert result is not None`;
`tests/arbitrage/test_solvers/test_py_bot.py` alone has 7
(`:498,515,536,564,656,687,920`). None of these can fail unless the call raises
— which makes them "no exception raised" tests wearing an assert.

Good news: no true tautologies (`assert True`, `assert 1 == 1`) were found, and
`tests/ffi/test_companion_alias_identity.py` explicitly records that it
*deleted* its historical tautological rows rather than migrating them.

### T9 — Modules that would be trivially testable with DI (MEDIUM)
`src/degenbot/telemetry/` and `src/degenbot/runner/_render.py` (602 lines) take
their sinks/providers as module-level imports; T5 exists because there is no
constructor seam. `src/degenbot/runner/diag.py` reads `/proc/self/*` at module
level (`diag.py:119-124`), which is why `test_diag_config.py` needs fresh
subprocesses instead of fixtures. Passing the sink + a `proc_root: Path`
argument makes both fixture-testable.

---

## Comments

### R1 — Numeric `§N.N` anchors with no surviving target (HIGH, overlaps C7)
151 × `§4.2`, 59 × `§3.4`, 24 × `§3.3`, plus quoted
heading anchors like `§"The heal operation"` and
`§"Column-mapping auto-derivation"` (from `docs/adr/ADR-011-…`, which is the
only form that actually resolves). The bare numeric ones do not.

Two sub-cases to treat differently:
- `ADR-005 §4.2` (`degenbot-core/src/eip_1559.rs:6`) — ADR-005 has no §4.2.
- bare `§4.2 parity gate` / `§3.4 atomicity fix` (150+ sites) — names a
  requirement gate and a fix that live in no current document.

### R2 — "removed in the stale-docs cleanup `71ec78b2`" as a standing citation (MEDIUM)
`tests/registry/test_pool_subclass_selection.py:5`, `tests/pancakeswap/test_pools.py:5`,
`tests/builders/test_from_chain.py:5`, `tests/balancer/test_quantamm_basket_parity.py:6`,
`rust/crates/facade/degenbot/tests/tier3_path5000_v4_clamp.rs:15`,
`rust/crates/facade/degenbot/tests/reachability.rs:62`,
`rust/crates/integrations/degenbot-pool-updater/tests/no_duplicate_writer_contract.rs:6`

A commit hash to a *deletion* is not an explanation. Each of these comments
still carries the substantive rule — the citation is scaffolding.

### R3 — Epic/task identifiers standing in for rationale (see C8)
`run_bot.sh` is the worst offender (its whole file header is a task-tracker
transcript).

---

## Names

### N1 — 2-char and generic fn names with a clarifying doc comment (MEDIUM)
| site | name | what it actually is |
|---|---|---|
| `degenbot-workers/src/posture.rs:796` | `pub fn process()` | the process-level `PostureOwner` (doc: "The process-level owner, or a doc-default owner when nothing was installed yet") |
| `degenbot-cli-core/src/config.rs:114` | `fn get()` | `config get` — read a declared key out of the inventory |
| `degenbot-cli-core/src/config.rs:154` | `fn set()` | `config set` — confirm-then-write through the validate-before-write path |
| `degenbot-cli/src/cli.rs:49` | `fn run()` | delegate to `degenbot_cli::run_args` |
| `degenbot-cli/src/lib.rs:127` | `pub fn run()` | parse `argv`, dispatch |
| `degenbot-cli-core/src/lib.rs:108` | `pub fn run()` | run a `Command` with a `Prompter` |
| `degenbot-cli/src/signal.rs:33` | `pub fn action()` | the signal-handling `Action` for `already_cancelled` |
| `facade/degenbot/src/investigation/hop_oracle.rs:59` | `is_ok()` | hop-oracle verdict |

`process()` and `get()`/`set()` are the ones to fix first: both are public and
both need a paragraph to explain.

### N2 — Short/terse test helpers whose purpose is in the docstring (LOW-MEDIUM)
`executor/tests/conftest_shared.py:97` `def _e(v, n=32, signed=False) -> bytes`
— a word encoder; `tests/test_config_rpc.py:46` `def _probe(tmp_path, …)`;
`scripts/backrun/quarantine_report.py:194` `def _probe(url, frame_hash, …)`;
`tests/arbitrage/test_arbitrage_config.py:276` `def _probe(self, **env)`;
`scripts/soak_percentiles.py:21` `DEFAULT_URL`.

`_probe` in three unrelated files means three different things. `_e` is the
single worst name in the tree.

### N3 — `PathStep` / `_PreparedGraph` docstrings that say only the type name (LOW)
`src/degenbot/pathfinding/_pathfinding.py:58`

    @dataclass(slots=True, frozen=True)
    class PathStep:
        """PathStep class."""

and `class _PreparedGraph` documents its two fields but not the invariant
("edge list and step builder must come from the same `build_path_graph` call").
The class is a genuine boundary value (C2) and deserves a real doc.

---

## Suggested order

1. **C4 + C5** — silent unmatched cases at the pool-family boundary. One
   `TryFrom` change removes a real "unknown family is invisible" failure mode.
2. **R1 + R2 + C7** — re-anchor or inline the §-anchors and drop the
   `71ec78b2` citations. Pure comment work, removes 200+ stale references.
3. **T2 + T3 + T1** — fixture-scope `_POOL_IDS`, consolidate `_rpc_env` &
   friends, replace the three timing stand-ins. Makes the arb suite
   deterministic.
4. **C1** — the `*_on_conn` doubling; one helper, 50 bodies, 8 redundant
   §3.4 comments.
5. **C2 + N3** — one typed pathfinding graph value across Rust→Python.
6. **C3** — single pool-family enum (largest change; start by deleting
   `PathFamily` in favor of `PoolKind`).
7. **C6, T5, T6, T4, T7** — the remaining seams and fixtures.
