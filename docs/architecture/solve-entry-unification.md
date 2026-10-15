# Solve-entry unification: the unify-or-split decision and its landing

The decision record for the two solve entries (`solve_concentrated_liquidity_piecewise` intake vs
`solve_piecewise`, the one solve body), its bench data, and the landing notes.
Split out of `walkmemo-cross-block-reuse-readout.md` (which owns the WalkMemo
retention questions) so each document keeps one concern; the walkmemo
readout's gated follow-up (b) shares the fingerprint history and links here.

## Entry unification (ergo A47GNH): UNIFY — the split buys no measurable cost

Decision on the question deferred with the entry surface: do the two solve
entries unify (mixed subsumes all-CL) or stay split? **UNIFY.** The split was
justified only by keeping the hotter all-CL path free of the mixed entry's
per-hop `Option` surface; both required bench inputs find that surface costs
nothing measurable, while the split's own route pays a prepared-hop clone the
unified route does not. Scoped implementation replacement: **ergo XW3H5V**.

### Method (both required inputs, offline)

The live node was unavailable in this environment (the regeneration-path
`ws://host.containers.internal:8546` probe: connection refused), so both
inputs are offline over the committed captured corpora — recorded here as a
substitution, not hidden. The harness is
`rust/crates/engine/degenbot-solvers/examples/entry_ab_readout.rs`; it times
TWO routes over the same captured all-CL paths, interleaved reps with
alternating order (median/p95 per path, 25–49 reps):

- **Arm "today"** — the production branch route in `mixed::families::cl`: the
  prepared hops cloned from the resolved path (three `Arc` bumps per hop)
  into `solve_concentrated_liquidity_piecewise`.
- **Arm "unified"** — one borrowed `MixedHopView::ConcentratedLiquidity` per position into
  `solve_piecewise`, the route unification would take.

Parity gate: both arms must return byte-equal
`(optimal_input, profit, hop_outputs)`; any divergence exits 1. Both memo
stances measured (the handle is present in both; the stance gates the
per-probe fingerprint fold). Exact commands:

```text
cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers \
  --example entry_ab_readout [-- <capture.jsonl>]
DR_AB_ITERS=25|49 ...   DR_AB_MEMO=1 ...   # the active-memo stance

# functions-timing over the concentrated_liquidity_solve.* labels:
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=hp.json \
HOTPATH_REPORT=functions-timing,threads \
cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers \
  --features hotpath --example entry_ab_readout
```

### Input 1 — telemetry functions-timing over the `concentrated_liquidity_solve.*` labels

Instrumented run (`--features hotpath`, 156 paths x 5 reps; `hp.json` is a
runtime artifact outside the repo):

| label | calls | avg | p95 | total | share |
|---|---|---|---|---|---|
| `concentrated_liquidity_solve.int_solve_concentrated_liquidity_path` | 781 | 157.53 µs | 350.46 µs | 123.04 ms | 17.56% |
| `concentrated_liquidity_solve.exact_solve_mixed_path_n` | 781 | 157.74 µs | 353.28 µs | 123.19 ms | 17.58% |
| `concentrated_liquidity_solve.active_set` (shared walk) | 1,561 | 156.76 µs | 349.69 µs | 244.70 ms | 34.92% |
| `concentrated_liquidity_solve.build_word_profiles` | 313 | 275.12 µs | 548.35 µs | 86.11 ms | 12.29% |
| `concentrated_liquidity_solve.build_crossing_table` | 313 | 212.90 µs | 416.25 µs | 66.64 ms | 9.51% |
| `concentrated_liquidity_solve.concentrated_liquidity_walk_hop` | 3,121 | 397 ns | 460 ns | 1.24 ms | 0.18% |

Identical call counts, **0.13% per-call difference** on identical
workloads. The walk itself (`active_set`) dominates both entry labels'
inclusive time — the entries are thin intake/fold wrappers over the same
`solve_active_set_path`.

### Input 2 — entry-level A/B (route totals)

| corpus (capture) | memo stance | paths | parity | today (µs) | unified (µs) | unified/today |
|---|---|---|---|---|---|---|
| `heavy_concentrated_liquidity_solve_captures` | off | 156 | 0 div | 614,993 | 618,331 | 1.0054x |
| `heavy_concentrated_liquidity_solve_captures` | on | 156 | 0 div | 45,708 | 45,507 | 0.9956x |
| `live_capture_loop13` | off | 104 | 0 div | 9,844,006 | 9,839,108 | 0.9995x |
| `live_capture_loop17` | off | 217 | 0 div | 23,525,079 | 23,366,284 | 0.9932x |

Stability runs across the session (same harness): 1.0020x / 1.0065x
(`heavy_concentrated_liquidity`, off, 25/49 reps), 0.9970x (`heavy_concentrated_liquidity`, on, 49 reps), 1.0020x
under the instrumented build. Every measurement sits in a **±0.7% band with
no systematic direction**; 477 captured paths x 25–49 reps x 2 arms, **0
parity divergences**, `sims` columns equal. The memo-active stance collapses
the totals (reps 2+ are memo hits: per-probe fold + probe ≈ 7 µs p50) and
still shows no arm difference — the mixed entry's two `Option`-vec +
order-vec fingerprint fold costs the same as the all-CL seq fold at the
probe.

### Which entry is hotter (the split's whole justification)

Row census of the captured corpora by the route the production branch picks
(all hops CL -> all-CL entry; any V2 hop -> mixed entry):

| corpus | rows | -> all-CL entry | -> mixed entry |
|---|---|---|---|
| `live_capture_loop13` | 104 | 104 | 0 |
| `live_capture_loop17` | 217 | 217 | 0 |
| `heavy_concentrated_liquidity_solve_captures` | 156 | 156 | 0 |
| `heavy_mixed_solve_captures` | 369 | 0 | 369 |

In captured live traffic the all-CL entry takes **425 of 425** solve
dispatches — the split protects exactly the hot path it claimed to, and the
measured protection is 0.13% per call at the label level and unmeasurable at
the route level.

### Decision

**UNIFY.** Route all-CL workloads through the mixed entry (borrowed
`MixedHopView::ConcentratedLiquidity`) and delete the duplicated entry surface. The split's only
justification is worth ≤0.13% per call and unmeasurable in route totals,
while the split route pays a prepared-hop clone (three `Arc` bumps per hop)
the unified route does not — the live corpora's 0.9932–0.9995 lean that way.
Unification is a pure refactor at the result level: byte-equal
`(optimal_input, profit, hop_outputs)` on every measured solve. Implementation
lands as scoped replacement **ergo XW3H5V**, which carries the three
behavioral notes the refactor must handle (the 1-hop guard asymmetry at the
substrate intake, the CL-only memo-key re-projection through
`walk_mixed_path_fingerprint`, and the offline-caller surface).

The harness's own 1-hop probe (a truncated corpus row) returns `None` on both
entries — non-discriminating on this corpus; the guard asymmetry
(`is_empty` vs `len < 2`) is code-verified and pinned for decision in XW3H5V.

### What this decision did and did not do

- Added: `examples/entry_ab_readout.rs` (example-grade, relaxed-lint header,
  exits nonzero on parity divergence) and this section. No production `src/`
  changes.
- Did not do: the unification itself (XW3H5V), any live-traffic mix claim
  beyond the two captured loops (the 425/425 figure is the captured shape;
  more capture would refine the mix input, not the decision — the decision
  turned on the tax being unmeasurable), and any ergo state change beyond
  closing A47GNH and opening XW3H5V.

### Unification landed (ergo XW3H5V)

The scoped replacement landed: `mixed::families/concentrated_liquidity.rs` carries ONE route for
every path the arm accepts (one borrowed `MixedHopView` per position into the
unified body — the prepared-hop clone is gone), `solve_concentrated_liquidity_piecewise` is a
forwarding intake convenience, and the duplicated all-CL solve body and its
all-CL-only fingerprint are retired (CL-only compositions fold through
`walk_mixed_path_fingerprint`: one entry, one key space). The contract is
uniform — paths shorter than two hops are refused at every solve surface,
pinned in the solvers tests and at the substrate intake (whose pin also
records that resolution projects nothing for a single-hop input, making the
arity layering explicit).

Post-land harness check (the "intake" arm is now the forwarding intake vs
the shared body; the parity gate is the forwarding contract):

| corpus | paths | parity | intake (µs) | direct (µs) | direct/intake |
|---|---|---|---|---|---|
| `heavy_concentrated_liquidity_solve_captures` | 156 | 0 div | 607,515 | 603,044 | 0.9926x |
| `live_capture_loop17` | 217 | 0 div | 23,514,828 | 23,676,046 | 1.0068x |

Same noise band as the decision measurements, 0 parity divergences — the
intake shape costs nothing measurable either. The tables above stand as the
historical A/B (two separate solve bodies then); nothing in the decision
changes. One label-semantics note for future functions-timing runs:
`concentrated_liquidity_solve.int_solve_concentrated_liquidity_path` now measures the forwarding intake (inclusive
of the body's span), and `concentrated_liquidity_solve.exact_solve_mixed_path_n` the one solve
body for every composition.
