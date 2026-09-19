# ADR-056: Retire the gated serving seam

## Status

Accepted (Phase B/B4).

## Context

`BotStateDb::storage_ref` carried an env-gated seam that returned the engine's packed
typed pool state for tracked scalar slots instead of the RPC fallback's value. It was
built to test the "stale engine state causes `CurrencyNotSettled`" hypothesis ("path
A"). Mainnet data refuted the premise: V3 hops matched the actual swap output exactly
(the engine state was correct) while only the V4 swap diverged by 1-8 units — a solver
calc rounding divergence, not stale state. The gate was default-off, so the seam was
already run-dead in production.

## Decision

The serving seam, its env gate, and its config-schema key retire; `storage_ref`
returns the RPC fallback's value for every read, exactly as production ran.

The seam's premise that the anchor's always-live payload was *only* pool membership
was incomplete: the always-on divergence observer is a second consumer of the anchor's
scalar words. So the sim DB depends on a `SimAnchorOracle` seam rather than the
concrete anchor — the boot-snapshot `RouteRegistry` answers membership (and offers no
words) for the sidecar, while the engine's `SimAnchorState` snapshot still supplies
the observation words. Pool membership migrates to the registry where one exists; the
observation path is preserved unchanged.

## Consequences

- A host without a registry answers membership from its own world-view; the sidecar's
  leaked empty anchor (the affordance the deleted seam required) disappears.
- The serving env key and its inventory entry are removed.
- The recorded refutation stands: tracked scalar engine state matches the RPC at sim
  time; the V4 divergence is a solver rounding artifact, not state staleness.
