# UO3JM4 — V3→V4→V3 1-wei take-overdraw: discriminator outcome

## Result: NO in-repo math bug — `v4_simulate_swap` is byte-exact.

Commit `a7747` (`rust/crates/degenbot-pools/tests/tier3_v4_pool_swap_vs_revm.rs`).

### 1) Closed the tier-3b V4 oracle gap (protocol-fee path)

Every pre-existing tier-3b V4 oracle seeded `slot0.protocol_fee: 0`, so the
on-chain `calculateSwapFee(proto, lp)` fee-combination path was never
byte-exactly cross-checked — yet that is exactly the path the fee-1/tiny
divergence pool exercises (lp_fee=50 + protocol_fee=0xd00d → combined 63 pips).

Threaded `state.protocol_fee` through the revm slot0 seed and added
`v4_pool_fee1_protocol_fee_override_matches_sim`: drives the canonical
PoolManager on the exact fee-1 scalars
(`sq=79_231_869_042_278_935_382_727_675_145`, `liq=94294142`, lp_fee=50,
protocol_fee=0xd00d) at the recorded amounts 9583/9585/9586 + spread, in both
directions. **GREEN**: `v4_simulate_swap` === on-chain `BalanceDelta`
byte-for-byte.

### 2) In-repo solver is consistent

A full-input sweep (amt `[1,4000]`) of the V4 int-sequence walk
(`build_int_v4_sequence` → `int_simulate_v3_swap` — the exact path the Möbius
solver uses for `hop_outputs[1]`) vs `v4_simulate_swap` on the reconstructed
fixture state gave **0 mismatches**. So
`solver-int == v4_simulate_swap == on-chain PoolManager` on the reconstructed
state; the harness already reports `matched=true` (PASS).

### 3) Where the 1-wei really lives

The recorded live divergence (`predicted=9586`, `actual=9585`) is **not
reproducible from the static fixture**. On the reconstructed state
`v4_simulate_swap` gives 9583 at the recorded input — a 2-wei gap vs the
recorded live actual (9585), LARGER than the 1-wei target. The live pool state
the solver saw (sqrt/position set) differed from on-chain at sim time: a
live-state reconstruction/staleness artifact, not Rust pool-state math and not
block anchoring. This aligns the residual cause with the solver-staleness class
already guarded by U6RNHH (solve-stage future-price tripwire) + TQ43TU
(staleness gate).

## Residual (operational, not in-repo code)

Recapture a reproducing live state via
`scripts/watch_fee1_overdraw.py` → `scripts/capture_fee1_v3v4v3_fixture.py`,
then re-run `cargo run -p degenbot --example fee1_v3v4v3_solver_fixture`. If a
solver-vs-oracle divergence surfaces on the byte-faithful fixture it is an
int-solve crossing bug to fix here. No Rust change is required for what is
proven today.
