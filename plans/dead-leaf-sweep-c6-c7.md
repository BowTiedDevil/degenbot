# C6 — Port Balancer FixedPoint/LogExpMath numerical tests to Rust, then retire Python leaves

## Goal
- Port the high-value numerical-accuracy assertions (V1 `pow_down`/`pow_up` rounding direction, fractional exponents, `complement` half/small, `ln_36` near-one, `log` round-trips, `pow(fp(2),fp(2))`, `ln(e)`) from the Python parity-oracle test files into the Rust `degenbot-balancer-math` `#[cfg(test)]` corpora as direct unit tests.
- Then delete the now-redundant Python leaves (`log_exp_math.py`, `helpers.py`, the dead fns of `fixed_point.py`) and their two tautological test modules.

## Context
- Sibling to the completed C-series dead-leaf retirements: `5f578b67` (V3 CL swap-math leaves), `3dc419a9` (v4_libraries), `93c296f4` (Balancer weighted_math), `eb01239a` (Balancer stable_math — route-then-delete), `a39595f7` (solidly_stable/camelot).
- After `eb01239a` retired `stable_math.py`, `log_exp_math.py`, `helpers.py`, and every `fixed_point.py` fn except `div_down`/`div_up`/`mul_down` lost their last src consumer (verified by `rg`).
- The Rust `degenbot-balancer-math` crate has `fixed_point.rs` + `log_exp_math.rs` with `#[cfg(test)]` corpora, but those corpora cover ONLY error/overflow paths + V2 fast paths. The numerical-accuracy branches (V1 general path with `MAX_POW_RELATIVE_ERROR` correction, `ln_36` near-one, `log` round-trips, fractional exponents) are untested at the unit level — they are hit only *transitively* via `stable_math`/`weighted_math` corpora.
- `scaling_helpers.py` (the only live src consumer of `fixed_point.py`) imports just `div_down`, `div_up`, `mul_down` — those three fns stay (Candidate 4 retires them separately).
- Decision (grilled): **port-then-delete, minimal targeted port** (~12–14 Rust fns, not a 1:1 mirror), **single commit**, **trim `fixed_point.py` in place** (don't delete the whole file — the 3 live fns stay).

## Acceptance Criteria
- Rust `fixed_point.rs` `#[cfg(test)]` corpus gains: V1 `pow_down`/`pow_up` rounding-direction tests (`x^2`/`x^4` V1 ≤ exact for down, ≥ exact for up); fractional exponent (`4^0.5 → 2` via `pow_down`); `complement` half/small (`complement(0.5e18)=0.5e18`, `complement(0.01e18)=0.99e18`).
- Rust `log_exp_math.rs` `#[cfg(test)]` corpus gains: `ln_36` near-one path (`ln(1.05e18)`/`ln(0.95e18)` vs `math.log` approx); `log` round-trips (`log_2(8e18)=3e18`, `log_10(100e18)=2e18`, `log_e(e*e18)≈1e18`); `pow(fp(2),fp(2))≈4`, `ln(e)≈1`.
- `src/degenbot/balancer/libraries/log_exp_math.py` deleted.
- `src/degenbot/balancer/libraries/helpers.py` deleted (only consumers are the two deleted test files).
- `src/degenbot/balancer/libraries/fixed_point.py` trimmed to only `div_down`/`div_up`/`mul_down` + the `constants` import they need; `add`/`sub`/`complement`/`mul_up`/`pow_down`/`pow_up` + the `log_exp_math` import removed.
- `tests/balancer/libraries/test_log_exp_math.py` deleted.
- `tests/balancer/libraries/test_fixed_point.py` deleted.
- `src/degenbot/balancer/CONTEXT.md` updated: drop the `LogExpMath`/`Helpers`/`InputHelpers`/`Truncated Division` rows (the latter two are already stale — `input_helpers.py` doesn't exist); trim the `FixedPoint` row to the 3 remaining fns; prune the file-tree lines for fixed_point/log_exp_math/helpers.
- `scaling_helpers.py` import path unchanged — zero production diff beyond deletions/trim.

## Validation Gates
- `cargo test --manifest-path rust/Cargo.toml -p degenbot-balancer-math`
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`
- `cargo clippy --manifest-path rust/Cargo.toml -p degenbot-balancer-math --all-targets -- --deny warnings`
- `uv run pytest tests/balancer/`
- `uv run ruff check src/degenbot/balancer/ && uv run ruff format --check src/degenbot/balancer/`
- `just lint-context-maps` (balancer/CONTEXT.md was edited)
- `rg 'log_exp_math|helpers\.py' src/degenbot/balancer/` returns no live references (only CONTEXT.md historical mentions, if any remain)

## Notes
- The Rust impls are already byte-proven by the `eb01239a` stable_math cutover (on-chain parity migrated to Rust and passed). The new Rust tests will be green on first run — this is a coverage port, not a bug fix.
- Do NOT expose `div_down`/`div_up`/`mul_down` via `#[pyfunction]` here — that is Candidate 4 (separate future grilling).
- `PowVersion` enum stays in `constants.py` (used at runtime for bytecode detection + stored on the Rust handle via `balancer_pow_version` getter).

---

# C7 — Remove dead Python log-decoder path + LogListener dispatch registry

## Goal
- Delete the standalone Python log-decoder path (`uniswap/log_decoders.py`, the `LOG_HANDLERS` ClassVar on V2/V3/V4/Aerodrome/Curve pools, the `degenbot.listener.LogListener` dispatch registry) — all dead code with zero production consumers.
- Rewrite the one online test that wired `LOG_HANDLERS → LogListener` to assert the WS log-dict shape directly.

## Context
- **Dead code, not a parity oracle.** The Python `decode_*` functions return a *closure* binding to `pool.external_update`; the Rust `degenbot-decoders` crate returns plain event structs for `BotState`. They were never two implementations of the same interface — the Python path was never the production pump's decoder.
- The production block pump (`BlockPump → Bot::dispatch_log → LogDispatcher → degenbot-decoders → BotState`, ADR-008) decodes in Rust and applies straight to `PyBot` state — it never touches the Python closure path.
- `bot.start_listening()` returns raw `Subscription` objects and wires nothing (verified in `bot.py:628-666`).
- Builders build `*ExternalUpdate` dataclasses directly and call `pool.external_update(update)` — they never call `decode_*` (verified in `v2/v3/v4_pool_builder.py`).
- `LOG_HANDLERS` has exactly 5 occurrences in src/, all *definitions* on pool classes (V2/V3/V4/Aerodrome filled, Curve empty `{}`) — **zero readers**.
- The Rust `degenbot-decoders` crate already has `#[cfg(test)]` corpora for every event (V2 Sync, V3 Swap/Mint/Burn, V4 Swap/ModifyLiquidity) covering valid-decode + wrong-topic + truncated-data + zero-reserve + negative-tick — *more* thorough than the deleted Python tests.
- Decision (grilled): **fold `LogListener` into this commit** (deleting `LOG_HANDLERS` breaks `tests/listener/test_log_listener.py`'s wiring test immediately, so C7 cannot land green without retiring `LogListener` too — keeping it alive for one commit is artificial).
- Decision (grilled): **single `refactor:` commit**, framed as dead-code retirement (NOT route-then-delete — there is no routing cutover; the Rust decode path is already production).

## Acceptance Criteria
- `src/degenbot/uniswap/log_decoders.py` deleted.
- `src/degenbot/listener/__init__.py` deleted.
- `src/degenbot/listener/log_listener.py` deleted.
- `src/degenbot/listener/CONTEXT.md` deleted (entire file documents LogListener/LOG_HANDLERS).
- `tests/uniswap/test_log_decoders.py` deleted.
- `tests/listener/` deleted (whole dir — `test_log_listener.py` is its only file).
- `LOG_HANDLERS` ClassVar removed from: `uniswap/v2_liquidity_pool.py`, `uniswap/v3_liquidity_pool.py`, `uniswap/v4_liquidity_pool.py`, `aerodrome/pools.py`, `curve/curve_stableswap_liquidity_pool.py` (the empty `{}`).
- Corresponding decoder/topic imports removed from those 5 files (e.g. `from degenbot.uniswap.log_decoders import V2_SYNC_TOPIC, decode_v2_sync`).
- `tests/rust/test_subscriptions_live.py` rewritten: the `test_subscribe_logs_and_dispatch_via_listener` test collects logs directly off `async for log in sub` and asserts `isinstance(log, dict)` + `address`/`topics` keys; the `from degenbot.listener import LogListener` import is dropped; the test/class is renamed to drop the `LogListener` framing (e.g. `test_subscribe_logs_yields_log_dicts` / `TestLiveWSAdapterLogSubscription`). The `AsyncProviderAdapter` + `subscribe_logs` assertions stay.
- `pool.external_update` is NOT touched (live — builders call it).
- The `*ExternalUpdate` dataclasses are NOT touched.
- The Rust `degenbot-decoders` crate is NOT touched (no change needed — its corpus already covers every event).
- No new ADR — ADR-008 already records the rationale for the Rust decode path that makes this code dead.

## Validation Gates
- `uv run pytest tests/uniswap/ tests/aerodrome/ tests/curve/ tests/builders/`
- `uv run pytest tests/rust/test_subscriptions_live.py` (collection/import must succeed even though the online test is skipped in CI without `ETHEREUM_ARCHIVE_NODE_WS_URI`)
- `uv run ruff check src/ && uv run ruff format --check src/`
- `rg 'log_decoders|LogListener|LOG_HANDLERS' src/ tests/ examples/` returns no live references (only docs/migration-guides prescriptive mentions, which stay)

## Notes
- The `listener/CONTEXT.md` ambiguity ruling ("LogListener vs SubscriptionManager — SubscriptionManager deleted") is about an already-deleted thing; it goes with the file.
- The migration guide `docs/migration-guides/three-layer-transition.md` §2.3 smell-lists `uniswap/log_decoders.py` as a candidate to evaluate — that is *prescriptive* (the rubric for future sweeps), leave it.