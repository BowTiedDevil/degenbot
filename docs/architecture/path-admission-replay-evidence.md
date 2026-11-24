# Path-admission cutover — matched replay evidence (ADR-107 D10)

Certification for ergo task `EIXMQP`. One corpus, one measurement method, two
binaries: the pre-implementation commit and the final tree. Every number below
comes from an instrumented run of that binary — the production admission doors,
the production solve cycle, the production inline-sim seam, the production crawl
admission, and the production Prometheus instrument set scraped over its HTTP
endpoint. Nothing is a structural estimate or a test-name inference.

Old binary: `3a36a098ccadd2f26d658506b9314b05547314a7`. New binary:
`fc289f118dfab35f2927fcd45b3a61111bdc5c75` plus this task's uncommitted tree.

## Reproduce

### Engine leg (admission, solve, inline sim, scrape)

Harness: `rust/crates/engine/degenbot-bot/src/arb_engine/tests/path_admission_replay.rs`
(gated on `degenbot-bot/otel`, the feature that compiles the real instrument
set). It is corpus-defined and uses only API present at both commits.

Every leg asserts its expectations from the environment, so a run cannot pass
by printing. New binary:

```sh
export DEGENBOT_REPLAY_EXPECT_REGISTERED_PATHS=4 \
       DEGENBOT_REPLAY_EXPECT_UNSUPPORTED_ADMISSIONS=6 \
       DEGENBOT_REPLAY_EXPECT_INLINE_CANDIDATES=4 \
       DEGENBOT_REPLAY_EXPECT_HOOK_REFUSED=1 \
       DEGENBOT_REPLAY_EXPECT_NO_ENCODER_ARM=0 \
       DEGENBOT_REPLAY_EXPECT_SOLVE_EVALUATIONS=4
DEGENBOT_ADMISSION_REPLAY_OUT=/tmp/admission-replay-new.txt \
  just test-rust-nextest -p degenbot-bot --features degenbot-bot/otel \
       -E 'test(arb_engine::tests::path_admission_replay)' --no-capture
```

Old binary: the same command with
`DEGENBOT_REPLAY_EXPECT_REGISTERED_PATHS=10`,
`..._UNSUPPORTED_ADMISSIONS=0`, `..._INLINE_CANDIDATES=8`,
`..._HOOK_REFUSED=2`, `..._NO_ENCODER_ARM=3`,
`..._SOLVE_EVALUATIONS=10`.

### Crawl leg (build, verify, registration)

Harness: `rust/crates/engine/degenbot-bot/src/arb_engine/crawl/unit/tests/path_admission_replay.rs`
(child module of the crawl's offline fixture; every RPC answer is a local
constant-free fixture built from helpers present at both commits).

New binary:

```sh
export DEGENBOT_CRAWL_EXPECT_FIRST_STAGES=3 \
       DEGENBOT_CRAWL_EXPECT_FIRST_VERIFY=0 \
       DEGENBOT_CRAWL_EXPECT_FIRST_REGISTERED=0 \
       DEGENBOT_CRAWL_EXPECT_RESIGHT_STAGES=1 \
       DEGENBOT_CRAWL_EXPECT_RESIGHT_REGISTERED_ANY=0 \
       DEGENBOT_CRAWL_EXPECT_RESIGHT_VERIFY=0 \
       DEGENBOT_CRAWL_EXPECT_RESIGHT_REGISTERED=0 \
       DEGENBOT_CRAWL_EXPECT_BUILD_ENTRIES=1 \
       DEGENBOT_CRAWL_EXPECT_CONTROL_STAGES=5 \
       DEGENBOT_CRAWL_EXPECT_CONTROL_VERIFY=1 \
       DEGENBOT_CRAWL_EXPECT_CONTROL_REGISTERED=1
DEGENBOT_ADMISSION_REPLAY_OUT=/tmp/crawl-new.txt \
  just test-rust-nextest -p degenbot-bot -E 'test(crawl_admission_replay)' --no-capture
```

Old binary: `FIRST_STAGES=5`, `FIRST_VERIFY=1`, `FIRST_REGISTERED=1`,
`RESIGHT_STAGES=5`, `RESIGHT_REGISTERED_ANY=1`, `RESIGHT_VERIFY=1`,
`RESIGHT_REGISTERED=2`, `BUILD_ENTRIES=2`, and the same control values.

### Old binary (either leg)

```sh
git worktree add /tmp/degenbot-old 3a36a098ccadd2f26d658506b9314b05547314a7
cp <repo>/rust/crates/engine/degenbot-bot/src/arb_engine/tests/path_admission_replay.rs \
   /tmp/degenbot-old/rust/crates/engine/degenbot-bot/src/arb_engine/tests/
mkdir -p /tmp/degenbot-old/rust/crates/engine/degenbot-bot/src/arb_engine/crawl/unit/tests
cp <repo>/rust/crates/engine/degenbot-bot/src/arb_engine/crawl/unit/tests/path_admission_replay.rs \
   /tmp/degenbot-old/rust/crates/engine/degenbot-bot/src/arb_engine/crawl/unit/tests/
# add `mod path_admission_replay;` beside the existing `mod`s in
# arb_engine/tests/mod.rs and arb_engine/crawl/unit/tests.rs (uncommitted)
cd /tmp/degenbot-old && \
DEGENBOT_ADMISSION_REPLAY_OUT=/tmp/admission-replay-old.txt \
  just test-rust-nextest -p degenbot-bot --features degenbot-bot/otel \
       -E 'test(path_admission_replay) or test(crawl_admission_replay)' --no-capture
git worktree remove --force /tmp/degenbot-old
```

The harness sources are byte-identical in both runs, so the corpus and the
measurement method are the same by construction.

## Corpus

Sixteen pools over two 18-decimal tokens (plus a V4 pool key), all constructed
in-process, then ten routes registered through the direct `register_path` door —
the live guard ADR-107 D4 names, because the crawl's DB path graph names only
V2/V3/V4 (C7).

| row | hops | families | class |
|---|---|---|---|
| `solidly_2hop` | 2 | SolidlyStable x2 | solve-only family |
| `balancer_weighted_2hop` | 2 | BalancerWeighted x2 | solve-only family |
| `balancer_stable_2hop` | 2 | BalancerStable x2 | solve-only family |
| `curve_2hop` | 2 | CurveStableswap x2 | solve-only family |
| `solidly_v3_mixed` | 2 | SolidlyStable + V3 | mixed supported/unsupported |
| `v2_v3_4hop_out_of_shape` | 4 | V2/V3/V2/V3 | supported families, outside the encoder shape key |
| `v2_v3_heterogeneous_2hop` | 2 | V2 + V3 | supported |
| `v2_v4_heterogeneous_2hop` | 2 | V2 + V4 | supported |
| `v2_all_4hop` | 4 | V2 x4 | supported, all-V2 any-N shape |
| `v2_compose_capable_2hop` | 2 | V2 x2 | supported; its hook refuses (pool de-registered between solve and dispatch) |

The concentrated-liquidity pools carry one initialized tick below and one above
the current price with the full in-range depth, so the mixed V2/V3 and V2/V4
routes price and solve instead of walking into an empty range.

The hook reproduces what the real hook can do: no payload for a route containing
a family the command-stream encoder has no arm for, no payload for a route whose
hop sequence has no producer, and no payload for the one path whose pool
de-registered between solve and dispatch. Every other route gets a payload.

## Results — engine leg

| Quantity | old `3a36a098c` | new tree | instrument |
|---|---|---|---|
| registered paths | 10 | 4 | `path_count` (the registry the gauge mirrors) |
| inline-arm candidates (`arm="inline"`) | 8 | 4 | `degenbot_candidates_found_total` |
| `degenbot_sim_degrades_total{reason="no_encoder_arm"}` | 3 | **absent** (no series) | census `InlineSimDegraded` |
| `degenbot_sim_degrades_total{reason="hook_refused"}` | 2 | 1 (series label-free: `degenbot_sim_degrades_total 1`) | census `InlineSimDegraded` |
| `no_encoder_arm` rate over inline candidates | 3/8 = 0.375 | 0 (series absent) | pairing above |
| per-path solve gate evaluations | 10 | 4 | `degenbot_solve_path_duration_seconds_count` |
| unsupported admissions refused at the door | 0 (no such outcome exists) | 6 | typed `register_path` results |
| `degenbot_registration_skips_total{reason="unsupported-capability"}` | absent | absent | not emitted at the direct door |
| `degenbot_selection_all_targets_declined_total` | absent | absent in this leg only | the all-target leg below measures it |

Old-binary refusals: none — all ten rows registered, including the four
solve-only families, the mixed route, and the 4-hop out-of-shape route.

New-binary refusals (6), verbatim from the typed error surface:

```text
solidly_2hop              hop 0 (solidly-stable pool) declares no settlement command composition
balancer_weighted_2hop    hop 0 (balancer-weighted pool) declares no settlement command composition
balancer_stable_2hop      hop 0 (balancer-stable pool) declares no settlement command composition
curve_2hop                hop 0 (curve-stableswap pool) declares no settlement command composition
solidly_v3_mixed          hop 0 (solidly-stable pool) declares no settlement command composition
v2_v3_4hop_out_of_shape   no command-stream producer for a 4-hop route
```

### Supported-path parity (identical in both binaries, every field)

```text
v2_v3_heterogeneous_2hop  optimal_input 1509719849144939995  profit 1485635106765834594
  hop_outputs 3005856991237538038,2995354955910774589        inline_sim payload
v2_v4_heterogeneous_2hop  optimal_input 1513511218182397611  profit 1481843737728371780
  hop_outputs 3013394245478349086,2995354955910769391        inline_sim payload
v2_all_4hop               optimal_input 4787558748146992357  profit 93340209480539668
  hop_outputs 9501041808366426220,4834228852887262191,9593215660120882892,4880898957627532025
                                                             inline_sim payload
v2_compose_capable_2hop   optimal_input 4810893256776020769  profit 46671192222482844
  hop_outputs 9547128727446589320,4857564448998503613        inline_sim none (hook refusal, counted hook_refused in both)
```

### The C4 misattribution, measured

`v2_v3_4hop_out_of_shape` solved in the old binary (optimal_input
759229696478482946, profit 2236125259432297991) and its hook refusal was
counted as `hook_refused` — the pre-cutover 2 total. The new binary refuses the
same route at admission, so the new `hook_refused` total is 1 (only the genuine
de-registered-pool case).

## Results — crawl leg

Two candidates and one supported control, same fixture in both runs.
`verify_completed` is `VerifyLifecycle::verified_pool_count()` (completed verify
lifecycles); `stages` is the receipt's stage-split length (1 = answered by the
pre-build pool fact, 3 = pre-build gate + build + eligibility, 5 = every stage).

| case | old | new |
|---|---|---|
| `solidly_family_first_sighting` | `stages=5`, `Registered{created:true}`, `verify_completed=1`, `registered_paths=1` | `stages=3`, `Skip{UnsupportedCapability}`, detail names hop 0 solidly-stable, `verify_completed=0`, `registered_paths=0` |
| `solidly_family_resight` | `stages=5`, `Registered{created:true}` (second path id), `verify_completed=1`, `registered_paths=2` | `stages=1`, `Skip{UnsupportedCapability}`, detail `None` (the pool fact), `verify_completed=0`, `registered_paths=0` |
| `supported_cycle_control` | `stages=5`, `Registered{created:true}`, `verify_completed=1`, `registered_paths=1` | identical: `stages=5`, `Registered{created:true}`, `verify_completed=1`, `registered_paths=1` |

Read as work counts over the two solve-only candidates: build-stage entries 2 ->
1 (the re-sighting never builds), completed pool verifications 1 -> 0 (the old
re-sighting joins the one completion the first sighting already made —
`VerifyLifecycle` runs at most once per pool, which is why `verify_completed`
reads 1 after both old candidates), registrations 2 -> 0; the supported control
is unchanged in both (one build, one verify, one
registration). `live_claim_count` reads 0 in every row of both runs because the
claim window closes when the lifecycle completes, so completed lifecycles are the
comparison.

## Results — all-target leg

Harness: `rust/crates/engine/degenbot-arbitrage/tests/release_binding/admission_replay.rs`,
a module of the release-binding fixture (`#[path]`-declared, gated on the
`degenbot-arbitrage/otel-replay` feature, which enables `degenbot-bot/otel` so
the production instrument set is compiled). It binds the real built-in release
(`BoundCmdExecutor::bind` over `ReleaseRegistry::production()`), drives
`TargetSelection::FirstCompatible` through `dispatch_across_deployments` over a
two-hop V4 route whose every hop names a foreign `PoolManager`, and scrapes the
endpoint. It never names the path-level outcome struct, so the same source runs
on both binaries.

```sh
DEGENBOT_ALL_TARGETS_EXPECT_SERIES_LINES=1 \
DEGENBOT_ALL_TARGETS_REPLAY_OUT=/tmp/all-targets-new.txt \
  just test-rust-nextest -p degenbot-arbitrage --features degenbot-arbitrage/otel-replay \
       -E 'test(all_targets_declined_replay)' --no-capture
```

Old binary expects `DEGENBOT_ALL_TARGETS_EXPECT_SERIES_LINES=0`.

Old-binary run: identical source copied into
`/tmp/degenbot-old/rust/crates/engine/degenbot-arbitrage/tests/release_binding/`,
with the same `#[path]` module line and the same `otel-replay` feature line
added to the worktree's `Cargo.toml` (both uncommitted).

| Quantity | old `3a36a098c` | new tree |
|---|---|---|
| built-in `encode` verdict | `Err(Unsupported("the command grammar has no producer for this path"))` | identical |
| candidate registration | `CandidateId{path_id:7, deployment_id:1}`, 2 steps, 2 hops | identical |
| walk `retired` | 0 | 0 |
| lane `gas_profitable` / `gas_unprofitable` / `failures` | 0 / 0 / 1 | 0 / 0 / 1 |
| lane `encode-failed` bucket | 1 | 1 |
| `degenbot_selection_all_targets_declined_total` series lines in the scrape | **0 (absent)** | `degenbot_selection_all_targets_declined_total{class="capability",otel_scope_name="degenbot-bot"} 1` |

The candidate keeps its own encode-failed refusal and its registration in both
binaries; the walk retires no target; and the only difference is the new
path-level `{class="capability"}` fact, counted once.

## Negative probes (assertions proven live)

```text
DEGENBOT_REPLAY_EXPECT_INLINE_CANDIDATES=99 (new binary)
  exit 100, assertion failed: inline-arm candidates (expected 99, measured 4)

DEGENBOT_ALL_TARGETS_EXPECT_SERIES_LINES=0 (new binary; i.e. expect the new series absent)
  exit 100, assertion failed: all-targets-declined series lines in the scrape
  (0 = absent) — so a missing new series cannot pass
```

Restored expectations: engine leg exit 0, crawl leg exit 0, all-target leg exit 0
on the new binary, and the same three legs exit 0 on the old binary with the old
values.

## Scope limits

1. **Crawl out-of-shape case.** The engine leg measures the out-of-shape refusal
at the registration door (row `v2_v3_4hop_out_of_shape`, refused new, admitted and
misattributed old). A crawl-level four-hop candidate was written and then removed
because its fixture needs the V3 tick reads the crawl's `set_v3` does not serve;
the refusal itself is proven by the existing crawl parity case and by the engine
leg.
2. **No live production RPC scrape.** As in the ADR's baseline section, the
corpus is synthetic and in-process, which is what makes it reproducible; the rate
denominator is the same run's inline-arm candidate count, not a one-hour window.
This item is allowed by the task.
