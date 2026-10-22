# T2OUL2 Phase 1 — GUSD/3Crv metapool rate-path parity analysis

**Conclusion: the divergence is the metapool base-rate SOURCE, not the math and not the strategy map.**
The contract's `_vp_rate()` uses the metapool's cached `base_virtual_price()` while the
base cache is fresh (`block.timestamp <= base_cache_updated + 600`) and the base pool's live
`get_virtual_price()` only after expiry. The Rust I/O layer always takes the live value
(`curve_dy_io.rs::resolve_dy_inputs` -> `CurveDataProvider::virtual_price`) and the current
Python driver bypasses its own cache-aware resolver on the swap path.

Evidence (block 26204126, GUSD/3Crv `0x4f062658EaAF2C1ccf8C8e36D6824CDf41167956`):

- sweep failure: `260189 GUSD for 3Crv — calc 2505906561443926636636, contract 2505906561443977164727`
  (`Δ = 50,528,091`, 2.016e-14 relative)
- cached `base_virtual_price()` = 1,039,823,717,425,653,995; live 3Crv `get_virtual_price()`
  = 1,039,823,717,425,675,030 (`Δ = 21,035` wei, 2.02e-14 relative)
- exact integer replica: `dy(vp=cached) = 2505906561443977164727` (== contract),
  `dy(vp=live) = 2505906561443926636636` (== repo calc)

Fix seam: generalize the metapool rate fetch in `curve_dy_io.rs` (~15 lines) + stub/test
updates in `curve_dy_io/tests.rs`. No strategy-map entry, no math change, no engine/PyO3 change.
