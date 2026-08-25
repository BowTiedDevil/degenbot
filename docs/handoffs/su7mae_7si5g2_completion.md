## 7SI5G2 — Live confirmation + hard cutover: DONE

**Soak (gated, env flag):** 56 min, 548 solve cycles, 0 desync trips. Gate cumulative: evaluated=2366, skipped=1727 (~73% of screened paths provably skipped at floor), unsupported=9496.

**Incident during cutover:** first default-on run died at startup registration with `PanicException: overflow` at profit_envelope.rs:88 — `ceil_div` did a bare `n + d - 1` and `eval()` deliberately saturates to I512::MAX on overflow, so a saturated numerator overflowed the bump add. Data-dependent; surfaced via an extreme pool state at registration. Fixed in f33049294: on checked_add overflow return I512::MAX directly (still a rigorous upper bound). Regression test `ceil_div_saturates_on_max_numerator` added. Clippy -D warnings clean; full solvers suite green (90 lib + integration).

**Cutover commits:**
- 144665b5b feat(solvers): make profit-envelope gate unconditional (SU7MAE 7SI5G2)
- f33049294 fix(solvers): ceil_div saturates instead of overflowing on I512::MAX numerator

**Post-cutover production verify:** bot restarted WITHOUT the env flag (gate now default-on); survived the previously fatal registration window (5m04s uptime vs ~45s death before fix); 44 solve cycles, 0 aborts/desyncs/panics.

**Replay golden check:** 12/12 goldens match, deterministic, false skips 0, heaviest median 948us (path 5).

**Follow-up tasks filed:** 7HUYWM (DB stamp-honestness race), 2WDZ5Y (V4 ModifyLiquidity symmetric test), 3X6SZ5 (Live-transition bitmap-diff probe), M6776W (envelope for Solidly/Curve/Balancer families — now the largest solver-CPU lever, ~73% unsupported).