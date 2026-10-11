# SERRGX result — config-holder env-snapshot seam

## What shipped

- `degenbot-config/src/holder.rs`: the first-wins process-once snapshot became a swappable slot (`RwLock<Option<&'static Arc<BotConfig>>>`, leak-on-publish). `install()` keeps production first-wins; `config()`/`config_arc()` signatures unchanged; `installed()` semantics unchanged (separate DEFAULT slot). `#[doc(hidden)] reinstall_for_tests()` is the sanctioned seam.
- `degenbot-python/src/config.rs`: LOADED + VERDICT became republishable (same leak-on-publish shape); `republish_loaded()` (crate-visible) fixes the stale-derived-verdict the reset would otherwise leave.
- `degenbot-python/src/config_seam.rs` (NEW, raw `_ffi` per ADR-013 — not re-exported into `degenbot`): `reinstall_config_holder_for_tests()` (fresh env+file load → holder reinstall + republish) and `resolved_node_uri_for_tests(chain_id, scope)` (read-only view of the CURRENT holder snapshot).
- `tests/conftest.py`: `governed_holder` fixture — govern(env) applies overrides + re-seams; teardown restores env exactly and re-governs ambient so non-opt-in tests are unchanged.
- `tests/test_config_holder_seam.py`: guards (env A → A, re-govern → env B → B across holder + verdict + the provider-factory step; non-opt-in unchanged; env restored).
- `src/degenbot/_ffi/__init__.pyi`: regenerated.

## Call-site audit (the safety work)

Process-once derived statics documented SAFE (process-lifetime sizing: cpu_budget SOLVE/AMBIENT workers, state_lock thresholds, verify permyriad, telemetry wiring) — deliberately not re-sized, same boundary as the io runtime. The shell config statics FIXED (republishable). All other call sites per-construction/per-call.

## Live-engine policy

Safe-by-design, no panic: engines read the holder at construction; a reinstall re-governs only future resolutions.

## PM review

- Negative probe (PM-run): neutralizing the holder reinstall trips the env-B guard red; restored → green.
- PM in-review fixes: rustfmt on 3 files; doc-comment reword (the ArbitrageEngine single-mention architecture gate); stub regen completion.
- Gates: degenbot-config / degenbot_rs / scoped clippy green; fmt-check, check-engine-impl-blocks green after fixes; full suite 3132 passed, 0 spray signatures. The 6 suite failures and the lint-rust-check breakage are foreign-attributed (tick_assembly / degenbot-db / degenbot-math — the concurrent stream's uncommitted churn; same tick-map signature, attribution pattern as chunk B).
