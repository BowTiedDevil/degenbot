# ADR-075: The mirrored engine surfaces stay — the driver/stages mirror is accepted, policed by the drift gates

**Status: accepted (2026-10-16).** Recorded at the architecture-review grilling
so future reviews do not re-suggest collapsing the mirror.

## Context

Architecture review (2026-10) candidate 7 found two parallel ~35-50-method
public surfaces over the one arb engine — `EngineStages`
(`arb_engine/engine_stages.rs`) and `EngineDriver` (`arb_engine/driver.rs`)
hand-mirror ~15 method names (`register_path`, `deregister_path`,
`set_path_cap`, `path_count`, `path_dedups`, `path_info_for`, `apply_retune`,
`set_inline_simulator`, `latest_results`), with the driver delegating to
`self.stages()`. `arb_engine/lifecycle.rs` is a 273-line file of one-line
forwarders that fails the deletion test: deleting it concentrates nothing, but
removes an indirection every reader pays.

The duplication is a standing hazard. Two structural gates policed it
(`candidate2_enginestages_has_no_inherent_poke_twins`,
`candidate2_driver_surface_stays_off_the_engine`); both were **deleted** under
[ADR-087](ADR-087-deletion-is-the-retirement.md) D1/D5 as absence asserts on
collapsed `EngineStages` twins. The compiler and the one-impl-block gate are the
guards now.

## Decision

**Keep the mirror.** ADR-049 dissolved the engine's inherent surface into
machine-direct free functions behind the one-impl-block structural gate, and
ADR-050 pins `EngineDriver` as the one stage seam the block pump crosses. The
`EngineStages` surface is the machine's own vocabulary; the driver's
delegations are the seam's exposure of it. Collapsing the mirror would reopen
both ADRs for a payoff (one interface to learn) that does not clear the cost
of re-litigating the stage-seam shape the pump, the Python driver, and the
structural gates are built on.

The mirror is the accepted shape, not a smell to remove; review is what holds
it.

## Reopen condition

Revisit if drift between the two surfaces starts costing real incidents
repeatedly — that is the signal the mirror is failing in practice, not merely
existing.
