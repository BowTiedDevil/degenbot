# ADR-085: Pool construction is one family-tagged request at both tiers

**Status: accepted (2026-10-25).** Recorded at the architecture-review grilling
(scope: the Python companion and the PyO3 seam) so future reviews do not
re-suggest the family×verb matrix it deletes. Extends ADR-078's write door from
registration to construction; applies ADR-082's rule one tier further out.

## Context

The core owns construction policy — GLOSSARY's **Pool construction** assigns the
route order, the DB two-step identity, and get-or-register to the core — and
ADR-078 gave registration one request and one verdict. Construction did not
follow. The measured shape:

- **The PyO3 `Bot` presents twelve construction/registration verbs.** Five of
  them (`build_aerodrome_v2_pool`, `build_balancer_weighted_pool`,
  `build_balancer_stable_pool`, and the V2/V3 builders behind them) have no
  Python caller outside the funnel; `register_v4_state_view` has none at all.
- **The funnel is a flat kwarg bag.** `construct_pool(address, family, variant,
  block, db, tick_data_fetcher, factories, registries, vault,
  invariant_version)` carries four hints that are each meaningful for exactly
  one family, selects on a `family: str`, and answers the positional tuple
  `(pool_id, token0, token1, address, variant)` where the two token fields are
  empty strings for the multi-token families. The caller then makes a second
  call (`get_pool(pool_id)`) to obtain the handle.
- **The matrix exists one tier below too.**
  `degenbot-bot::bot_core::build_register` carries `build_and_register_v2`,
  `_aerodrome_v2`, `_balancer_weighted`, `_balancer_stable`, `_curve_pool`,
  `_v3`, `_v4`. `degenbot-substrate::registration_door`'s `PoolRegistration` is
  registration-only by its own doc: "additive and no existing entry is rewired
  onto it."
- **The driver finishes the build, per family.** `Bot.build_pool` branches on
  family after the core returns: V3 gets `seed_genesis`; Balancer gets
  `_register_delegated_tokens`; Curve gets `CurvePoolBuilder.finalize` (base-pool
  recursion, coin / underlying / LP ERC20 builds) — the last two exist only so
  the companion's getters stop answering `None` for addresses the core already
  knows. `CurvePoolBuilder.build` re-enters the same entry a second way. V4 takes
  a different shape again: `resolve_v4_identity` → `build_many` → `build_v4_pool`
  as three driver steps.
- **Identity divergence is checked in the driver**, twice, by raising
  `DegenbotValueError` when the core's echo disagrees with the handle or with
  the resolver.

## Decision

**Construction is one family-tagged request and one verdict at both tiers.**

1. **The request is a core value.** `PoolConstruction` is a family-tagged enum in
   `bot_core` — `V2 { … }`, `ConcentratedLiquidity { … }`, `ManagedV4 {
   pool_manager, pool_id, immutables? }`, `Curve { … }`, `Balancer { … }`,
   `AerodromeV2 { … }` — carrying the pool identity and the resolved policy
   values (route, block, DB eligibility, registry lists). One
   `construct_and_register(state, request) -> ConstructionVerdict` walks the
   route, builds, registers, and folds the result. The per-family
   `build_and_register_*` entries become private helpers behind it. The request and
   verdict already exist — `pool_builder/construction_door.rs` landed them
   additively under ADR-078 D2, with `route::construct_pool` as the interior — so
   this decision's core half is the *rewiring*: the outer verbs move behind the
   door, and the FFI tier gets the same one-verb shape.
2. **The verdict carries the answer, not an echo.** `ConstructionVerdict::Registered
   { pool_id, pool: PoolHandle, identity }` or `Refused { family, refusal }`,
   where `refusal` is the shared vocabulary ADR-077/ADR-078 already own. The
   identity (family, variant, and the pool's token set) rides in the verdict so
   a consumer never re-derives it and never re-reads the registry to find the
   handle.
3. **The Python-facing request is a tagged `#[pyclass]`**, not a core type: core
   crates are pyo3-free by invariant (`degenbot-pools`' manifest says so, and no
   `cfg_attr(feature = "pyo3")` exists anywhere in `foundation/` or `engine/`).
   The pyclass exposes per-family constructors (`PoolConstructionRequest.v2(…)`,
   `.concentrated_liquidity(…)`, `.managed_v4(…)`), so the family tag is chosen at
   construction and a family's required parameters (`vault` for the weighted
   family) are unrepresentable when missing — replacing the `ValueError` the
   funnel's match arm raises today. One conversion function in the wrapper maps
   the pyclass onto `PoolConstruction`.
4. **The PyO3 `Bot` presents one construction verb.** The twelve family verbs
   become private functions of the wrapper; `construct_pool(request) ->
   ConstructionVerdict`. The identity-divergence assertions move inside the
   seam, where a mismatch is the seam's bug rather than a driver check.
5. **Post-construction finalize is the core arm's, or driven by the verdict.**
   The steps the core holds the facts for (V3 genesis seed, V4's resolve-then-
   build sequence, Curve base-pool recursion) move into the construction arm.
   The one remaining driver loop — building the ERC20 companions the companion's
   reads resolve against — iterates the verdict's token identity instead of
   branching on family, so it is data-driven, not family knowledge.

## Consequences

- The seam's construction interface is one verb and one value per outcome; a new
  family is a params variant plus a core arm, never a new pymethod.
- The sentinel tuple (`pool_id, "", "", address, variant`) and the second
  `get_pool` call disappear from every call site, and the driver's four
  identity-divergence raise sites with them.
- `degenbot-bot`'s `build_and_register_*` matrix stops being a surface: it is
  private mechanics under the one verb, which is what ADR-078 did for
  registration.
- The generated stubs move with the surface (`just gen-stubs`; the
  regenerate-and-diff gate fails otherwise), and `tests/test_ffi_boundary.py`'s
  verification is unaffected because no new flat-root symbol is introduced.
- Deliberately **not** decided here: making the companion getters
  registration-free (the deeper deletion, which changes when an RPC happens), and
  consolidating the two tick-word traits (`TickBootstrapRpc` /
  `TickWordFetcher`) into the one **Tick-word source** capability. Both are
  recorded in GLOSSARY and land on their own gates.
