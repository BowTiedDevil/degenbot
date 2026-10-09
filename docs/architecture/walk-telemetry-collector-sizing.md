# Walk telemetry collector sizing — the ambient counters become an owned value (epic DMRSRY)

**Status: sizing investigation, ergo `PX3Y3E` (epic `DMRSRY`).** Documentation only — no production
code ships with this doc. Every claim carries a `file:line` citation verified on the working tree at
writing time; an implementer with none of this session's context should be able to execute §5
commit-by-commit from this file alone.

---

## 0. Scope, method, and the one-paragraph problem

The CL walk machinery writes its telemetry **ambiently**: fourteen thread-locals (thirteen `Cell`s
plus one `RefCell`) and twenty process-global `AtomicU64`s inside
`rust/crates/engine/degenbot-solvers/src/cl/telemetry.rs` (503 lines), all behind the crate's
`telemetry` cargo feature (`rust/crates/engine/degenbot-solvers/Cargo.toml:43-49`, default OFF) plus
a runtime env-gate subset. Epic `DMRSRY` replaces that table with an **owned collector value**
threaded like `WalkMemo` (`Option<&WalkMemo>` at the solve entries,
`rust/crates/engine/degenbot-solvers/src/cl/entries.rs:115,218`), such that the default build
compiles to **zero ambient writes** and production reads of `WalkOutcome.stats` keep their shape.

Verification anchors used throughout (all read during this investigation):

| Anchor | Path |
|---|---|
| Ambient telemetry module | `rust/crates/engine/degenbot-solvers/src/cl/telemetry.rs` |
| Walk + stats types | `rust/crates/engine/degenbot-solvers/src/cl/active_set.rs` (`WalkStats` :577, `WalkOutcome` :615, entry :667) |
| Solve entries (memo precedent) | `rust/crates/engine/degenbot-solvers/src/cl/entries.rs`, `src/cl/memo.rs` (`Arc<WalkMemo>` :111-136) |
| Production consumer | `rust/crates/engine/degenbot-bot/src/arb_engine/lane_walk.rs` (`solve_one_path` :138) |
| Env gates | `rust/crates/foundation/degenbot-config/src/schema.rs:433-447` → `rust/crates/engine/degenbot-solvers/src/runtime.rs:25-46` |
| Gate courier | `rust/crates/engine/degenbot-solvers/src/profit_envelope.rs` `GateDeps` :1797-1810 |
| Engine construction | `rust/crates/engine/degenbot-bot/src/arb_engine/mod.rs:542`, `lifecycle.rs:14-37` |
| Example reader | `rust/crates/engine/degenbot-solvers/examples/cl_cache_lab.rs:296-332` |
| Prior art (shape only) | `rust/crates/engine/degenbot-bot/src/arb_engine/walk_telemetry.rs` (ergo `3S7HGF`, the per-epoch memo-stats drain) |

Board facts checked this session (`ergo --json list --all`): `PX3Y3E` todo (this task); the card
titled **"C2 — BotState: registry core + CL orchestration capability split"** (`RLE7BT`, epic
`O4EXZV`) is **done** — but it is the BotState C2, not a telemetry cfg-gate; **no card or repo
reference for `NOWFGF` exists** (zero hits across `ergo` and the tree). The plan in §5 treats the
"C2 cfg-gate so the namespace is known" prerequisite as **already satisfied** by the existing
`telemetry` cargo feature (Cargo.toml:43-49) — the namespace `cl::telemetry` is cfg-known today —
and treats NOWFGF as a **layout-churn risk on `lane_walk.rs`** (recent epics T3/T4/SIMPIPE2 moved
and folded arms there), handled by sequencing the degenbot-bot commit last and small (M5).

---

## 1. Every ambient writer mapped to its collector-field equivalent

### 1.1 Thread-local `Cell`s (per-solve counters)

All declared in one `thread_local!` block, `cl/telemetry.rs:307-345`, plus the census pair at
:66-72. Writers route through the `gated_cell_bump!` accessors (:411-433); readers through
`peek_walk_stats` (:372-401). Without the `telemetry` feature every writer is a no-op
(`let _ = by;`) and reads return zeros.

| TLS cell (decl) | Collector field (`WalkTally`) | Writers (site → caller file) | Readers | Reset lifecycle today |
|---|---|---|---|---|
| `WALK_PIECES_VISITED` (:309) | `tally.pieces` | `bump_pieces_visited` (:424) → `active_set.rs:846` (piece loop) | `peek` :384 → `WalkStats.pieces`; post-hoc warn `active_set.rs:1144-1166` | `reset_walk_stats_inner` :357 (per solve, `active_set.rs:675`) + `clear_walk_pieces_and_sims` :471-478 (`active_set.rs:810`, anchor-overflow restart) |
| `WALK_PATH_SIMULATIONS` (:311) | `tally.sims` | `bump_path_simulations` (:423) → `active_set.rs:224` (inside `simulate_walk_path`) | `peek` :385; `path_simulations_now` :481-489 → `active_set.rs:873,894` (anchor delta); `Mark` :250,269-276 (probe deltas) | :358, :473-476 (same two resets) |
| `WALK_WORD_STEPS` (:315) | `tally.word_steps` | `bump_word_steps` (:425) → `hop_sim.rs:102` (per word-boundary step), `word_profile.rs:131,181` (profile inversions) | `peek` :386 | :359 |
| `WALK_REFINE_SIMS` (:318) | `tally.refine_sims` | `bump_refine_sims` (:426) → `active_set.rs:492` | `peek` :387 | :360 |
| `WALK_TERNARY_SIMS` (:322) | `tally.ternary_sims` | `bump_ternary_sims` (:427) → `active_set.rs:494` | `peek` :388 | :361 |
| `WALK_GRID_SIMS` (:323) | `tally.grid_sims` | `bump_grid_sims` (:428) → `active_set.rs:496` | `peek` :389 | :362 |
| `WALK_LEFT_EDGE_SIMS` (:328) | `tally.left_edge_sims` | `bump_left_edge_sims` (:429) → `crossings.rs:105,120,130` (left-edge bisection) | `peek` :390 | :363 |
| `WALK_RIGHT_EDGE_SIMS` (:329) | `tally.right_edge_sims` | `bump_right_edge_sims` (:430) → `crossings.rs:173,222,240` | `peek` :391 | :364 |
| `WALK_ANCHOR_SIMS` (:330) | `tally.anchor_sims` | `bump_anchor_sims` (:431) → `active_set.rs:895` (**computed as a `path_simulations_now` delta**, :873-894) | `peek` :392 | :365 |
| `WALK_EVENT_SOLVER_OK` (:335) | `tally.event_solver_ok` | `bump_event_solver_ok` (:432) → `crossings.rs:175` | `peek` :393 | :366 |
| `WALK_EVENT_SOLVER_FALLBACKS` (:336) | `tally.event_solver_fallbacks` | `bump_event_solver_fallbacks` (:433) → `crossings.rs:180` | `peek` :394 | :367 |
| `WALK_MAX_DENSE_WORDS` (:344) | `tally.max_dense_words` (**decision D3, §3.3**) | `observe_max_dense_words` (:457-469) → `crossings.rs:283` | `peek` :397 → `WalkStats.max_dense_words` → `lane_walk.rs:210` (Q3-DENSE one-shot alert) | **NEVER reset — thread-lifetime monotone max (defect, §1.3)** |
| `EVENT_CENSUS: Cell<WalkEventCensus>` (:67, struct :25-64) | `tally.census` | `event_census_record_inner` :101-153 (match-armed increments; gate `cfg.walk_event_census` :86-92) | `peek` :396 | :355 (per solve) |
| `EVENT_CENSUS_PIECES: RefCell<Vec<(Vec<usize>, U256)>>` (:71) | collector `census_pieces` log (see 2.2) | `event_census_record_inner` :150-152 | `take_event_census_pieces` :494-501 → `WalkOutcome.census_pieces` (`active_set.rs:679`, `WalkOutcome` :615-625) | drained per solve by `take` (mem::take) at `active_set.rs:679`; not part of `reset_walk_stats` |

### 1.2 Process-global atomics (cumulative wall-time census)

Declared :183-296; writers via `gated_atomic_add!` (:436-454) and `Mark::commit` (:261-286); **the
only reader/resetter in the workspace is the `cl_cache_lab` example**, which `swap(0)`s each one at
`examples/cl_cache_lab.rs:296-332` and prints the split. The walker itself never resets or reads
them (except `Mark`'s internal deltas on `WALK_SIM_NS_TOTAL`).

| Atomic (decl) | Collector field (`WalkTelemetry.ns`) | Writers | Readers |
|---|---|---|---|
| `WALK_SIM_NS_TOTAL` (:183) | `ns.sim` | `add_sim_ns` (:448) → `active_set.rs:227` (per `simulate_walk_path`); `Mark` probe deltas :252,274-279 | `cl_cache_lab.rs:296` |
| `WALK_ANCHOR_NS_TOTAL` (:187) | `ns.anchor` | `add_anchor_ns` (:449) → `active_set.rs:871` | `cl_cache_lab.rs:298` |
| `WALK_PRED_NS_TOTAL` (:191) | `ns.pred` | `add_pred_ns` (:450) → `crossings.rs:50` | `cl_cache_lab.rs:300` |
| `WALK_SOLVE_NS_TOTAL` (:194) | `ns.solve` | `add_solve_ns` (:451) → `active_set.rs:677` (entry wall) | `cl_cache_lab.rs:302` |
| `WALK_CENSUS_EDGE_NS/_SIMS/_SIMNS` (:197-206) | `ns.edge_{wall,sims,simns}` | `Mark::commit(&EDGE_*)` → `active_set.rs:965,980,987,996` (4 exit arms of the left-edge section) | `cl_cache_lab.rs:304-310` |
| `WALK_CENSUS_REDGE_NS/_SIMS/_SIMNS` (:208-214) | `ns.redge_*` | `Mark::commit` → `active_set.rs:1015` | `cl_cache_lab.rs:312-318` |
| `WALK_CENSUS_DIR_NS/_SIMS/_SIMNS` (:216-221) | `ns.dir_*` | `Mark::commit` → `active_set.rs:1063` | `cl_cache_lab.rs:320-326` |
| `WALK_CENSUS_REFINE_NS/_SIMS/_SIMNS` (:224-230) | `ns.refine_*` | `Mark::commit` → `active_set.rs:923,1034,1130` (corner-refine, terminal, second-terminal) | `cl_cache_lab.rs:322-328` |
| `WALK_ANCHOR_BUILD_NS` (:291) | `ns.anchor_build` | `add_anchor_build_ns` (:452) → `active_set.rs:857-861` | `cl_cache_lab.rs:328-330` |
| `WALK_ANCHOR_COMPOSE_NS` (:293) | `ns.anchor_compose` | `add_anchor_compose_ns` (:453) → `active_set.rs:862-866` | `cl_cache_lab.rs` (same print) |
| `WALK_ANCHOR_ARGMAX_NS` (:295) | `ns.anchor_argmax` | `add_anchor_argmax_ns` (:454) → `active_set.rs:868-869` | `cl_cache_lab.rs` (same print) |
| `WALK_CENSUS_SIMNS` (:203) | — **DEAD** | none (verified: zero writers, zero readers; re-exported at `cl/mod.rs:89` only) | none |

The `Mark` section helper (:233-286) has 6 `Mark::start()` sites (`active_set.rs:921,935,1002,1022,
1052,1118`) and 9 `commit` sites (:923,965,980,987,996,1015,1034,1063,1130). Its `start` snapshots
`WALK_PATH_SIMULATIONS` + `WALK_SIM_NS_TOTAL`; its `commit` writes one (wall, sims, simns) triple.
In the collector design `Mark` carries `&mut WalkTally` instead of statics — same call count.

### 1.3 Reset-lifecycle summary and the one live defect

Two owners, two scopes, **no overlap** (this asymmetry is the heart of §3):

- **Per-solve** (TLS cells + census): the walk entry `solve_active_set_path`
  (`active_set.rs:667-687`) drains at entry (`reset_walk_stats()` :675) and snapshots at exit
  (`peek_walk_stats()` :678, `take_event_census_pieces()` :679) into `WalkOutcome`. The doc comment
  :671-674 promises "the returned outcome carries THIS path's telemetry".
- **Per-process** (atomics): accumulate for the whole process life; only the example's `swap(0)`
  sweep clears them. No production code reads them.

**Defect found (and fixed by this refactor):** `WALK_MAX_DENSE_WORDS` is absent from
`reset_walk_stats_inner` (:355-368) and has no other resetter (verified by grep over the workspace:
only decl :344, read :397, write :457-469). It is therefore a **thread-lifetime monotone max**, so
`WalkOutcome.stats.max_dense_words` — which `lane_walk.rs:205-218` treats as the *current path's*
dense observation for the Q3-DENSE one-shot alert — actually reports the **rayon worker's running
max across every solve it has ever run**. The claim at `active_set.rs:671-674` ("THIS path's
telemetry") is false for this one field. The collector's per-solve tally fixes this by construction;
§5 M2 carries the parity caveat (decision D3).

---

## 2. The collector API

### 2.1 `WalkTally` — the per-solve accumulator (successor of the TLS table)

A plain stack value created by the walk entry, threaded by `&mut` to every writer. It is the
`WalkStats` superset: the twelve counter fields of §1.1 plus the ns/section fields the `Mark`
sections and `simulate_walk_path` currently push into process atomics (`sim_ns`, `solve_ns`,
`anchor_ns`, `pred_ns`, `edge_{wall,sims,simns}`, `redge_*`, `dir_*`, `refine_*`,
`anchor_{build,compose,argmax}_ns`). Sketch:

```rust
/// Per-solve walk telemetry accumulator. Lives on the solving stack for the
/// duration of ONE `solve_active_set_path`; merged into the engine-owned
/// `WalkTelemetry` at exit. Without the `telemetry` feature every method is a
/// no-op and the value compiles away (zero ambient writes, zero atomic RMAs).
#[derive(Default)]
pub struct WalkTally { /* WalkStats fields + the ns/section fields of §1.2 */ }

impl WalkTally {
    pub fn bump_sims(&mut self, by: usize);          // was bump_path_simulations
    pub fn bump_word_steps(&mut self, by: usize);    // was bump_word_steps
    // ... one method per §1.1 row ...
    pub fn mark_commit(&mut self, section: Section); // was Mark::commit + its three statics
    pub fn observe_max_dense_words(&mut self, n: usize);
    pub fn walk_stats(&self) -> WalkStats;           // the projection WalkOutcome already carries
    pub fn merge_into(&self, tele: &WalkTelemetry);  // one locked merge per solve
}
```

Design rules (each traceable to an epic acceptance line):

- **One signature, no cfg-duplicated shapes.** The `&mut WalkTally` parameter exists in both build
  configurations; without the feature the methods are `#[cfg]`-no-ops and the struct's fields are
  dead (the optimizer erases them). This replaces today's dual-shape pattern (macro bodies
  `#[cfg(feature)]`/`#[cfg(not)]` at :413-421,437-446) with one cfg per method body — same idea,
  no statics.
- **`Option<&WalkTelemetry>` rides the entries exactly like `memo: Option<&WalkMemo>`**
  (entries.rs:115,218). `None` = offline/tests/examples that do not want the ns census; the
  per-solve `WalkStats` still flows out via `WalkOutcome` either way.
- **Zero ambient writes in the default build**: no `thread_local!`, no `pub static` counters remain
  in `cl/` after M6 (epic acceptance: "No remaining `Cell<` writes inside mobius/cl code paths").

### 2.2 `WalkTelemetry` — the owned collector (successor of the atomics + piece log)

`Arc`-constructed by the engine next to `WalkMemo` (`mod.rs:542` shape), `parking_lot::Mutex`
interior (same lock family the memo uses; `parking_lot` is already a solvers dependency,
`Cargo.toml:33-36`), one merge per solve — WalkMemo's granularity, never per-bump:

```rust
/// The engine-owned walk telemetry collector. Arc'd at engine construction;
/// passed as `Option<&WalkTelemetry>` beside `Option<&WalkMemo>`.
/// `None` = offline run; the per-solve WalkStats still reaches WalkOutcome.
pub struct WalkTelemetry { inner: parking_lot::Mutex<WalkTelemetryState> }

struct WalkTelemetryState {
    ns: WalkNsCensus,                       // the §1.2 cumulative fields (u64 each)
    census_pieces: Vec<(Vec<usize>, U256)>, // EVENT_CENSUS_PIECES successor
}

impl WalkTelemetry {
    pub fn new() -> Self;                              // all zeros; no env reads
    pub fn merge_tally(&self, tally: &WalkTally);      // one lock per solve
    pub fn drain_ns(&self) -> WalkNsCensus;            // swap-out, example readout
    pub fn take_census_pieces(&self) -> Vec<(Vec<usize>, U256)>;
}
```

- The **`WalkNsCensus` fields are process-cumulative** (merge adds, drain swaps to zero) — the
  atomics' semantics carried over; `drain_ns` is what `cl_cache_lab`'s swap-sweep becomes.
- The **`census_pieces` log is drained (take)** at the same cadence the example drives
  (`cl_cache_lab.rs` prints per-transition edge shifts); `WalkOutcome.census_pieces` keeps flowing
  per-solve for `cl_solve_replay` (`examples/cl_solve_replay.rs:160`, unchanged).
- The collector is **not** feature-gated as a type (it must exist in default builds so signatures
  are single); with `telemetry` off the M2 merge site is `#[cfg(feature = "telemetry")]`-skipped,
  so a default-build solve pays one `None` check, nothing else.

### 2.3 Threading map (who receives what)

| Layer | Signature change | Site |
|---|---|---|
| `solve_active_set_path` | creates `WalkTally`, gains `tele: Option<&WalkTelemetry>` | `active_set.rs:667-687` |
| walk internals (`solve_active_set_path_inner`, `walk_refine_window`, `refine_at_stop`, `piece_window_left_edge`, `piece_window_right_edge_evented`, `landed_beyond`, anchor block) | `+ &mut WalkTally` param | `active_set.rs:689-1170`, `crossings.rs:96-283` |
| `simulate_walk_path` / `_inner` | `+ &mut WalkTally` | `active_set.rs:223-229` |
| `simulate_v3_range_swap` (pub) | `+ &mut WalkTally` | `hop_sim.rs:57-59` (writer :102); callers `active_set.rs:136`, `path_sim.rs:92` (its `ClPathSim::simulate` gains the param too), `profit_envelope.rs:3454,3548` (tests), `examples/v2_cl_parity_probe.rs:442`, `cl/tests/mod.rs` (~12 sites) |
| `word_profile_min_input_for_output` + profile build | `+ &mut WalkTally` | `word_profile.rs:125-185`; caller `crossings.rs:14` |
| `solve_cl_piecewise` / `solve_mixed_piecewise` | `+ tele: Option<&WalkTelemetry>` beside `memo` | `entries.rs:112,218` |
| `derive_and_solve_cl_piecewise` (offline wrapper) | **UNCHANGED** — threads `None` internally | `entries.rs:94-100` (kills ~35 test/example touch points) |
| `GateDeps` | `+ pub telemetry: Option<&'a WalkTelemetry>` + accessor (mirrors `walk_memo` :1806,1858) | `profit_envelope.rs:1797-1810` |
| mixed dispatch | `gate.telemetry()` beside `gate.walk_memo()` | `mixed/solve.rs:471-486` |

### 2.4 MSRV applicability

Workspace floor: `edition = "2021"`, `rust-version = "1.97"` (`rust/Cargo.toml:24,26`). The
collector uses nothing newer than what the crate already ships: plain structs, `parking_lot::Mutex`
(memo precedent, `Cargo.toml:33-36`), `Arc`, `const fn new`. **No MSRV movement; no new
dependency.** The one MSRV-adjacent note: `thread_local! { const { ... } }` initializers (:67-72,
:309-344) disappear entirely, which only ever lowered the bar.

### 2.5 Feature coordination with the five `degenbot-config::schema` env gates

The resolver cascade (schema key ← env/TOML file, `degenbot-config` loader; capability-scoped
resolution per ADR-062 D3, pinned by
`rust/crates/foundation/degenbot-config/tests/resolver_layers.rs`) is untouched — the collector
adds no config key. The five existing keys keep their roles:

| Schema key (schema.rs) | Packed at | Runtime field | Interaction with the collector |
|---|---|---|---|
| `walk_event_census` (:435-437, env `DEGENBOT_WALK_EVENT_CENSUS`) | `lifecycle.rs:19` | `SolveRuntimeConfig.walk_event_census` (`runtime.rs:31-32`) | Stays a **runtime** gate read by `event_census_record` (`telemetry.rs:86-92`); the census tally moves from the `EVENT_CENSUS` cell to `tally.census`. No constructor flag. |
| `walk_event_solver_legacy` (:433-435, `DEGENBOT_WALK_EVENT_SOLVER`, inverted) | `lifecycle.rs:18` | `runtime.rs:28-30` | Behavior gate only; its counters (`event_solver_ok/fallbacks`) attribute which arm ran — unchanged semantics, new home `tally`. |
| `solver_walk_memo` (:443-445, `DEGENBOT_SOLVER_WALK_MEMO`) | `mod.rs:542-544` | `WalkMemo::new` flag 1 | **None.** Deliberate separation: the collector observes; the memo caches. The two `Arc`s are constructed adjacently at the engine (M5) but share no flags. |
| `solver_walk_memo_stats` (:445-447, `DEGENBOT_SOLVER_WALK_MEMO_STATS`) | `mod.rs:542-544` | `WalkMemo::new` flag 2 | None directly; the memo-stats JSONL tap (`degenbot-bot/src/arb_engine/walk_telemetry.rs`, ergo `3S7HGF`) is the **shape precedent** for this design's drain discipline (§7), not a consumer. |
| `walk_anchor_sweep` (:437-439, `DEGENBOT_WALK_ANCHOR_SWEEP`) | `lifecycle.rs:20-26` | `runtime.rs:33-34` → `anchor_sweep_mode` (`active_set.rs:380-381`, used :853) | Behavior gate; `tally.anchor_sims` must attribute identically across `Off`/`CenterOnly`/`Full` — pinned by `cl/tests/refinements.rs:577-595` (`loop17_anchor_sweep_modes_agree`), which builds `SolveRuntimeConfig` values directly and needs no env. |

Plus the crate-level compile gate: **`telemetry = []`** (`Cargo.toml:43-49`) stays exactly as-is;
the collector's zero-cost-default promise is expressed through it (cfg-no-op `WalkTally` methods,
skip-the-merge at M2's one call site), not through a new feature.

---

## 3. Thread-scope resolution: the mix is deliberate — carry the semantics, drop the mechanisms

### 3.1 The evidence (both halves are quoted in-tree)

**The thread-local half is deliberate** — `cl/telemetry.rs:301-305`:

> "Thread-local because `cargo test` runs tests (and their solves) on separate threads concurrently
> — a shared static would mix counts."

And the per-solve reset contract depends on it: `solve_active_set_path` drains **the calling
thread's** counters at entry (`active_set.rs:672-675`) — a process-global Cell table would need a
different identity model to keep "THIS path's telemetry" (the comment's own words) true.

**The process-global half is deliberate too** — `cl/telemetry.rs:181-183`:

> "Loop-17 census: total wall time spent inside `simulate_walk_path` **(process-wide atomics —
> avoids thread-local TLS budget)**."

The parenthetical is not paranoia; it references a real incident recorded in the same crate:
`profit_envelope.rs:1517-1520` — "ONE TLS block entry (loop-16 T4): **the per-timer statics
exhausted the dlopen static-TLS surplus on the Python import path** ('cannot allocate memory in
static TLS block')". The walk's ns counters went further than the envelope's collapse-into-one-TLS-
block fix: they left TLS entirely.

### 3.2 The verdict

**Deliberate — carry it over, in the collector's own terms.** The two scopes answer two different
questions and have two different reset owners (§1.3): per-solve counters (walk drains at entry)
vs. process-cumulative ns (only the example drains). The refactor therefore does **not** pick one
scope; it preserves the split inside owned values:

- **Per-solve scope → `WalkTally` on the solving stack.** Solve-locality is strictly stronger than
  thread-locality here: a rayon worker runs one solve at a time, so a stack value can never mix
  counts across solves or tests — the property telemetry.rs:302-304 bought with TLS, without TLS.
  This also deletes the "frozen thread-local" read-back hazard the entry comment names.
- **Process-cumulative scope → `WalkTelemetry.ns` under the engine-owned `Arc`.** Today these are
  atomics precisely to dodge the TLS-budget incident; an owned collector with a per-solve locked
  merge achieves the same "no TLS slots" property with plain `u64`s, and the merge cost is one
  uncontended `parking_lot` lock per solve — WalkMemo's proven granularity.
- **The quote-the-location problem is thereby dissolved, not inherited:** there is no remaining
  ambient location to quote. Every write site gains an explicit `tally` in scope (the compiler
  enforces the threading), so the "who writes what where" question becomes a type error instead of
  a grep.

### 3.3 The one semantic decision the implementer must make (D3)

`max_dense_words` (§1.3 defect): the collector's per-solve tally makes the field honestly per-solve,
which **changes observable behavior** — the Q3-DENSE one-shot alert (`lane_walk.rs:205-218`) may
fire on a later path than before (or not at all in a run whose dense path is not the max-dense one
on its worker). Recommendation: **take the fix** (per-solve max) because it matches the documented
contract (`active_set.rs:671-674`, `WalkStats` field doc :601-603, and `lane_walk.rs:24-27`'s "the
walk reports `WalkStats::max_dense_words`; this logs once per process"), and record the delta in the
M2 A/B note. The conservative alternative (a running max inside the collector) is listed for the
PM to veto — it would preserve byte-identical alert behavior but perpetuate the lie in
`WalkOutcome`.

---

## 4. Every consumer that must change

Production path first, then the offline surface. "TP" = touch points (call/signature sites).

### 4.1 `degenbot-solvers` (the crate that owns the telemetry)

1. `src/cl/telemetry.rs` — the module itself: TLS block (:66-72, :307-345), 20 statics (:183-296),
   both macros (:411-454), `Mark` (:233-286), reset/peek/take (:349-501) rewritten to `WalkTally` +
   `WalkTelemetry`. `WalkEventCensus` (:25-64) and `accumulate_event_census` (:158-171) survive
   unchanged (they already operate on returned values; used by `cl_solve_replay.rs:82,184`).
2. `src/cl/active_set.rs` — entry (:667-687), 19 writer sites (:224,227,492,494,496,675,677,678,
   679,810,846,857,862,868,871,873,894,895,1014), post-hoc warn read (:1144-1166), `simulate_walk_path`
   (:223-229), 6 `Mark::start` + 9 `Mark::commit` sites (:921-1130), ~8 internal signatures. ~25 TP.
3. `src/cl/crossings.rs` — 10 writer sites (:50,105,120,130,173,175,180,222,240,283) + ~6 signatures
   (the edge helpers already take `cfg`; the tally rides beside it). ~16 TP.
4. `src/cl/hop_sim.rs` — `simulate_v3_range_swap` pub signature (:57-59) + 1 writer (:102). 2 TP
   (+ its callers counted where they live).
5. `src/cl/word_profile.rs` — 2 writer sites (:131,181) + 2 signatures. 4 TP.
6. `src/cl/entries.rs` — 2 pub entry signatures gain `tele` (:112,218); `derive_and_solve_cl_piecewise`
   keeps its signature, threading `None` (:94-100). 3 TP.
7. `src/cl/mod.rs` — re-exports (:84-93): the 20 statics leave the public surface (breaking only to
   `cl_cache_lab`, fixed in M6); `WalkTelemetry`/`WalkTally`/`WalkNsCensus` enter;
   `DENSE_OBSERVE_THRESHOLD` (:76) unchanged. 2 TP.
8. `src/cl/path_sim.rs` — `ClPathSim::simulate` signature (calls `simulate_v3_range_swap` :92);
   2 test callers (:168-169, :293) pass `None`/tally. 3 TP.
9. `src/mixed/solve.rs` — 2 call sites pass `gate.telemetry()` (:471-486); `SolvePathResult.stats`
   (:46) unchanged — the production return shape is preserved. 2 TP.
10. `src/profit_envelope.rs` — `GateDeps` field + accessor (:1797-1810, :1858-1860); 2 test callers
    of `simulate_v3_range_swap` (:3454,3548). 3 TP.
11. Tests in-crate: `cl/tests/mod.rs` (direct TLS reads at :2741-2786, :2949-2956 become tally
    reads; ~15 direct entry calls add `None`), `cl/tests/memo.rs` (~14 entry calls), `cl/tests/refinements.rs`
    (~6), `cl/tests/refine_undersample.rs` (:482), `cl/path_sim.rs` tests (above). ~35 TP, mechanical.
12. `tests/offline_heavy_replay.rs:138`, `tests/mobius_discriminant_domain.rs:68`,
    `tests/cl_cache_lab_goldens.rs` (:61,76,165,241) — mostly `derive_and_solve_…` (unchanged) and
    2 direct entry calls. 3 TP.
13. `benches/walkmemo_contention.rs:164` — 1 call adds `None`. 1 TP.
14. Examples: `cl_cache_lab.rs` (entry calls :95,115,247 + the reader cut-over §4.3),
    `cl_solve_replay.rs:154`, `mixed_solve_replay.rs:213`, `cl_capture_gen.rs:332`,
    `walkmemo_readout.rs:141,162`, `v2_cl_parity_probe.rs:442` (direct swap call). ~9 TP.

### 4.2 `degenbot-substrate` (a second production entry consumer — easy to miss)

15. `src/resolve/mod.rs:949,1006` — the resolve-time solve calls pass `None` (they already pass
    `None` memo). 2 TP. **This crate is outside engine/ and must not be forgotten in the commit
    that changes the entry signatures (M4).**

### 4.3 `degenbot-bot` (the only real production consumer)

16. `src/arb_engine/mod.rs:542` — construct `walk_telemetry: Arc<WalkTelemetry>` beside the memo;
    `SolveCycleShared` gains the field; `lifecycle.rs` unchanged (the collector takes no env
    gates). 2 TP.
17. `src/arb_engine/lane_walk.rs:158-171` — `gate_deps` literal gains `telemetry: Some(&*ctx.walk_telemetry)`;
    the `outcome.stats` reads (:205-220) and the OTel attributes (:241-268) are **unchanged** —
    that is the epic's "production-audience shape preserved" clause. The bot-local
    `WALK_DENSE_ALERTED` AtomicBool (:29-31) is NOT walk telemetry (bot-owned latch) and stays.
18. Test scaffolds constructing the shared context — `tests/clamp_merge_worker_tests.rs:138`,
    `src/arb_engine/fleet_sim_executor.rs:528`, `src/arb_engine/executor_ab_probe.rs:133` — add
    `walk_telemetry: Arc::new(WalkTelemetry::new())` (or a disabled twin) beside their
    `WalkMemo::new(false, false)`. 3 TP.
19. `src/arb_engine/walk_telemetry.rs:362` (memo-tap test) — 1 call adds `None`. 1 TP.

### 4.4 Hotpath labels — no overlap, one redundancy

The `hotpath::measure` labels (`cl_solve.active_set` :666, `cl_solve.int_solve_cl_path`
`entries.rs:111`, `cl_solve.exact_solve_mixed_path_n` :209, `cl_solve.int_simulate_v3_swap`
`hop_sim.rs:54`, `cl_solve.build_crossing_table` :253 / `build_word_profiles` :277 / `cl_walk_hop`
:324 in `crossings.rs`, `mixed.solve_path_inner` `mixed/solve.rs:183`) are a separate observability
plane (ADR-043) and **do not read the ambient counters** — no label changes. The one overlap is
*redundant measurement*, not coupling: `WALK_SOLVE_NS_TOTAL` (:677) re-times the same region
`cl_solve.active_set` spans. Keep it (the example prints it; parity), and note it as a candidate
deletion once the example drains the collector.

---

## 5. Stepwise migration plan

Gates for every step: `cargo test -p degenbot-solvers` in **both** feature configurations
(default, and `--features telemetry`), plus `cargo test -p degenbot-substrate -p degenbot-bot` when
those crates are touched (scoped per the dispatched-agent lane rules; the PM owns the workspace
ladder). Baseline first, always.

### The per-commit A/B protocol (the executor runs this at every step)

- **Baseline capture (M0 artifact):**
  1. `cargo run --release -p degenbot-solvers --example cl_solve_replay -- <capture.jsonl>` —
     record the profit vector + per-path `stats` (the parity artifact; fixture via
     `degenbot_solvers::capture_fixture`, the `heavy_cl_solve_captures.jsonl` corpus the
     walkmemo readout used — see `docs/architecture/walkmemo-cross-block-reuse-readout.md`).
  2. `cargo run --release --features telemetry -p degenbot-solvers --example cl_cache_lab --
     <capture.jsonl>` — record the strategy matrix + the ns sections.
  3. `cargo bench -p degenbot-solvers --bench walkmemo_contention` — the rayon-contention canary
     (the only bench that stresses the solve entries under parallel load).
- **A (numbers parity):** step 1's profits and `WalkStats` counters byte-equal against baseline
  (epic acceptance: "walk number parity between baseline and gated"). ns fields compare within
  run-to-run noise only after M6; before that they are literally the same code.
- **B (readout parity):** step 2's matrix identical; section ns sums (edge+redge+dir+refine ≤ solve
  total) sane.
- **C (zero-ambient audit):** `grep -n "thread_local!\|Cell<" rust/crates/engine/degenbot-solvers/
  src/cl/ | grep -v tests` must shrink monotonically and hit zero at M6.

### Commits

| Step | Content | Effort (lines / TP) | A/B | Risk advisory |
|---|---|---|---|---|
| **M0** | Record the baseline artifacts (protocol above). No code. | 0 lines | — | Pin machine state (release profile, `taskset` if available); ns sections are noise-sensitive. |
| **M1** | Additive skeleton: `WalkTally` + `WalkTelemetry` + `WalkNsCensus` in `cl/telemetry.rs` (or new `cl/tally.rs`); re-exports in `cl/mod.rs`; **GLOSSARY entries land here** (§6). Nothing consumes them yet. | ~180 added / 3 TP | C only | None — dead code, feature-independent. |
| **M2** | Thread `&mut WalkTally` through `active_set.rs` (entry rewrite :667-687 included); `Mark` carries the tally; entry gains `tele: Option<&WalkTelemetry>` and the one cfg'd merge. `simulate_walk_path` + `hop_sim.rs:102` writer. | ~130 touched / ~28 TP | A + C | **D3 lands here** (max_dense_words becomes per-solve; record the alert-firing delta). `anchor_sims` is a `tally.sims` delta now (:873-895) — same arithmetic, verify in A. No atomic RMAs added; hot cost class unchanged (stack field vs TLS cell). |
| **M3** | Thread through `crossings.rs` (10 writers + signatures) and `word_profile.rs` (2 writers); `simulate_v3_range_swap` pub signature (+ its callers: `path_sim.rs:92`, `active_set.rs:136`, envelope tests, parity example, ~12 test sites). | ~150 touched / ~38 TP | A + B + C | `word_steps` from profile inversions must match — the goldens test (`tests/cl_cache_lab_goldens.rs`) byte-compares solves and will catch attribution drift. Hottest counter (per word step) stays a plain stack write. |
| **M4** | Entry plumbing: `entries.rs` signatures (+tele beside memo), `derive_and_solve_…` threads `None` (:99), `GateDeps.telemetry` + accessor, `mixed/solve.rs:471-486` passes `gate.telemetry()`, **`degenbot-substrate/src/resolve/mod.rs:949,1006` adds `None`**, all remaining test/bench/example entry calls add `None`. | ~90 touched / ~48 TP (mechanical, sed-shaped) | A + C (both feature configs) | Churn commit — do it as one mechanical sweep; the compiler's missing-arg errors are the checklist. Do NOT forget substrate (outside engine/). |
| **M5** | degenbot-bot ownership: `mod.rs:542`-adjacent `Arc<WalkTelemetry>` + `SolveCycleShared` field + `lane_walk.rs:158-171` wiring + 3 test scaffolds + memo-tap test `None`. | ~20 touched / 7 TP | C + `cargo test -p degenbot-bot --lib` | **NOWFGF/layout-churn risk lives here**: `lane_walk.rs` is the most-churned file in the epic queue (T3/T4/SIMPIPE2 history). Keep the diff ≤4 files and land M5 last among behavior commits; rebase-cheap by construction. |
| **M6** | Cut over `cl_cache_lab.rs` to `tele.drain_ns()`/`take_census_pieces()` (the 19-value swap-sweep :296-332 becomes one drain + print); then demolish: delete the TLS block, the 20 statics, both macros, `reset_walk_stats`/`peek`/`take` + `clear_walk_pieces_and_sims`/`path_simulations_now`, the mod.rs static re-exports, and dead `WALK_CENSUS_SIMNS`; convert `cl/tests/mod.rs` TLS helpers to tally reads. | ~120 touched, **net ≈ −250 lines** / ~30 TP | A + B + C (C must hit zero) | The readout must print the same matrix shape; ns numbers are now collector-drained — compare against M0 within noise. Consider asserting ns > 0 to prove the merge path is live under `--features telemetry`. |
| **M7** | Close-out: parity report vs M0 attached to the task result (epic acceptance item), ADR-043 cross-link, `docs/cache-lab-report.md` note that the ns sections now drain from the collector. | ~15 doc lines | none | None. |

**Totals:** ≈ 600 lines touched gross (≈200 added, ≈250 deleted, ≈150 churned), ≈ **120 touch
points** across ≈16 files, 7 commits. Biggest single chunk is M4's mechanical `None`-sweep; biggest
thinking chunk is M2 (the D3 decision + anchor-delta arithmetic).

### Perf-regression risk advisories (per the profiled baseline)

1. **`bump_word_steps` is the hot path** (one write per word-boundary step inside
   `compute_swap_step_v3` loops, `hop_sim.rs:102`, `word_profile.rs:131,181`). Today: TLS cell
   get+set. After: `&mut` stack field. Same or better; **never** make it an atomic on the shared
   collector — that would introduce cross-worker cache-line contention the current design
   deliberately avoids (evidence: telemetry.rs:301-305). If M3's A shows any drift, suspect a
   forgotten `#[inline]` on the tally methods, not the design.
2. **The per-solve merge** (one `parking_lot` lock) replaces 13 TLS resets + 1 census reset per
   solve. Cost comparable; contention canary is `walkmemo_contention`. A regression there means the
   merge moved inside a per-path loop — it must be once per `solve_active_set_path`, not per path
   probe.
3. **`Mark::start` still calls `Instant::now()`** per section (unchanged count: 6 per piece visit).
   Do not "optimize" it during the move; that is a separate experiment.
4. **Default build:** zero ambient writes, zero atomics, one `None` check per solve at the merge
   guard. The `telemetry`-off A/B (protocol step 1 without `--features telemetry`) must be
   indistinguishable from baseline within noise; if not, a cfg arm leaked.
5. **The ns atomics' disappearance changes their contention profile** (today: Relaxed RMAs from
   every rayon worker on the 19 live shared counters; after: local adds + one merge). Expect the telemetry
   build to get marginally *faster* under high parallelism; treat any slowdown >2% on
   `walkmemo_contention` as a bug hunt, not a tolerance.

---

## 6. Vocabulary registration (the Q5 standing rule)

Correction to the brief: **no `CONTEXT.md` exists in the tree** (verified: not at the repo root,
nowhere under `docs/`). The vocabulary home is **`GLOSSARY.md`** (repo root; `AGENTS.md:50` points
at it for machine-emitted artifacts). Per the Q5 rule ("register at the moment they become
stable"), this doc **proposes** the terms and defers registration to **M1** — the commit where the
names become code — so the canonical glossary never holds unshipped vocabulary. Ready-to-paste
entries for the M1 implementer:

```markdown
**WalkTally**:
The per-solve walk telemetry accumulator the active-set walk threads by `&mut`; one value per
`solve_active_set_path` call, merged into the engine's `WalkTelemetry` at exit. Solve-scoped, so
it cannot mix counts across solves or tests.
_Avoid_: "thread-local counters", "the TLS cells", treating it as ambient state.

**WalkTelemetry**:
The engine-owned walk telemetry collector (`Arc`, `Option<&WalkTelemetry>` at the solve entries
beside `WalkMemo`): cumulative wall-time census plus the census-piece log, drained by tooling.
_Avoid_: "the walk statics", "the process globals", a process-global singleton.
```

(Epic `DMRSRY` notes CONTEXT.md vocabulary was "already updated through chunk A" — consistent with
the file having been folded into `GLOSSARY.md`; the two entries above follow its
term/_Avoid_ convention, cf. "Piecewise walker", GLOSSARY.md:486-492.)

---

## 7. Out of scope / adjacent prior art (do not fold in)

- **`degenbot-bot/src/arb_engine/walk_telemetry.rs`** (ergo `3S7HGF`): the per-epoch `WalkMemoStats`
  JSONL/OTel drain. Cited for **shape only** — boundary-drain + final-drain discipline, sink
  injection, activity gating. It consumes `WalkMemo`, not the walk counters; it changes only by one
  `None` arg (§4.3 item 19).
- **`profit_envelope.rs` gate stats** (`GATE_TLS: RefCell<GateStats>` :1520-1521,
  `reset_gate_stats`/`take_last_gate_stats`, read by `lane_walk.rs:186,195`): the envelope's own
  ambient TLS channel with the same reset/take shape. Adjacent, **not** walk telemetry; it has its
  own TLS-budget history (:1517-1520) and deserves the same treatment in a follow-up epic if the
  PM wants the pattern retired crate-wide.
- **`hotpath::measure` instrumentation**: orthogonal plane per ADR-043; epic lists it out of scope.
- **The bot-side `WALK_DENSE_ALERTED` latch and OTel per-path attributes**: consumer-side, shape
  preserved, untouched (§4.3).
