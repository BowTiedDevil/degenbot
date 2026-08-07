# T3 — CL test co-location: DECIDED (defer, keep resident)

## Decision
Keep the CL-family test blocks in `bot_core/mod.rs`'s single resident `mod tests`.
Do NOT co-locate them into `cl_orchestration.rs` in this epic.

## Why (measured, not guessed)
The resident `mod tests` (~4,100 lines) is a single cohesive unit whose helpers are
shared **across families**, not per-family:

- Low-level address/factory helpers: `make_pool_addr`, `make_token0/1`,
  `make_factory`, `make_params` (V2) — used by V2, V3, V4, and curve tests alike.
- Registration helpers: `register_v3`, `register_v3_on_core`,
  `register_v4_on_core`, `register_v4_on_core_with_pid`, `make_v3/_v4_params_in_spec`
  — used by CL tests AND cross-family dispatch / reorg / solver-calc tests that
  must stay resident (e.g. `apply_swap_by_pool_id_routes_to_v4_*`,
  `get_v3_or_v4_pool_reads_*`, `v3_restore_before_block_*`).
- CL and non-CL tests interleave in the same module (a V2 test sits between CL
  snapshot tests; a CL test between V2 identity tests).

## Why this is the project-consistent resolution
The plan doc's own T3 mechanism — "relocate the helper next to its only remaining
user" — does not apply: there is ALWAYS more than one family-remaining user per
helper (e.g. `register_v4_on_core` is needed by both the CL apply tests we'd move
and the cross-family `apply_swap_by_pool_id` tests we'd keep). The only ways to
force co-location both violate the project's stated rules:

1. **Duplicate the shared helpers** into a second test module — creates two copies
   that must stay in sync (drift), contradicting AGENTS.md "one concept, one
   spelling".
2. **Share helpers across module boundaries** (`pub(crate)` test helpers / a
   `pub(crate) mod tests`) — exactly what the plan T3 explicitly discourages
   ("not shared across module boundaries unless trivially re-exported").

## What "done" means here
- The CL methods moved (T2) are fully exercised by the resident tests, which pass
  unchanged (410 tests green + standalone) — the RED-neutral→GREEN rule holds.
  Moving the impls changed nothing about callability because they are inherent
  `BotState` methods reachable from any module.
- No behavioral change was introduced; no test body was edited to pass.
- The test-module **decomposition is its own, larger task**: first split the shared
  helpers into a `bot_core::test_util` (or `tests/common`) module with `#[cfg(test)]`
  pub(crate) visibility, THEN per-family test co-location becomes mechanical. That
  is out of scope for this epic (it is a refactor of the test surface itself, not
  the impl god-file) and is left as a documented follow-up.

## Follow-up pointer
Future task: "Decompose bot_core `mod tests` shared helpers into a `pub(crate)`
`#[cfg(test)]` `test_util` module, then co-locate per-family test blocks with their
`*_orchestration.rs` modules."
