# Capability-surface restructure plan — doors, charters, one home per capability

**Status: v5 — agreed by all three adversarial reviewers (`glm-5.3`,
`mimo-v2.6-pro`, `kimi-k3`) after nine rounds; filed as
[ADR-084](../adr/ADR-084-capability-homes.md) (this file is the work
breakdown).**
Review history: v1 and v2 rejected unanimously; v3 approved by glm-5.3 and
kimi-k3 (mimo-v2.6-pro's three round-3 blockers → §9 B14–B16); v4 approved by
glm-5.3 and kimi-k3 (mimo-v2.6-pro's phantom-`walk` blocker plus the
nonexistent `write_fixture` item → §9 B17/B18); v5 is the text all three
approved.
Proposes **ADR-084**, filed through `ergo` (`docs/adr/README.md`).

Written against `a7ce2184a` + the uncommitted doc-consistency pass.

**Crate count is not the axis.** The axis: can a user name a capability, find
one home for it, use it without inheriting machinery they did not ask for,
complete the task in their own surface, and read the surface without wading
through a module tree.

---

## 1. The five tests

| # | Test | Failure today |
| --- | --- | --- |
| **C1 Nameable** | One capability word, umbrella alias through to type names. | `degenbot::solvers` resolves to `degenbot_pools::solve::mixed::solve_path`. |
| **C2 One home** | Code, types, door in one crate+module; a second address is an alias. | Solve has one home (ADR-073 A5). `pools` presents **33** root `pub mod`s (+25 nested = 58 declarations) and **686** `pub` items with no door. |
| **C3 Dep set = contract** | `cargo tree -p X -e normal ⊆` X's charter, per-crate feature resolution. | `degenbot-execution` (657 LOC, the user-owned `ExecutionAdapter` seam) compiles `zstd-sys` — a C build — because `pools` made the fixture machinery mandatory. |
| **C4 Door, not tree** | Public surface is a curated, intent-named set; internals `pub(crate)`. | **246** references across **67** files name a `pools::solve` path outside `degenbot-pools`; the distinct set is 33 paths (census in §2), several through modules that are implementation detail. |
| **C5 Task-complete in the consumer's own surface** | One documented task per consumer completes using catalog door items only, proven by a runnable probe. | Probes exist for (i) `cargo add degenbot` (`examples/settlement_bot`) and (iv) `ExecutionAdapter` (`examples/degenbot-execution-sample`); none for (ii) `cargo add degenbot-pools` path-solve, (iii) the Python driver's solve surface, (v) the console task `degenbot config show --resolved`. W8 delivers them. |

Consumers: (i) `cargo add degenbot` embedder · (ii) `cargo add degenbot-pools`
value/hop user · (iii) Python driver via `degenbot._ffi` · (iv) external
`ExecutionAdapter` implementor · (v) the `degenbot` console
(`degenbot-cli`/`degenbot-cli-core`, AGENTS.md's first-class invocation path).

## 2. Measurements (definition + command per row; every count below is a
`rg`/`cargo` output, and the fragile ones are replaced by a committed census
artifact in W2 rather than prose)

| Measurement | Definition + command | Value |
| --- | --- | --- |
| Members | `cargo metadata --locked --no-deps`: count / `publish != false` | **35 / 31** (non-publishable: `degenbot_rs`, 2 examples, stubgen) |
| Workspace `src` LOC | `find rust/crates rust/examples rust/tools -path '*/src/*.rs' \| xargs wc -l` | **390,334** (31 publishable: 340,636); median crate **6,025** |
| `pools` size | `find …/degenbot-pools/src -name '*.rs' \| xargs wc -l`; pre-T4 same definition via `git show fcb4c50f2^` | **49,611** (pre-T4 **24,162**, +105 %) |
| `pools` split | same, per subtree | `solve/` 20,793 · `families/` 11,162 · value+sim 17,656 |
| `pools` deps | `[dependencies]` entries in its manifest | **7 → 14** |
| `pools` surface | `rg -c '^pub mod' src/lib.rs` = **33**; `rg -c '^pub mod' src` total = **58** (so 25 nested); `rg -c '^\s*pub (fn\|struct\|enum\|trait\|mod\|const\|type\|use)' src` = **686** | as stated |
| In-degree | `cargo metadata`, normal deps | `core` 24, `config` 17, `rpc` 14, **`pools` 12**, `decoders` 12, `executor` 11, `db` 10 |
| T4 dependents | packages (excluding `degenbot-solvers` itself) with a normal **or optional** dep on it at `fcb4c50f2^` | **7** = `bot`, `execution`, `strategy`, `substrate`, facade `degenbot`, `python` (optional), `execution-sample`. **5** already had a direct `pools` edge; `execution` and `execution-sample` had none and gained one at T4. `pools` net **11 → 12** (11 − 1 + 2). |
| Rebuild blast radius | `touch F && cargo check --workspace --locked \| grep -c 'Checking degenbot'` (needs a build; reviewers may mark UNVERIFIED) | `pools/src/solve/mod.rs` → **20 crates / 26 s**; `order-index/src/lib.rs` → **6 / 2 s** |
| Leakage | `cargo tree -p X -e normal` | `zstd`+`zstd-sys` reachable only via `pools` (`solve/capture_fixture.rs`) and present in `execution`, `rpc`, `pool-updater`, `substrate`, `strategy`, `simulation`, `bot`. `num-bigint 0.5` in the same trees is **kernel charter**. `tokio`/`serde_json` also arrive via `degenbot-abi → alloy` — out of scope. |
| Role edges | dependent role dir vs dep role dir | **6 upward edges from 4 sources over 3 targets**: `substrate`→{`ingestion`,`pool-updater`}, `bot`→{`ingestion`,`pool-updater`}, `submission`→`runs`, `strategy`→`runs` |
| Umbrella | `rg -c '^pub use degenbot_' facade/src/lib.rs` | **49** aliases |
| Solve census (C4) | anchored `rg -o --no-filename -e '\b(degenbot_pools::)?solve::[A-Za-z_][A-Za-z_0-9]*(::[A-Za-z_][A-Za-z_0-9]*)?' rust/crates rust/examples -g '!…/degenbot-pools/**'`, filtered to the 7 solve modules | **246 refs / 33 distinct paths / 67 files**; per module: `mixed` 118, `affected_keys` 59, `concentrated_liquidity` 29, `profit_envelope` 21, `runtime` 15, `basket` 3, `capture_fixture` 1 |
| `pools` root refs | `rg -c 'degenbot_pools::' -t rust`, pools excluded | **998** lines (the W5 churn figure; v2's "610" was undefined) |
| Door closure (D2(a)) | public fields of the door types, enumerated at W2 | `SolveOutcome`'s fields name `WalkRefusal`, `WalkStats`, `GateStats` (`solve/mixed/solve.rs:52-67`); `PreparedConcentratedLiquidityHop` exposes `Arc<ConcentratedLiquidityCrossingTable>` / `Arc<ConcentratedLiquidityProfileTable>` (`concentrated_liquidity/entries.rs:27-35`). These are **not** separate census rows because no external file names them today — but making them `pub(crate)` would be a private-type-in-public-interface, so D2(a) closes the door over them. The census figure `SolveOutcome` = **1 external file** (`bot/src/arb_engine/path_cost_ledger.rs:189`); the other 12 files naming a `SolveOutcome` use the bot's own `arb_engine::executor::SolveOutcome`. `Envelope` = 6 refs in one file (`substrate/src/planning.rs`). |

**Conclusion (corrected twice).** The dissolution was fan-out-neutral *in
aggregate* (net +1) but **not** per-crate: two seam crates gained the whole
`pools` tree, which is exactly the leak W3 fixes. The load-bearing argument
against a solvers crate is the **object model**: the hop vocabulary is defined
by the read side and consumed by the solve side, so any boundary cuts through
every family file (the 5×2 two-homes ADR-073 removed).

## 3. Decision (proposed ADR-084, "Capability homes")

- **D1 — One capability, one home, one door.** `docs/capabilities.md` is
  authoritative: capability word → home (crate + module) → door items → the C5
  consumer and its probe → "must not pull". Every publishable crate appears at
  least once; every entry resolves to real code (gate). Rows carry the Rust path
  and, where one exists, the Python name; rows with none say "Rust-only".
- **D2 — Doors, not trees.** `pub` items are a curated surface; the trees
  beneath are `pub(crate)` unless a *named* consumer needs them. Two mechanical
  consequences: **(a)** a type that appears in a public field of a door item is
  itself a door item, and the door is **closed under that rule** — W2 computes the
  closure mechanically and the reference channel's `clippy --deny warnings`
  (`private_interfaces`) enforces it (this is why `Envelope` — 6 refs, one file —
  and `SolveOutcome`'s `WalkStats`/`GateStats`/`WalkRefusal` fields are door
  items); **(b)** `[[example]]`/`tests/` targets are
  separate crates and cannot see `pub(crate)`, so a lab needing internals either
  earns explicit door items, moves into `#[cfg(test)]` handlers, or is deleted —
  decided per lab in W2. Error enums raised at a capability boundary are door
  items of the crate that raises them.
- **D3 — The dependency charter is the published contract.** Each crate declares
  the dep prefixes it may carry; a gate asserts `cargo tree -p X -e normal ⊆`
  charter at single-package feature resolution and names its recipe
  (`just check-dependency-charter`, wired into `lint-rust-check`). Seam crates
  (`execution`, `rpc`, `core`, `pools`, `math`, `uniswap`, `pathfinding`) add
  "no C-build dep" and "no pyo3".
- **D4 — Capability layers, honestly named.** `foundation` ⊂ `engine` ⊂
  `integrations`; `shells`/`facade` compose. Edge direction is enforced over a
  census allowlist that can only shrink. Transport/IO leaves the stack depends
  on live in `foundation`.
- **D5 — No workspace member is added or removed.** Role-directory moves and
  module namespacing are in scope; count is not the axis. A future split must
  pass C2–C4 *and* show the seam does not cut an object model (D6).
- **D6 — Rejected alternatives are recorded with their measurements** (§7).
- **D7 — A breaking cut ships with a migration note.** CHANGELOG entry + "was →
  is" table for every published path removed: W2's `solve::mixed::*` → the
  `solvers` root door, `profit_envelope` → `solve::gate` (the door *module*; the
  `profit_envelope::gate` submodule's items are **hoisted to the door root**, and
  `derive`/`kernel`/`capture` stay internal — so there is no `solve::gate::gate`
  collision), `concentrated_liquidity` →
  `solve::concentrated_liquidity` (closure items promoted), `affected_keys` →
  `affected`, `capture_fixture` → `fixtures`, `mixed::solve::SolveOutcome` →
  `solvers::SolveOutcome`; W5's root-module moves. `0.6.0-alpha.*` permits hard
  cutover, not silence.

## 4. Work items

Sizes in engineer-days. Each lands green, behavior-identical; no move and no
logic change in the same commit.

### W1 — Capability catalog + vocabulary (1 d)
`docs/capabilities.md`: one row per capability (word · Rust home · Python name or
"Rust-only" · door items · C5 consumer + probe · must-not-pull). Seed from 49
umbrella aliases + 31 crates; the row set is reviewed, not generated. GLOSSARY
gains *capability home*, *capability door* (general; the existing `Family write
door` entry at `GLOSSARY.md:517` is ADR-078's specific concept and is cited, not
reused), *dependency charter*.
**Acceptance**: `architecture_gates.rs::capability_catalog_covers_every_public_crate`
— crate coverage; every path resolves; umbrella aliases ⊆ catalog; every row's
C5 cell names a consumer **and its probe** (naming a *planned* W8 probe is
valid before W8 lands) or states, as a finding to justify, that the capability
has no consumer task. `cli`/`cli-core` rows carry consumer (v) with the
`config show --resolved` task. Gate registered in `docs/gates.md` as ratchet
**#6**, whose header count becomes **six** at W1 landing (seven once W3 lands
ratchet #7), and whose table gains the `just check-capability-catalog` command.

### W2 — Solve doors from the census (3 d)
The table below is the **census seed**; the door is the seed **closed under
D2(a)** (every type reachable through a door item's public fields). W2 computes
that closure mechanically into `solve_door_census.txt` and enforces it with
`clippy --deny warnings` (`private_interfaces`), so the published table can
never leave a door type unnameable. `SolveOutcome` (**1** external file —
`bot/src/arb_engine/path_cost_ledger.rs:189`; the other 12 files naming
`SolveOutcome` use the bot's own executor type) and `simulate_solidly_path`
(1 ref) are door items; `solve_path` is **production**
(`bot/src/arb_engine/solve_cycle/mod.rs:1486`).

| Door | Items |
| --- | --- |
| `pools::solve` | `solve_path_with_min_profit`, `solve_path`, `SolvePathResult`, `SolveOutcome`, `ResolvedMixedPath`, `ResolvedHop`, `MixedPoolRef`, `PoolHop`, `HopType`, `simulate_solidly_path` |
| `pools::solve::gate` | `GateDeps`, `Envelope`, `GateStats`, `GateSkipCause`, `DegeneratePathSink`, `PrefixCache` |
| `pools::solve::concentrated_liquidity` | `WalkMemo`, `WalkMemoStats`, `WalkTelemetry`, `DENSE_OBSERVE_THRESHOLD`, `MixedHopView`, `PreparedConcentratedLiquidityHop`, `IntV3TickRangeSequence`, `IntTickRangeCrossing`, `ConcentratedLiquidityCrossingTable`, `ConcentratedLiquidityProfileTable`, `ConcentratedLiquidityWordProfile`, `WalkRefusal`, `WalkStats`, `solve_piecewise`, `solve_concentrated_liquidity_piecewise`, `derive_and_solve_concentrated_liquidity_piecewise` |
| `pools::solve::runtime` | `SolveRuntimeConfig`, `AnchorSweep` |
| `pools::affected` (moved out of `solve`) | `AffectedKey` |
| `pools::solve::basket` | `solve_balancer_weighted`, `BalancerMultiTokenState` |
| `pools::solve::fixtures` (feature-gated, W3; rename per D7) | `read_fixture` only (census: 1 ref, `bot/src/arb_engine/executor_ab_probe.rs:91`, itself `#[cfg(test)]`) — **no `write_fixture`**: the fixture *writers* are `examples/synth_corpus_gen.rs` (plain `.jsonl`) and `bot/src/arb_engine/solver_capture.rs`, and a bot-side writer cannot earn a `pools` door item. W2 neither creates nor renames a write path |

Everything else in `solve/` becomes `pub(crate)`. W2 commits the census as
`solve_door_census.txt` next to the gate, so the acceptance is a regenerable
artifact instead of a prose count.
**Acceptance**: the door-census architecture-gates entry matches the
**post-W2 door paths** — anchored on `\bdegenbot_pools::solve::` **and** on the
bare `solve::` import form (`use degenbot_pools::solve;` then `solve::…`) — and
is generated against the post-rename names — six surviving submodules plus the
root door: `gate`, `concentrated_liquidity`, `runtime`, `basket`, `affected`,
`fixtures`, with `mixed` promoted to the `solvers` root door (the `Walk*` items
live under `concentrated_liquidity`, matching `concentrated_liquidity/{memo,tally,telemetry}.rs`;
there is no `walk` module; `WalkStats`/`WalkRefusal` live in
`concentrated_liquidity/active_set.rs`, `WalkMemo`/`WalkMemoStats` in `memo.rs`,
`WalkTelemetry` in `tally.rs`) — not the pre-rename ones; every external path in `solve_door_census.txt` compiles against
a door; the door is closed under D2(a) and
`cargo clippy -p degenbot-pools --all-targets -- -D warnings` is green;
`[[example]]` labs resolved per D2; `just gen-stubs --check` green; goldens
byte-identical; D7 note for the renamed paths.
**Value (C1/C4/C5)**: `degenbot::solvers::solve_path_with_min_profit(&path, floor, &gate)`.

### W3 — `capture-fixtures` feature + charter gate (2 d)
`pools`: `zstd`/`serde_json` optional behind `capture-fixtures`;
`solve/capture_fixture.rs` and the `capture` module are `#[cfg(feature)]` in
code; the `serde_json`-using **test** modules in `solve/profit_envelope/{gate,kernel}.rs`
are gated `#[cfg(all(test, feature = "capture-fixtures"))]` (or `serde_json`
becomes an optional dev-dep) so the default `cargo test -p degenbot-pools`
compiles.
`degenbot-bot`: new feature `executor-ab-probe = ["degenbot-pools/capture-fixtures"]`;
the module gating is `#[cfg(all(test, feature = "executor-ab-probe"))] mod
executor_ab_probe;` (it is a `#[cfg(test)]` module today, `arb_engine/mod.rs:158`)
and the `#[cfg(test)]` islands of four **production** modules that consume its
corpus (`lane_walk`, `fleet_solve_executor`, `fleet_sim_executor`,
`workload_partition`) gate on the same feature.
Pools-side consumers that read fixtures also need the feature spelled out: three
integration tests (`offline_heavy_replay`, `profit_envelope_tests`,
`concentrated_liquidity_cache_lab_goldens`) and eight examples
(`concentrated_liquidity_solve_replay`, `entry_ab_readout`, `mixed_solve_replay`,
`walkmemo_readout`, `rayon_scale_probe`,
`v2_concentrated_liquidity_parity_probe`, `concentrated_liquidity_cache_lab`,
`examples/common`) — via `[[test]]`/`[[example]] required-features` **and**
recipe-side feature enabling, so a scoped `cargo test -p degenbot-pools` compiles
them instead of dropping them.
Test wiring names **both** Rust lanes plus the two corpus-running pre-push
recipes: the nextest recipes, the cargo lane (`just test-rust`, which CI runs),
`just test-golden-replay` (runs a replay example directly) and
`just test-fleet-parity` (its `the_outcome_corpus` filter matches zero tests — and
exits green — when the feature is off). No pytest change (no Python consumer of
the Rust corpora).
**Acceptance (C3)**: `cargo tree -p X -e normal` free of `zstd`/`zstd-sys` for
`execution`, `rpc`, `pool-updater`, `substrate`, `strategy`, `simulation`,
`bot` (default features); `cargo test -p degenbot-pools` (default features)
compiles; golden corpora still **executed** in every lane above — asserted on the
run/`test count`, not on a green exit, so a filtered-to-zero recipe fails;
charter gate
`architecture_gates.rs::dependency_charter_is_respected` over
`dependency_charter.txt` (`num-bigint` = `pools` kernel charter); ratchet **#7**
with `just check-dependency-charter`.
Residual, stated in the doc: `-p degenbot-bot --features executor-ab-probe` and
the A/B probe lanes still build zstd — intended, opt-in.

### W4 — Capability layers (1 d)
`git mv` `degenbot-ingestion`, `degenbot-runs`, `degenbot-pool-updater` into
`crates/foundation/` (deps verified foundation-resident: pool-updater →
db/decoders/pools/core/rpc; ingestion → rpc/decoders/core; runs → config), update
the role gate's `EXPECTED_NAMES` and `rust/README.md`'s crate tables. Anything
not moved carries an allowlist row with a rationale, and the acceptance reads
"zero upward edges outside the allowlist".
**Acceptance**: role test green; `just test` green; README matches the tree.
**Value**: contributor/architecture hygiene (relabelled — not user value); the
precondition for the catalog's "home" column being trustworthy.

### W5 — `pools` namespacing, staged (0.5 d now, 2.5 d after ADR-080)
Phase 0: record the target mapping for **all 33** root modules (`state` /
`families` / `sim` / `solve`) in `docs/capabilities.md`, stating where ADR-080's
CL state module lands (`families/concentrated_liquidity/`, with `v3_state`/
`v4_state` as adapters) so the two plans do not claim the same files; take the
surface snapshot **at execution time**, not at plan acceptance.
Phase 1 (after ADR-080): execute the moves, `PoolEntry` staying at the root as a
re-export alias. 998 external `degenbot_pools::` references make this the
largest churn item; deliberately last.
**Acceptance**: root `pub mod` ≤ 4; snapshot regenerated with a review-required
diff; goldens byte-identical.
**Value (C2)**: 33 entry points become 4. Deferred because W2 + ADR-080 deliver
most of it first.

### W6 — Umbrella as the capability map (1 d)
Group the 49 aliases by capability; link the catalog from `README.md` and
`docs/getting-started.md`; assert the **aliased subset** mapping (31 publishable
crates include `cli`/`cli-core`, which have no alias — v1's bijection was
impossible).
**Acceptance**: alias → catalog row total (many-to-one allowed); catalog rows
marked "umbrella" resolve to real aliases; the D7 migration table is linked from
the getting-started page.

### W7 — Migration + coordination (0.25 d, lands with W2)
CHANGELOG entry + "was → is" table for W2/W5 path removals; a note that W2/W3/W5
touch files another lane currently holds uncommitted (AGENTS.md concurrent-work
rule), so the work rebases behind that landing.

### W8 — Consumer probes (1 d) *(new: v2's C5 had no deliverable)*
- `pools/examples/solve_path_probe.rs` — consumer (ii): build a `ResolvedMixedPath`
  from synthetic hops, call `solve::solve_path_with_min_profit` (**in-crate** path;
  the umbrella consumer writes `degenbot::solvers::…`), print the result; doubles
  as the catalog's documented snippet for consumer (ii).
- `tests/standalone_parity/test_solve_surface_probe.py` — consumer (iii): one
  solve task through `degenbot._ffi`. The name must match
  `python_files = "test_*.py"` (`pyproject.toml:207`) or pytest never collects it.
  The `solvers_basket` surface exists (`degenbot-python/src/lib.rs:125`) while the
  stub tree has no `solvers*` `.pyi` — fix or document it in the same item.
- consumer (v): `degenbot config show --resolved` (`cli-core/src/config.rs:33-40`,
  offline and read-only) — asserted by a CLI snapshot/exit-code test.
**Acceptance**: a new `just check-consumer-probes` recipe **runs** all three
(`cargo run --example solve_path_probe -p degenbot-pools`, the pytest probe, the
console probe) and is wired into `just test`; note `just check-rust-consumer`
only checks `degenbot`'s own targets and never builds `pools`' examples, so a
`check` recipe alone cannot satisfy C5. W1's gate requires every catalog row to
name its probe or justify the absence.

## 5. Sequencing

`W1 → W3 → W4 → W2 → W8 → W6 → W5(phase 0) → [ADR-080 lands] → W5(phase 1)`,
W7 with W2. W3 second because it is user-visible, zero-API-break, and 2 d;
W4 third because it is a pure `git mv` that derisks the big cut; W5 last because
ADR-080 owns the same files. (Ranking by user value alone: W2, W3, W1, W6, W8,
W5, W7, W4 — the order above trades one slot for risk.)

## 6. Gate registry (mapped to `docs/gates.md`)

| Gate | Class prevented | Home | Registry + command |
| --- | --- | --- | --- |
| `capability_catalog_covers_every_public_crate` | capability with no home / dead path / unproven C5 claim | `architecture_gates.rs` | ratchet **#6**, `just check-capability-catalog` |
| `dependency_charter_is_respected` | a seam crate silently inheriting machinery (the zstd incident) | `architecture_gates.rs` + `dependency_charter.txt` | ratchet **#7**, `just check-dependency-charter` |
| door census | a dependent naming an internal `solve::*` submodule | `architecture_gates.rs` + `solve_door_census.txt` | architecture-gates family (not a ratchet), `just check-door-census` |
| role direction | an upward edge across capability layers | extends the existing `EXPECTED_NAMES`/role check | architecture-gates family |
| consumer probes (C5) | a catalog row whose task nobody can actually run | `just check-consumer-probes` (runs the three probes) | `just test` dependency, not a ratchet |

`docs/gates.md`'s "The five ratchets" header becomes "The seven ratchets"; each
new row fills its four columns. Wiring is `just lint-rust-check` (CI + pre-push
both already call it); no new prek hook (ADR-074).

## 7. Rejected alternatives (corrected evidence)

1. **Resurrect `degenbot-solvers`.** Load-bearing reason: the object model —
   `ResolvedHop`/hop states are defined by the read side, consumed by the solve
   side, so any boundary cuts through every family file. Fan-out is *not* the
   argument: the merge was net-neutral (+1 in-degree) though two seam crates
   gained the edge, which W3 removes. *Rejected: reopens ADR-071/073 for a 26 s
   build win and the leak W3 removes anyway.*
2. **Split `pools` into `pools-state` + `pools`.** Not horizontal: the CL family
   tree owns state machinery the value modules consume — `v3_state.rs:17-19` →
   `families::concentrated_liquidity::{state_journal, state_seeding, state_walk}`,
   likewise `v4_state.rs:16-18`, `registry.rs:9`; the reverse direction is 29
   files (`crate::v3_state` 30 refs, `state_history` 21, `int_v3_hop` 18).
   ADR-080 is mid-flight moving exactly those modules. *Rejected: a boundary
   through an object model + collision with an accepted ADR.*
3. **Merge the small crates by size.** The 10 sub-2k members are charters
   (`execution` = the user-owned adapter trait; `eventhub` = transport-pure
   fan-out; `order-index` = one minimal dep, not zero). ADR-005's interface-crate
   pattern is why the graph has no cycles. *Rejected: size is not a defect;
   publish cost is answered by `just crates-io-pending`.*

## 8. Questions answered

1. **Door cut** → §4 W2 table, generated from the §2 census, with `SolveOutcome`
   and `simulate_solidly_path` included and `Envelope` justified by D2(a).
2. **Does `capture-fixtures` reach the wheel?** No — it is not in `dev-features`
   (`degenbot-python/Cargo.toml:113-121`) and bot enables it only via
   `executor-ab-probe`; residual dirty trees named in W3.
3. **Is W5 worth it now?** No — phase 0 pins the map, phase 1 waits for ADR-080.
4. **Gate home** → `architecture_gates.rs` (it already spawns `cargo tree`);
   reuse the census-file idiom, no `cargo-public-api`.
5. **ADR or architecture doc?** ADR — `docs/adr/ADR-084-capability-homes.md`
   (filed); this file is the work breakdown.

## 9. Review disposition (rounds 2–5)

| # | Round-2 blocker (sources) | Disposition |
| --- | --- | --- |
| B1 | `solve_path` "only user is a bench" false — production calls it (glm, mimo) | **Fixed** — production caller named; keep/drop rule deleted |
| B2 | Door omits `SolveOutcome` (4-segment path, 13 files) and `simulate_solidly_path`; `Envelope` unexplained (glm, mimo, kimi) | **Fixed** — both added; `Envelope` justified by D2(a) (v3 said "+ 11 refs": that was a substring count; the type has **6** refs in `substrate/src/planning.rs` — see §2) |
| B3 | "57 files" not reproducible; "20 items"/"8 mixed items" contradict the table (all three) | **Fixed** — 246 refs / 33 paths / 67 files, anchored command, and the census becomes a committed artifact |
| B4 | Ratchet pattern misses `runtime` and goes stale on door renames (glm) | **Fixed** — anchored pattern, post-W2 door paths, all seven modules |
| B5 | "6 upward edges over 4 targets" miscounts targets (glm) | **Fixed** — 4 sources over 3 targets |
| B6 | C5 is decoration: no probe deliverable; B10 claimed otherwise (mimo, kimi) | **Fixed** — W8 + W1's per-row probe requirement; B10 now reported as partially fixed in v2 |
| B7 | W3 breaks the default bot build (`mod executor_ab_probe` ungated) and skips bot corpus tests; pytest clause spurious; `simulation` dropped (kimi, mimo) | **Fixed** — bot feature + module/test gating, both Rust lanes, pytest clause removed, `simulation` restored |
| B8 | `serde_json` test modules in `gate.rs`/`kernel.rs` would break `cargo test -p degenbot-pools` (glm, mimo) | **Fixed** — cfg-gated (or optional dev-dep) |
| B9 | "6 external dependents (not 7)" miscount (mimo) | **Fixed** — 7 with definition (normal or optional; example included) |
| B10 | "58 nested" (25 nested), `profit_envelope::derive`/`cl::cache` example has no external refs, W5 "610 refs" undefined (glm, mimo) | **Fixed** — 33+25=58; C4 reworded; W5 figure defined as `rg -c 'degenbot_pools::'` = 998 |
| B11 | gates.md: header count + missing local-command column; GLOSSARY cite wrong (`:517` is "Family write door") (glm, mimo) | **Fixed** — §6 + W1 |
| B12 | W3 door table lists `basket` in both the deny-set and the door-set; `fixtures`/`affected` renames unstated (kimi, mimo) | **Fixed** — deny-set now "everything not in the table"; renames in D7 |
| B13 | Blast-radius row UNVERIFIED / apples-to-oranges (glm, mimo) | **Fixed** — marked as requiring a build; nextest comparison removed from the conclusion |
| B14 | Door table contradicts D2(a): `SolveOutcome`'s public fields name `WalkRefusal`/`WalkStats`/`GateStats`, and `PreparedConcentratedLiquidityHop` exposes crossing/profile tables — `pub(crate)` on those is a private-type-in-public-interface (mimo, round 3) | **Fixed** — the table is a seed; the door is **closed under D2(a)**, computed into `solve_door_census.txt`, enforced by `clippy --deny warnings`; the items are named in the table |
| B15 | Consumer (v) had an escape hatch and no task (mimo, round 3) | **Fixed** — escape hatch deleted; task is `degenbot config show --resolved` (`cli-core/src/config.rs:33-40`) |
| B16 | W8's probes could not run where the plan said (pytest `python_files = "test_*.py"`, `check-rust-consumer` never builds pools' examples) (mimo, round 3) | **Fixed** — probe renamed `test_solve_surface_probe.py`; new `just check-consumer-probes` runs all three and is wired into `just test` |
| B17 | W2's acceptance named a `walk` post-rename module that exists in neither the tree, the door table, nor D7 (mimo, round 4; glm/kimi flagged as a nit) | **Fixed** — the name set is the six surviving submodules + the `mixed`→root promotion, with the `Walk*` items' real files cited |
| B18 | `write_fixture` was a door item for a symbol that does not exist (all three, rounds 4–5) | **Fixed** — removed from the door; the writers are named and neither can earn a `pools` door item |
| — | Round-3 non-blocking folded in: `SolveOutcome` is 1 external file not 13; `Envelope` is 6 refs in one file; `write_fixture` was *then* classified as a D2(b) lab item — **superseded in v5, §9 B18** (the symbol does not exist and is not created); `profit_envelope`→`gate` rename in D7; gates.md count is six at W1 / seven after W3; ratchet vocabulary unified; bare-import `solve::` form matched; pools' own fixture consumers and the two corpus recipes named (glm, kimi) | **Fixed** |

## 10. Revision history

**v4 → v5.** `walk` removed from W2's post-rename name set and the real `Walk*`
files cited (B17); `write_fixture` dropped from the fixtures door (B18 — it was
never a symbol; the earlier "lab-earned item" classification is superseded); D7
now says the `profit_envelope::gate` items are *hoisted* to the `solve::gate`
door root; W8 distinguishes the in-crate `solve::…` path from the umbrella
`degenbot::solvers::…`; B2's "11 refs" marked historical against the measured 6.

**v3 → v4.** v4: counts replaced by an anchored, committed census; `SolveOutcome`/
`simulate_solidly_path` added and `Envelope` justified; W3's bot-side gating and
lane wiring specified; W8 probes added; sequencing moved W3 ahead of W4 on user
value; gates.md/GLOSSARY integration corrected. v4 also closed the door
explicitly under D2(a) with the closure items named and
`clippy -D warnings` as the enforcement; `Envelope`/`SolveOutcome` counts
corrected to their real external users (6 refs / 1 file); `write_fixture`
classified as a lab-earned item (**superseded in v5 — see above**); D7 now lists the `profit_envelope` → `gate`
rename; the CLI consumer task is concrete (`config show --resolved`); the probes
are runnable where the plan says (renamed pytest probe + `just
check-consumer-probes`); W3 names pools' own fixture consumers and the two
corpus-running recipes; gates.md's count is stated per landing (six, then seven).

## Appendix — measurement commands

```
cargo metadata --locked --no-deps --format-version 1 --manifest-path rust/Cargo.toml
find rust/crates/foundation/degenbot-pools/src -name '*.rs' | xargs wc -l
rg -c '^pub mod' rust/crates/foundation/degenbot-pools/src/lib.rs
rg -c '^pub mod' rust/crates/foundation/degenbot-pools/src
rg -c '^\s*pub (fn|struct|enum|trait|mod|const|type|use)' rust/crates/foundation/degenbot-pools/src
cargo tree -p degenbot-execution -e normal
rg -o --no-filename -e '\b(degenbot_pools::)?solve::[A-Za-z_][A-Za-z_0-9]*(::[A-Za-z_][A-Za-z_0-9]*)?' \
   rust/crates rust/examples --glob '*.rs' -g '!rust/crates/foundation/degenbot-pools/**'
rg -c 'degenbot_pools::' -t rust rust/crates rust/examples -g '!rust/crates/foundation/degenbot-pools/**'
git show fcb4c50f2^:rust/crates/engine/degenbot-execution/Cargo.toml
```
