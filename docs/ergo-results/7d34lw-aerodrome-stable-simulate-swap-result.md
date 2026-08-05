## 7D34LW — Wire Aerodrome V2 stable swaps through simulate_swap (decimals plumbing)

### Done (commit `391bb666`)
Dispatched the Aerodrome V2 **stable** arm of `simulate_swap` to
`calc_exact_in_stable_solidly`, retiring the stable-path `Ok(U256::ZERO)`
sentinel. The volatile path was already wired (`787d646e`).

### Implementation
1. **Decimals on identity + params** — added `token0_decimals`/`token1_decimals`
   (`u8`) to `AerodromeV2PoolIdentity`, `RegisterAerodromeV2PoolParams`, and the
   PyO3 `register_aerodrome_pool` signature (with the matching `#[pyo3(signature)]`
   update). No backwards-compat layer — every caller updated.
2. **Builder fetches decimals** — `build_aerodrome_v2` reads ERC-20 `decimals()`
   for both tokens via the existing `decimals_of` choreography primitive.
3. **simulate_swap stable arm** — `simulate_aerodrome_stable_swap` helper calls
   `calc_exact_in_stable_solidly` with `10**decimals` scale factors (mirrors the
   Python `AerodromeV2Pool` companion's `10**token.decimals`).
4. **Pool handle getters** — added `aerodrome_token0_decimals`/`aerodrome_token1_decimals`.

### Tier-2 dual-driver pair (ADR-005)
Recorded constant **753627265063405946** for the equal-balances fixture
(reserve0=reserve1=1e18, both 18 decimals, fee (3,10000)=0.03%, swap 1e18):
- Rust: `pool_handle_aerodrome.rs` `aerodrome_stable_swap_matches_recorded_constant`
  + `aerodrome_stable_swap_is_monotonic` (symmetry + monotonicity + bounded-by-input).
- Python: `test_aerodrome_pool_handle_dual_driver.py` same constant + symmetry +
  monotonicity via the PyBot handle.
The shared builder parity fixture (`aerodrome_pool_builder.json`) was extended with
the decimal fields; both parity consumers (`parity_aerodrome_builder.rs` +
`test_aerodrome_builder_dual_driver.py`) now assert they flow losslessly.

### Validation gates — all green
- `just test-rust` — PASS
- `just test-python` — PASS (2457 passed, 30 skipped)
- `just lint-rust` — PASS (clippy ++ no-pyo3); the stable arm was extracted into a
  helper to stay under the clippy `too_many_lines` bound on `simulate_swap`.
