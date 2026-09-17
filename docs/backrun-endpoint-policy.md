# Backrun endpoint policy (MEVBlocker searcher feed)

> Status: operator-approved extension of the closed relay allowlist from
> `docs/autonomous-user-journey/RELAYS_AND_GUARDRAILS.md`, scoped to the
> same operator (MEVBlocker) documentedunwrap Surfaces used by the backrun
> pipeline tasks (epic `L5NSHX`). Extend only by operator sign-off.

## Searcher-facing endpoints (closed list)

| Endpoint | Role | Trust basis |
|---|---|---|
| `wss://searchers.mevblocker.io` | event feed (`mevblocker_partialPendingTransactions`) + `eth_sendBundle` / `eth_cancelBundle` bids | same operator as the user-journey relay allowlist; searcher docs (docs.mevblocker.io/how-to/searchers), no auth |
| `https://rpc.mevblocker.io/fast` | `eth_callMany` bundle simulation (exact post-target verification) | same operator; simulation-only (no tx lifecycle) |

Existing relay allowlist (unchanged): the provided RPC,
`https://rpc.flashbots.net?hint=hash`, `https://rpc.mevblocker.io/noreverts`,
`https://rpc.mevblocker.io/fullprivacy`.

## Trust model for bundles (S7 analogy)

A backrun bundle is `[target_hash, signed_backrun]`. The backrun executes
**only after** its target; if the backrun reverts the bundle is dropped by
the builder network (no gas burned). This is the same revert-shield
semantics the seed relays documented, applied at bundle granularity.

## Rules carried from the user-journey exercise

1. Chain id must be verified `1` on any endpoint before first use.
2. Contents of a bid: only the target hash (public) + our signed bytes. No
   key material in bundle payloads or logs.
3. Bids are revert-shielded; landed reverts across relay-path txs remain a
   disproof of the SLA (S7) — measure, never assume.
4. Endpoint widening beyond this list requires operator sign-off (widen =
   fail the trust exercise).
5. Observe-only soak precedes any bid path (zero-gate). Sim-only bidding
   (real operator-funded target txs) is a separately gated runbook step.
