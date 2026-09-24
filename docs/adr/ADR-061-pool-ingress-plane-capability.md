# ADR-061: Pool-state provisioning is a plane capability — pool ingress, sealed seeds, the strategy kit

**Status: accepted** (2026-09-23; closure verified 2026-09-24). Landed: a86825d6e + 5f8ed1dcb (D1
ingress facade + backrun cutover), 5b4ab69b6 (D2 sealed `TickMapSeed`,
`VerifyLevel`, `verify_ticks` facets), 1f8298af2 (D3 `StrategyKit` +
`STRATEGY_CELLS` pin), and 2e9fcf236c (D1 tail, one shared tick-map
precedence). The reviewed integration closure is c43386c98, 695e6f0c9,
b8036b373, f582ee1e5, and 90b2cce47. Basis: the mevblocker-backrun
`sequence_unavailable` autopsy (the production handoff in
`.scratch/backrun-v3-sequence-unavailable-handoff.md`), the affected-site
survey recorded in Context, and the submission-lane precedent. Predecessors:
ADR-058 (D5 amended by D4 below), ADR-057, ADR-055, ADR-054, ADR-022
(registration verify-lifecycle is core-owned), ADR-004 (the tick-map typed
boundary), ADR-059 (the declared==wired pinning pattern), ADR-012
(spec-bound pool admission), ADR-041, ADR-018.

## Context

The backrun funnel spent three days solving zero cycles. The proximate defect:
every chain anchored on (or hopping through) a wide/full-range depositor V3
pool rejected `SequenceUnavailable`, because the pool entered the per-frame
sandbox with a tick map covering only the current bitmap word ± 1 — a chain
RPC ladder staged under an implicit "depositors concentrate near price"
assumption that fails for exactly the depositor class the live feed surfaces.

The shared resolver (`bot_core/resolve/cl.rs`) emitted the rejection honestly.
The deception was upstream of it: the pool entered state through a different
module than the one whose data the resolver needed. The tree carried **four**
V3 tick-map staging implementations with two different precedence semantics:

| # | Module | Precedence | Coverage stamped | Consumer |
|---|---|---|---|---|
| 1 | `tick_assembly::assemble_v3_tick_map` (sync) | Db→Chain | DB hit → `Tracked` + snapshot block; chain → `Sparse` | settlement (sync registration arm) |
| 2 | `pool_builder::assemble_db_or_chain_v3/v4` | Db→Chain (async-native duplicate of #1, forked over the nested-`block_on` deadlock class) | same | settlement (`build_v3/v4`) |
| 3 | `BackrunSolver::admit_v3_full` inline 3-word ladder | Chain-only | always `Sparse` | backrun cold hops |
| 4 | `read_v3_tick_window` / `read_v3_view` | Chain-only | n/a (window merge) | backrun anchors |

Each copy carried its own costs, telemetry, and failure labels, so one defect
presented as three different symptoms. The complete per-tick maps the
settlement fleet already maintains (`TickMapDb::fetch_liquidity_map` →
`LiquidityMap`) were one method call away from #3 — `MarketContext` even held
the DB handle — and nothing routed the backrun lane to them.

Why the architecture allowed it. ADR-058 D5's subtraction rule ("a capability
slot with one consumer stays out of the plane") applied to **invariant-bearing
machinery**: pool-state provisioning had one *shaped* consumer (settlement via
`pool_builder`), so the plane carried no surface for it, and when the backrun
arm onboarded it never consumed that capability — it re-rolled a cheaper
private one. The six-slot vocabulary (source, infrastructure, calculation,
encoder, simulator, submission) existed as documentation only; the plane
contract was `trait Strategy { const NAME }` — identity, not behavior. The
add-a-strategy runbook promised "sandbox admission… provided" while leaving
the contents of that admission un-owned, so the cheapest private
implementation filled the vacuum.

Working precedent: the submission cluster. One process-global authority
(`NonceAuthority`), per-strategy lanes minted by composition (`NonceLane`),
signing reachable only through the lane's single entry, and a name-pin test
(`nonce_issuer_unified`) asserting no second path exists. Zero per-strategy
reimplementation. This ADR extends that shape to pool-state provisioning.

What must NOT be unified: the resolve/ per-family projections (the CL V3/V4
no-shared-constructor guardrail is deliberate), the executor grammar, solver
math. The rule this ADR draws: **invariant-bearing machinery gets one home;
family-shaped projections may fork with a recorded guardrail.**

## Decision

### D1 — Pool ingress: one module, one home

A new facade, `degenbot-bot::bot_core::pool_ingress` (a capability module in
the capability crate, re-exported on the strategy plane per ADR-058 D2),
composes the existing internals — identity probe (`pool_builder`), Db→Chain
tick-map assembly (`tick_assembly`), registration
(`planning::Workspace::register_with_state`), and the verify lifecycle
(`snapshot_verify::register_with_cl_buffers`, ADR-022) — behind one
deep interface:

```rust
pub struct PoolIngress { /* DbArm, Chain arm, memo, verifier, policy */ }

impl PoolIngress {
    pub async fn admit_v3_verified(
        &self, ws: &mut Workspace, params: IngressV3Params, head: u64,
    ) -> Result<u64, IngressDecline>;
    pub async fn admit_v3_replay(
        &self, ws: &mut Workspace, params: IngressV3Params,
        overlay: HashMap<i32, TickInfo>, head: u64,
    ) -> Result<u64, IngressDecline>;
    pub async fn admit_v4_verified(
        &self, ws: &mut Workspace, params: IngressV4Params, head: u64,
    ) -> Result<u64, IngressDecline>;
    pub async fn admit_v4_replay(
        &self, ws: &mut Workspace, params: IngressV4Params,
        overlay: HashMap<i32, TickInfo>, head: u64,
    ) -> Result<u64, IngressDecline>;
}
```

Implementations #1 and #2 collapse into one async-native arm (the sync arm
is deleted; the nested-`block_on` deadlock class closes by construction,
via `ConstructionIo`). Implementations #3 and #4 are **deleted as seams**:
the backrun cold-hop admission and the touched-anchor window merge become
ingress consumers. The backrun handoff's primary open item — Db-first tick-map
precedence at both admission seams, per-block memoized fetch — is this
cutover, not a standalone fetch; its proof is the inverted
`sequence_deficit_probe` asserts (the harness's wide map equals what
production stages; `dfs_evaluated` becomes 1).

### D2 — Sealed seed provenance

`planning::ExplicitPoolState` carries a non-exhaustive
`TickMapSeed { ticks, bitmaps, coverage, seed_block, source_block, identity,
source }`. The `Db` and `Chain` constructors are crate-private to `bot_core`
and minted by `PoolIngress`; the Journal constructor is retained only for
capability tests. A strategy cannot fabricate a sparse ladder or bypass the
workspace registration seam: replay facts enter through
`PoolIngress::admit_v3_replay` / `admit_v4_replay`. Enforcement doubles: the
type system seals construction, and a name-pin test (the `nonce_issuer_unified`
pattern) asserts no tick-word fetch or seed construction lives on a
registration path in `degenbot-strategy`. Freshness stamps (the two-stamp
liquidity clock) ride in the seed, joining coverage and provenance at the one
boundary.

### D3 — The strategy kit: composition by struct, variance in values

The boot resolves a `StrategyKit` once per strategy and hands it to the spawn
factory; strategies compose cells, never constructors:

```rust
pub struct StrategyKit {
    pub provision: ProvisionCell,      // PoolIngress + VerifyLevel
    pub discovery: Option<DiscoveryHandles>,
}
```

The landed composition deliberately has no `simulate`, `submit`, or `react`
cell: simulation is loop-local, the submission lane is host-owned, and
reaction feeds are registered at drive time. Those are not placeholder
capabilities; they remain with their existing owners.

Fields are concrete where variation is hypothetical and `dyn` only at real
adapter seams (`TickMapDb`); `None` keeps its "lane shut" meaning, declared in
the facet. `MarketContext` is recomposed as views over kit fields with no
behavioral change — closing the structural vacancy of "holds the DB handle
cannot provision with it". A new strategy family's need for genuinely new
shared machinery (a liquidation lane's `account_ingress`, say) is added to the
kit as a plane decision in one reviewable diff; the compile-level churn across
existing strategies **is the audit checkpoint**, not a cost to optimize away.

**Rejected alternative — defaulted-trait composites** (`trait
StrategyComponents { fn ingress(&self) -> Option<&dyn PoolIngress> { None }
… }`). Recorded, not merely dismissed: defaults re-open the diagnosed failure
class — a strategy may answer `ingress()` with something it built itself,
which is precisely the private-implementation escape hatch this ADR exists to
close; the plane loses its static "which strategies compose what" table;
policy (verification level, freshness stance) has no single home and would
multiply per impl; and with one real adapter today it is the hypothetical-seam
shape. Addition-without-churn is the wrong optimization here: a design where
adding shared machinery is silent is a design where inventing it privately is
also silent.

### D4 — Invariant-bearing machinery is plane-level at consumer #1

Amends ADR-058 D5. Subtraction still governs convenience utilities, but
machinery carrying a soundness or performance invariant — state precedence,
coverage semantics, freshness clocks, nonce contiguity — is single-home
**when it first exists**, with exactly one consumer if that is all there is.
Later strategies consume by construction (D2's seal), pinned by a
declared==wired capability-table row per arm (the ADR-059 pattern extended to
state provisioning). The one-consumer-hides-the-invariant mistake is the
recorded cause of the autopsy in Context.

### D5 — The pool ledger: two adapters of one freshness seam

Freshness stance is a real seam with **two existing adapters**, hence one
trait, not two modules: `PoolLedger`, with the settlement pump's persistent
maintained stance and the per-frame stance's per-block memoized view (the
`BotStateDb` storage-memo shape; bounded by the pool's own map size). Both
funnel through the ingress (D1); the per-frame stance consults the ledger, it
does not re-derive a third freshness model.

**Implementation note:** the landed code expresses this seam as `PoolIngress`'s
`DbArm` plus its per-block `MapMemo`; no separate public `PoolLedger` type is
required by the current adapters. The invariant and locality decision remain
the same: callers cannot introduce a third freshness model beside ingress.

### D6 — Reaction-kind dispatch stays strategy-owned

Settled-block (`StageMachine`, payload-settlement-shaped) and
pending-transaction (`PendingTxReaction`) remain per-kind compositions; the
settlement pump's shape stays on the special-case ledger, and its
generalization remains ADR-018 on-demand work pulled by a second
settled-block strategy. Ingress deliberately does not vary by reaction kind.

### D7 — Verification is a policy value, defaulting on

Chain-sample verification gates through a typed knob on the provisioning cell:

* `Strict` — sample-verify every admission (new-lane development stance).
* `Bootstrap` — verify first admission per pool per process, memoed;
  production default. Feasible in the per-frame stance because the memo is
  process-scoped; it is the pump's verified-once discipline inherited by both
  arms.
* `Off` — operator-declared confidence; emits a loud boot journal entry.

One declaration site per strategy facet (`strategy.<name>.verify_ticks`),
env-spelled like the gas-floor knob, docs regenerated. **Integrity is distinct
from sampling and unconditional**: the Tracked self-contradiction abort
(`liquidity_map_to_tick_info`) and the two-stamp clock never gate on this
knob — `Off` never means "proceed on a self-contradictory map".

## Consequences

- The autopsy's bug class becomes unwritable: the backrun lane's first cold V3
  admission routes through `ingress.admit()` — Db-first, `Tracked`-stamped,
  verify-attached — by construction; the four-implementation table collapses
  to one module with two ledger adapters.
- Adding a strategy family is: config facet + reaction-kind impl + kit
  composition + pins. `docs/architecture/adding-a-strategy.md` §0's gloss is
  replaced by a state-provisioning section, and `strategy-seams.md`'s
  substrate table carries the ingress/kit rows.
- `ExplicitPoolState` consumers use the seeded provenance boundary; the V4
  anchor admission now follows the V3 cutover on the same ingress seam.
- Kit churn is accepted: a new shared capability edits the kit, the one resolve
  site, and the capability table in one diff.
- The `sequence_deficit_probe`, `family_capabilities` declared==wired table,
  and the name-pin test are the enforcement pins; `StrategyKit`
  construction-site locality studies are out of scope until strategy family
  four exists.

## Acceptance and closure (2026-09-24)

The accepted sequence is closed against the implementation rather than merely
re-described here. `pool_ingress::PoolIngress` is now the deep interface and
module boundary for V3 and V4 production pool admission: it owns Db→Chain
precedence, per-block locality, backfill, replay-overlay merge, sealed
`TickMapSeed` provenance, `VerifyLevel` policy, and registration. The strategy
plane consumes that interface; it does not open a second staging or workspace
registration seam. The adapter boundary is deliberate: contract-facing full
liquidity-map verification lives in `degenbot-rpc::liquidity_verifier`, while
bot and updater modules retain only their lifecycle, error, and rollback policy.

For V4, the verification target is the canonical `PoolManager` plus `PoolId`.
`StateView` remains optional scalar/bootstrap configuration and is not the
full-map target. The former private V4 tick-window seam and duplicate
full-map verification implementations have no production caller; V3 staging
through the raw public `v3_tick_map` shape is likewise no longer an ingress
entry. The acceptance search therefore finds one shared verifier and one
`PoolIngress` ownership path, with `VerifyLevel` and sealed seeds preserving the
integrity boundary.

The adjacent locality decisions reviewed in the same integration sequence are
also closed: discovered V2 fees flow through the typed `V2FeePair`/`V2Fees`
projection into the executable hop, and the typed `Envelope` verdict is carried
through `SolveOutcome` to the planning seam instead of being re-derived from
walk statistics. These are locality improvements, not new family or solver
interfaces.

## Related

- **ADR-058** — amended by D4 (invariant-bearing machinery is plane-level at
  consumer #1); the rejection of defaulted-trait composites elaborates its D3.
- **ADR-057** — the host the kit resolves under; kit is composition surface,
  distinct from the host's selection surface (`StrategyName`).
- **ADR-022** — registration verify-lifecycle is core-owned; D7 moves its
  activation into a typed policy without changing ownership.
- **ADR-055, ADR-054** — the pending-tx and frame-evidence seams the ingress
  consumership respects.
- **ADR-012** — spec-bound admission contract; `IngressDecline` joins its
  refusal taxonomy.
- **ADR-004** — the tick-map typed boundary provenance stamps extend.
- **ADR-059** — the declared==wired pinning pattern D4 extends to state
  provisioning.
- **ADR-018** — the on-demand dispatch generalization D6 defers to.
- `docs/architecture/strategy-seams.md`,
  `docs/architecture/adding-a-strategy.md` — the substrate map and runbook,
  updated at the accepted cutover.
- `.scratch/backrun-v3-sequence-unavailable-handoff.md` — the production
  autopsy this ADR's Context rests on; its D1/D2 cutover is closed by the
  reviewed sequence recorded above.
