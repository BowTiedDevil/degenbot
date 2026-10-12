# GMDX5V — simulation-free `t`-parametrized solve for one-Solidly-hop mixed paths

> ## MANAGER VERDICT: implemented, measured, REJECTED, REVERTED
>
> The implementation met every parity and sim-count acceptance criterion, and the
> agent reported the wall-time regression honestly. The manager reproduced the
> bench independently (`cargo bench -p degenbot-solvers --bench solidly_t`):
>
> | arm | wall time (18-path corpus) |
> | --- | --- |
> | `closed_form_t_search` | 44.26 ms |
> | `golden_section` | 542 µs |
>
> — an 81x regression, confirmed first-hand. Rejected on three grounds:
>
> 1. **The per-probe cost inverts the method's premise.** Every closed-form
>    probe extracts a fourth root via two `isqrt_u2048` calls plus U2048
>    Mobius arithmetic on ~1000-bit intermediates; that costs MORE than the
>    leaf's own U256 Newton solve. Zero-simulation probes are not cheap
>    probes. Even with the 1024-sample exact refine removed, the model search
>    alone (97-point geometric scan + ~192-probe golden + ~192-probe edge
>    bisection, ~670 big-integer probes) sits near 30x the golden arm's entire
>    budget.
> 2. **The quality delta is economically nil.** The 93 "strictly better" paths
>    beat the golden arm by a few WEI on flat stable-pool plateaus (the agent's
>    own analysis: golden's bracket midpoint lands a few wei below the discrete
>    max). 1e-18 of a token is not a product feature.
> 3. **Complexity without a customer.** ~1100 lines of U2048 machinery, a
>    second search path, and dual-arm dispatch, behind a hot loop that measurably
>    loses.
>
> **Disposition**: the production changes were reverted (the crate is
> byte-identical to the SR1 commit); the design record below is preserved.
> The chunk's real deliverable is the knowledge: the golden-section arm is
> already within wei of the discrete optimum on realistic one-Solidly-hop
> pools, so the search method is NOT the bottleneck for this family — the
> expensive part is the leaf's invariant solve, and every alternative probe
> representation (ratio, fourth root, cubic root) costs at least as much as
> the Newton iteration it would replace. The synthetic parity corpus and
> brute-force verification are salvaged into a permanent golden-arm regression
> test by a follow-up task, pinning that conclusion against future regressions.
>
> Design record: this doc IS the record of the rejected patch. The patch and
> module lived only as session-local scratch (`.scratch/sr2-rejected.patch`,
> `.scratch/sr2-solidly_t.rs`) that was never committed and does not survive;
> the derivation, the measured regression, and the disposition above are the
> durable reference.

---

## Implementation record (as reviewed, pre-revert)



## What changed

- **New module `rust/crates/engine/degenbot-solvers/src/mixed/solidly_t.rs`** (the one new home):
  the exact integer Möbius composition of the prefix/suffix V2 hops (in a U2048 accumulator so a
  long composition cannot wrap), the Möbius inverse, the Solidly hop's `t = y/x0` I/O, the search,
  and the final discrete patch. Registered from `mixed/mod.rs`.
- **`mixed/solve.rs`**: the former `solve_solidly_path_int` golden body was renamed
  `solve_solidly_golden_arm` (body byte-for-byte unchanged) and a new dispatcher
  `solve_solidly_path_int` first tries the `t`-search arm and falls through to the golden arm when
  it declines. `solve_solidly_t_arm` / `solve_solidly_golden_arm` are re-exported for the A/B bench
  and integration callers. `simulate_solidly_path_outputs` / `solidly_hop_v2_equiv` widened to
  `pub(crate)`; a `#[cfg(test)]` thread-local `sim_counter` was added (test-only probe counter, no
  production telemetry channel).
- **`profit_envelope/derive.rs`**: the faithful-arithmetic window checks were extracted from
  `solidly_stable_rise` into a shared `pub(crate) solidly_stable_arithmetic_faithful(ra, rb,
  reserve_out, decimals_in)` predicate (re-exported from `profit_envelope`). The rise keeps its line
  only where it holds; the `t`-search uses it as its eligibility test and to bound its search window.
- **`benches/solidly_t.rs`** (new criterion bench, registered in `Cargo.toml`).

## The design

`t = y/x0` reparametrizes the search onto the Solidly hop's post-trade reserve ratio. The level
curve `x0·y·(x0²+y²) = K` (`K = calc_k·ONE³`) makes every quantity closed-form in `t`:
`x0⁴ = K/(t+t³)` (two integer square roots), `m_scaled = x0 − Ra`, `y = t·x0`, `n_scaled = Rb − y`.
The prefix/suffix compose to exact integer Möbius maps, exactly invertible. The smooth model's
optimum is found by a coarse geometric scan + golden section, costing ZERO path simulations.

Eligibility is narrow: exactly one Solidly hop, all other hops V2, the Solidly/Aerodrome invariant
(not Camelot's rounding variant), and the faithful-arithmetic predicate. Anything else falls back
to the unchanged golden arm.

### Deviation from the settled shape (please review)

The task's math calls for every probe to be closed-form. On the captured/synthetic pool sizes the
**smooth** closed form is exact over the reals but cannot see the Solidly leaf's floor staircase:
the profit plateau of a stable-pool-vs-V2 arb is flat over ~10⁴ wei, and the smooth argmax drifts
hundreds of wei from the discrete argmax, so a ±3 patch (or even a 32-probe golden section on the
discrete sim) lands 1 wei short of the golden arm. To keep parity absolute, the refine evaluates
the leaf's own per-hop arithmetic (`calc_k` + `get_y_solidly`, plus the floored prefix/suffix chains)
— simulation-free, no full-path `simulate_solidly_path_outputs` call until the verification patch.
That refine is a dense scan (1024 samples) of the smooth model's near-optimal interval; it is what
makes the arm strictly better than the golden arm, and it is also why the arm is slower in wall time
(see the bench). This is a real trade-off and the manager may want to reconsider the budget/wall-time
balance.

## Parity (the drift net)

Neither committed mixed corpus carries a Solidly leg (asserted by
`committed_mixed_corpora_carry_no_solidly_legs`: `heavy_mixed_solve_captures.jsonl.zst` = 369 paths,
`live_gatebursts_mixed.jsonl` = 24 paths, no `Solidly` hop kinds). Parity is therefore on a
synthesized one-Solidly-hop corpus spanning the imbalance / decimals / fee matrix (4 shapes × 3
decimal scales × 3 Solidly fees × 2 V2 fees × 4 basis-point imbalances = 288 paths), comparing the
**dispatched** arm (t-search, else golden) against the golden arm:

```
parity: 288 paths, 143 profitable, 143 fast-path, 50 equal, 93 strictly better, 0 regressions
```

- `profit_new >= profit_old` on every path (0 regressions).
- 93/143 strictly better: these are the golden arm's bracket residual on the flat stable-pool
  plateaus (its 25-iteration bracket midpoint + ±3 sweep lands on a jagged point a few wei below the
  discrete max). The exact-model scan places the discrete argmax, so the new arm is strictly better
  rather than equal — the drift net working in the new arm's favour, not a search bug.
- The fine brute-force test (`brute_force_confirms_the_discrete_optimum`) sweeps ±20000 wei at unit
  resolution around the arm's answer and confirms `arm.profit >= brute_max`.

## Sim-count evidence

`sim_count_pins_the_win` (one-Solidly-hop path): the `t`-arm spends **7** path simulations (the
bounded ±3 patch + one extension), the golden arm **36**; the test asserts `<= 8` and `>= 27` in both
directions and that the arm's returned count matches the thread-local probe counter. The search
itself runs zero path simulations.

## Bench numbers (`cargo bench -p degenbot-solvers --bench solidly_t`, 18-path corpus)

| arm | wall time (18 paths) | per path |
| --- | --- | --- |
| `closed_form_t_search` | 45.0 ms | ~2.5 ms |
| `golden_section` | 0.537 ms | ~30 µs |

The `t`-search wins the simulation count decisively but is ~80x slower in wall time on this corpus:
the exact-model refine is a 1024-sample dense scan of the near-optimal interval (the plateau is flat
enough that a golden section converges past the discrete argmax). Recorded as-is; this is the
trade-off named above.

## Gates

- `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings` — **PASS**.
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` — **PASS** (no diffs).
- `just test-rust-nextest -p degenbot-solvers` — **PENDING**: the foreground run detached past the
  harness bound; the harness reports it still running. A direct narrow run
  (`cargo nextest run -p degenbot-solvers -E 'test(solidly_t)'`) passed all 5 new tests, and the
  pre-existing `degenbot-config::no_stray_env_reads` failure is not in this crate.
  NOTE: the workspace-wide recipe also compiles `degenbot-aave` test targets, which a concurrent
  foreign stream currently has mid-edit (`RunSpec` missing `max_block_range`) — that is not mine.

## Foreign activity

A concurrent Python/Rust stream is committing to the tree (e.g. `46454bbca`). Observed unrelated
modifications (left alone): `src/degenbot/**` (`_bot.py`, `bot_lifecycle.py`, `uniswap/deployments.py`,
`builders/request.py`, `types/abstract/liquidity_pool.py`, `runner/bot_runner.py`, `_ffi/aave.pyi`),
`rust/crates/integrations/degenbot-aave/**` (incl. the broken `RunSpec` test initializers),
`rust/crates/shells/degenbot-cli*`, `rust/crates/facade/degenbot/**`, `docs/cli/aave.md`,
`docs/rust-cli.md`, `rust/crates/shells/degenbot-python/src/aave_updater.rs`. None of these are
reverted or touched.

## Files

- `rust/crates/engine/degenbot-solvers/src/mixed/solidly_t.rs` (new)
- `rust/crates/engine/degenbot-solvers/src/mixed/mod.rs`
- `rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`
- `rust/crates/engine/degenbot-solvers/src/profit_envelope/derive.rs`
- `rust/crates/engine/degenbot-solvers/src/profit_envelope/mod.rs`
- `rust/crates/engine/degenbot-solvers/benches/solidly_t.rs` (new)
- `rust/crates/engine/degenbot-solvers/Cargo.toml`
- `docs/ergo-results/gmdx5v-result.md` (this file)
