{
  "id": "aff2a1c7",
  "title": "Make UniswapEngine::resolve_path build only the solver representation it needs",
  "tags": [
    "rust",
    "performance",
    "solver_dispatch",
    "uniswap_engine"
  ],
  "status": "closed",
  "created_at": "2026-06-16T17:01:07.955Z"
}

Performance and interface cleanup in `rust/src/optimizers/uniswap_engine/solver_dispatch.rs`.

For every V3 hop, `resolve_path` builds:
- a full f64 `V3TickRangeSequence` (`build_sequence`),
- an `IntV3TickRangeHop` (`build_int_v3_hop`),
- an `IntV3TickRangeSequence` (`build_int_v3_sequence`).

The current solver dispatch only consumes `as_int_sequence()` (and mixed paths also use the integer sequence). For V4 hops an empty `base: HopState::new(0.0,0.0,0.0)` placeholder is created just to satisfy a dead f64 path.

Tasks:
- Remove unused fields from `ResolvedHop` (`seq`, `int_hop`, and the V4 `base` placeholder) once confirmed truly unused.
- Have `resolve_path` request only the representation required by the solver branch (integer sequence for CL hops; V2 state for V2 hops).
- Keep any f64 fallback behind an explicit, conditional path if still needed for diagnostics.
- Run `just test-rust` and `just lint-rust`.
