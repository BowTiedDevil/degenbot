Verified fully landed on takeover (commit a256954d, "feat(curve): port standard
stableswap get_dy to core simulate_swap"). All acceptance criteria met: a_precision
plumbed through CurvePoolIdentity/RegisterCurvePoolParams/register_curve_pool PyO3
signature; simulate_curve_stableswap_swap implements get_dy (xp rate-scaling, amp
a_coefficient*a_precision, get_y, dy-1, fee subtraction, rate descale); Tier-2
dual-driver recorded constant 934112765606210873 (both pool_handle_balance_vector.rs
and test_balance_vector_pool_handle_dual_driver.py) + symmetry/monotonicity; Tier-3
revm oracle exists; Curve ZERO sentinel arm retired. My only addition: refreshed the
stale simulate_swap module header + function doc (783cb189) that still claimed
Curve/Balancer/AerodromeV2 invariant math was unported. Validations: just test-rust
(PASS), just test-python (2455 passed/30 skipped), just lint-rust (PASS).
