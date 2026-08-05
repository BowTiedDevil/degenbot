## QHGN2E — Resolve Identity to DEX name via deployments lookup

### Done (commit `66f29e98`)

Implemented DEX-name resolution on the structural `Pool` handle per the WCSS3V
spike's recommended **Shape (A)**.

### Design (inherited from WCSS3V, with one adjustment)
- Added `DexName` enum (`Uniswap`/`SushiSwap`/`PancakeSwap`/`Camelot`/`Aerodrome`/
  `SwapBased`/`Balancer`) to `degenbot-uniswap::dex_identity`.
- `deployments.rs` now parses the JSON `name` label into `Option<DexName>` on each
  `DeploymentRecord` + `resolve_dex_name(chain_id, factory) -> Option<DexName>`.
  **Finding:** `pool_type` collapses every V2 row to `"uniswap-v2"` and `dex_variant`
  is mostly absent, so the `name` label is the only complete per-row DEX
  discriminator.
- **Adjustment vs the spike's preferred mechanism:** the spike provisionally ranked
  "store chain_id on the V3/V4 identities" first, but that requires touching 105
  `RegisterV3/V4PoolParams` literals. Threading `chain_id` through the structural
  `Pool::new(entry, chain_id)` handle needed only 13 call sites, so that lower-churn
  path was used. `Pool::new` now takes `chain_id` (additive; callers updated).
- `Identity` variants gained `dex: Option<DexName>`; `Pool::identity()` resolves V2
  and V3 via the deployments lookup (chain_id + factory). V4/Curve/Balancer degrade
  to `dex: None` (generic variant, never an error).
- PyO3: `PyPool` carries `chain_id`; new `dex_name` getter surfaced on the handle.

### Tier-2 dual-driver pair
- Rust `pool_handle_v2.rs` / `pool_handle_v3.rs`: resolved-name assertions for a
  SushiSwap V2 mainnet factory and Uniswap V3 mainnet factory, plus a `None`
  fallback for an unknown synthetic factory.
- Python `test_v2/v3_pool_handle_dual_driver.py`: same assertions via `handle.dex_name`
  (the V3 known-deployment case uses the real CREATE2-computed pool address).

### Validation gates — all green
- `just test-rust` — PASS
- `just test-python` — PASS (2461 passed, 30 skipped)
- `just lint-rust` — PASS (fmt + clippy + no-pyo3)
