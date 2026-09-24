# ADR-044: Fleet intake liveness — BackstopTick + PostureEdge complete the host transition relation

**Status: accepted** (2026-09-12, ergo epic `TB4QGX`; tasks `3RKEV6` -> `5L4FSL` -> `LMNDZ4` -> `PEVBO4` -> `VXP27K` -> `PPIJJO` -> `XATYL5`).
Ratified in a two-peer workshop (`peer_a` glm-5.3, `peer_b` kimi-k3, both FINAL
ACCEPT), stress-tested by an adversarial review session across the task chain,
and mechanized as properties in `PPIJJO`.

## Context

The second live settlement-bot run deadlocked its registration crawl at
**9,955 registered paths**. Under a `Cordoned` posture the fleet intake backlog
held units; the crawl driver's bounded submission window
(`REG_INTAKE_WINDOW = 32`) blocked on a parked receipt; the host — which only
pumped on a message — parked in `rx.recv()` forever. Safety (never-drop) held;
liveness failed. Evidence: `degenbot_engine_registered_paths` frozen at 9955,
registration skips frozen, no `[build_paths] Path discovery complete`, every
`work-fleet-pool*` thread in futex, Python main in `ep_poll`.

## Root cause: an incomplete transition relation

`HostMsg { Enqueue, SeatDone }` was the **entire input alphabet**, while posture
is mutated by the block-pump thread and only *sampled* as a guard. That leaves

```
(backlog != empty, posture Nominal, in-flight = 0, no pending messages)
```

a reachable **terminal** state: no symbol in the alphabet can leave it. The
retired wake-discipline note argued that a continuing flood of submissions
would self-wake the backlog, but that is a **client-fairness assumption**, and
the driver's bounded window falsifies it — the client legitimately stops
submitting while its window is full.

### Guard vs input (the analysis that decided the fix)

Posture is a *live guard variable* mutated by another thread. The host samples
it only when it runs; a transition relation over `{Enqueue, SeatDone}` cannot
observe a guard change that produces no message. Two families of fix exist:

1. **Mirror the guard as an input** (push posture values into the host).
2. **Add a wake source that forces a re-read of the live guard.**

(1) duplicates state and invites divergence (a stale mirror is a new bug class);
it was rejected. (2) requires no value transfer and cannot go stale — only a
*nudge* to re-run the pump. The ratified design takes (2), with (1)'s
responsibility falling on the pure admission predicate instead.

## Decision (ratified design)

1. **Posture stays a live guard; no host mirror.** The `PostureHeld` hand-back
   remains the TOCTOU backstop.
2. **Two inputs added:** `BackstopTick` (mandatory liveness) and `PostureEdge`
   (untrusted, seq-stamped hint — never a value).
3. **`recv_timeout` is armed iff the backlog is non-empty**; on timeout the
   host runs `pump()`. An idle host blocks indefinitely (no polling cost).
4. **`PostureEdge` delivery** is a bot-side, block-pump-fed waker
   (`arb_engine::fleet_wake`): no new thread, no layering inversion, plus a
   feeder-site contract (every owner-mutating site emits on a non-`Held`
   change).
5. **Progress FSM: `Idle` / `Backed` / `Faulted` / `Closed`**, with admission
   expressed as a **pure predicate** over `(queue_len, queue_cap,
   posture_admits)`.
6. **`Faulted` keys on the typed `LaneDeath` latch** (never a duration
   heuristic); it drains held receipts with **typed terminal receipts**, while
   in-flight units complete naturally.
7. **K consecutive admit-but-no-progress ticks -> loud `discipline.fail`**; K is
   a typed config constant.
8. **Properties:** S = unit conservation
   (`submitted == in-flight + queued + backlog + resolved-receipts`);
   L = every reachable `Backed` state receives a `BackstopTick` within Delta,
   re-evaluating every mutable guard => `AF(drained | Faulted | loud-abort)`.

The observable counterpart is the `degenbot_fleet_intake_backlog{role=...}`
gauge: a held backlog is visible and must be driven to zero by the backstop.

## Consequences

- A held backlog is drained at the latest one backstop interval after a guard
  change, even with zero client messages.
- A stuck intake with capacity is loud within K ticks rather than silent.
- A dead lane resolves every waiting receipt terminally (typed error) and
  latches sticky until a fresh process — no parked driver, no silent loss.
- Hosts that persist no pyo3-owned receipts (`sim`/`solve`) never enter
  `Faulted`; held work there is not applicable.

## Alternatives considered (rejected)

- **Floor-not-hold** (let one unit trickle through a cordon): changes the
  fleet-wide duty-window intent, and a floor-1 trickle keeps the duty window
  dirty so exit hysteresis may never fire.
- **Per-host bridge thread**: a new thread per host with no never-drop gain,
  and a lifetime/supervision surface the fleet does not need.
- **Owner-side sender registry**: the posture owner would have to know host
  channels — a layering inversion, and a registration surface that must be
  kept in sync with host lifetimes.
- **Bare timed `recv` (unconditional polling)**: an idle host would wake and
  re-run the pump forever; the conditional arming (`backlog != empty`) keeps
  the backstop a liveness floor instead of a busy-spin.
- **Posture mirror in the host**: duplicates a guard and invites stale-value
  divergence; the pure predicate plus a hint is strictly simpler.
- **Respawn/retry of parked units**: breaks at-most-once and the never-execute
  semantics of terminal receipts.

## References

- `rust/crates/engine/degenbot-bot/src/arb_engine/seat_host.rs` — `HostPump`
  (`run`/`apply_host_msg`/`pump`), backlog, wake discipline, properties.
- `rust/crates/engine/degenbot-workers/src/posture.rs` — `PostureOwner`/
  `PostureWatch`, `LaneDeath` latch, feeder-site contract.
- `rust/crates/engine/degenbot-workers/src/dispatcher.rs` — `FleetHost`,
  `try_enqueue` (`PostureHeld`), `posture_admits_role`.
- `rust/crates/engine/degenbot-bot/src/arb_engine/fleet_wake.rs` — bot-side waker.
- `rust/crates/engine/degenbot-bot/src/bot_core/block_pump.rs` — per-header posture
  feed.
- `src/degenbot/runner/build_paths.py` — `run_registration` window,
  `_consume` operator path.
- `docs/spike-intake-terminal-receipts.md` — the terminal-receipt spike.
