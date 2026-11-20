# ADR-104: Typed event-hub intake — a permanent head claim, an engine-owned session stream, and no named-channel seam

**Status: accepted (implemented).** Records the deepening of
`degenbot-eventhub` and its consumer seams (`RB75A2` → `H7SXBF` → `CMY3KF`).
Basis: [ADR-087](ADR-087-deletion-is-the-retirement.md) (deletion is the complete
retirement), [ADR-093](ADR-093-session-lifecycle-and-consumer.md) D5 (the
ordered Session event stream and the Consumer claim, preserved unchanged),
[ADR-057](ADR-057-strategy-host.md) and
[ADR-058](ADR-058-strategy-crate-split.md) (the host and its plane, whose
pre-share registration phases this supersedes). The crate sources remain the
last word on what the code does today.

## Context

Phase B's hub was a policy-typed registry. One `HubEvent` union carried every
payload the process could publish, a `HubClass` keyed each source into a class
audit table, and `OverflowPolicy` declared each channel's strictness by name.
Registration and subscription returned unions, so every consumer re-matched the
class it had just declared: `degenbot-rpc`'s pending-tx stream carried five
separate `PolicyMismatch` arms, and three pending drains carried
"wrong variant" warning arms for a case the type system never ruled out.

Four facts were being re-proved at runtime instead of stated by the interface:

- **Payload class.** A look-alike union let a consumer of one class receive
  another class's payload and decide what to do at runtime.
- **Head ownership.** `LatestChannel` owned its own `watch::Sender` while
  `LatestReceiver` held an `Arc` to the channel, so a receiver kept its sender
  alive and `changed() -> Err` could not occur — the closure branches in
  `hosted_sources.rs` and `driver_loop.rs` were unreachable prose. `head.rs`
  accepted any `HubEvent` and projected only a number, discarding the metadata
  the poller and the fee policy needed.
- **Session-event ownership.** The engine's one ordered `SessionEvent` stream
  was a *named* hub channel: an either-end-immediately-unwrapped `NamedSender`,
  a hub-held `NamedReceiver` handed out once, and a second `claimed` latch on
  `PumpSession` beside the counter it drove. `ChannelRegistration` existed only
  so a strategy could mint such a channel inside the hub's exclusive `&mut`
  window, which made `StrategyHost::mint` fallible over a declaration the host
  validated but could not enforce.
- **Capacity.** A ring could be requested at capacity zero; `DropOldestChannel`
  counted a drop on an empty ring and then retained one event.

## Decision

### D1 — A payload's class is fixed by its typed channel

The hub no longer holds a union, a class audit table, or a policy vocabulary.
`Hub::open_pending_tx_ring(source, capacity)` returns
`(PendingTxPublisher, PendingTxView)` over `PendingTx` frames for exactly that
`PendingTxSource`; `Hub::claim_head_source`/`subscribe_head` carry `Head`. There
is no `HubEvent`, `HubClass`, `SourceHandle`, `Subscription`,
`OverflowPolicy`, `registered_classes()`, or `policy_of()`.

Overflow behavior is a property of the channel that has one, not a declared
label: a pending ring is bounded drop-oldest counting its evictions on the
publisher; the head clock is a latest-value watch. Wrong-class payloads and
missing/mistyped registrations are unrepresentable rather than runtime
`PolicyMismatch` arms, and `HubError` now carries its one remaining variant
(`AlreadyRegistered(PendingTxSource)`) in `hub.rs`.

### D2 — The head claim is permanent, routing is explicit, attachment is clean

`Unclaimed { sender in slot } -> Claimed { sender out }` happens once per hub.
A claim never lapses and a second claim is always `AlreadyClaimed`, even after
every publisher drops; `head_source_is_claimed()` stays true once claimed.
Shared state holds a `watch::Receiver` prototype and one last-seen `Instant`,
never a sender, so:

- a claimed clock's subscriptions close when the last publisher drops — the
  teardown-deterministic closure that earlier could not occur;
- a never-claimed clock closes only when the hub drops its slot sender;
- `subscribe_head` is infallible and starts each subscription at the clock's
  current value (clone the prototype, then `borrow_and_update()`), so a late
  subscriber sees the current head without replaying it as a new edge.

Publishers are routed once, where the hub is minted: a `PumpOwned` or hostless
session's boot claims once and hands the sender to `EngineDriver`, which passes
a clone to `BlockPump`; a `Watch`/`Poll` boot claims once and moves it into
`HostedSources`. `HostedSources::mint` accepts the post-claim
`Option<HostedClockBinding>`, so pump-owned-with-sender and hosted-without-sender
are both unconstructible.

Closure is a teardown and attachment fact, never a mid-session liveness
detector: production publishers live for the session, and transport liveness
remains the watchdog/staleness path. `last_header_age()` and the sender-side
`stale` were dead API and are deleted; `HeadSubscription::stale` is the one live
clock read.

### D3 — The session's ordered stream is engine-owned

`SessionEvent` is an engine-owned downstream type, so the engine owns its stream
outright. `EngineChannelHandles::new()` mints the raw `mpsc` pair and the
`EventHandoff`; assembly attaches the producer to `EngineStages` and files the
hand-off in the driver as `consumer: Mutex<Option<EventHandoff>>`.
`EngineDriver::claim_consumer` atomically takes it:

- the first claim yields the stream and its claim;
- only a second claim is `ClaimRefusal::AlreadyClaimed`, and a refused second
  hand-off leaves the first consumer's stream usable;
- the stream closes with its driver, because the driver owns the producer's
  attachment point and its single teardown closes the delivery channels.

`PumpSession.claimed`, `SESSION_EVENT_CHANNEL_NAME`, and the named-channel
wrappers, depth counters, and hub-held receivers are deleted. `StrategyHost::mint`
is now the infallible pairing `mint(registry, nonce, attachment) -> (Self,
HostHub<T>)`: the hub is fresh and the attachment is a value the caller already
owns, so there is no declaration to validate and no partially registered hub to
reach.

### D4 — Ring capacity is `NonZeroUsize`

Zero-capacity rings are unrepresentable in Rust rather than clamped. The Python
boundary takes `ring_capacity: int | None`: `None` selects the production
default, a positive `int` is the ring's capacity, and an explicit `0` raises
`ValueError` before anything spawns.

### D5 — ADR-093's ConsumerClaim protocol is preserved exactly

`ConsumerClaim` and `run(claim)` are unchanged: the claim stays non-cloneable,
minted only by the engine's hand-off, and consumed by value by `run`. What
changed is ownership of the hand-off, not the protocol that proves attachment.

### D6 — Retirement is deletion

`named.rs`, `policy.rs`, the class/policy registry, `ChannelRegistration`,
`ChannelRegistrationError`, `HostError::ChannelRegistration`, and every test that
existed only to read the audit vocabulary are deleted. Per
[ADR-087](ADR-087-deletion-is-the-retirement.md) this ships with no retirement
tripwires: no test asserts a retired name is absent, and no prose exists to keep
a deleted thing gone. The compiler, this ADR, review, and git history are the
guards.

### D7 — Superseded decisions

- **ADR-057 D3 is superseded.** Its "hub is hoisted; the driver attaches via a
  bound pair" existed to mint `&mut self` named channels inside one exclusive
  window. There are no named channels, so `HostHub<T>` no longer carries
  declaration-minted producers and `mint` no longer validates a declaration.
  The bound-pair guarantee (a driver cannot be attached to a hub it was not
  minted with) stands.
- **ADR-058 D5's "common pre-share channel registration" ledger entry is
  superseded** for the same reason: no strategy registers channels at all. A
  producer opens and owns its typed pending end ([`Hub::open_pending_tx_ring`])
  or claims the head source, and the engine owns its session stream outright, so
  there is no hub-held registry entry and no pre-share declaration.
- **ADR-055 D5's pool-log producer remains scheduled.** No eventhub `PoolEvent`
  struct or `open_pool_event_channel()` ships speculatively; the scheduled
  producer/consumer slice defines or relocates the type it actually needs, with
  its own tests. Git history and this ADR preserve the rationale.

## Consequences

- The hub is a typed intake surface: typed pending and head handles
  (`PendingTxPublisher`/`PendingTxView`, `HeadSender`/`HeadSubscription`) and no
  policy mirror for a consumer to re-match.
- Teardown is deterministic: a claimed clock's subscriptions close when its
  publishers drop, and the session stream closes with its driver.
- The engine's session stream has one owner and one hand-off; the host only
  pairs a fresh hub with the engine's ends.
- Full heads carry their metadata, so the fee policy can price from the header
  it already received instead of re-reading the base fee over RPC; number-only
  polled heads carry `None` metadata and fall back to the fee reader.
- Dead audit accessors, sender-side stale, and the metric-label vocabulary are
  gone with their only readers.

## Alternatives considered

- **Keep the union and make every consumer exhaustive.** Rejected: it preserves
  the wrong-class arm as a case the compiler cannot rule out, and forces each
  consumer to carry a mapping table from the class it declared back to the class
  it received.
- **Keep the named registry for a future engine-shaped payload.** Rejected: no
  second named channel exists, and ADR-055 D5's producer is the one candidate —
  it defines what it needs when it lands. Speculative vocabulary is the drag
  ADR-005/ADR-087 name.
- **Retain a root-held head publisher so closure never happens.** Rejected: it
  is what made `changed() -> Err` unreachable and left closure branches that
  could never run. Closure at teardown is the property worth having; liveness
  detection is the watchdog's job.
- **Let the hub own the session stream (a `Hub::open_session_events`).**
  Rejected: `SessionEvent` is engine vocabulary, and a hub method for it would
  either name that type in a transport-pure crate or reintroduce a generic
  named channel — the seam this ADR deletes.

## Migration

`Hub::register_source`/`subscribe` callers move to
`open_pending_tx_ring`/`claim_head_source`/`subscribe_head`; `HubEvent`
consumers read their typed payload. `StrategyHost::mint(registry, nonce,
registration)` becomes `mint(registry, nonce, attachment)` and no longer
returns a `Result`; `EngineChannelHandles::registration()` becomes
`EngineChannelHandles::new()`. Python callers pass `ring_capacity: int | None`
(`None` selects the production default, a positive value is the capacity, and an
explicit `0` raises `ValueError` before any feed is constructed). No
compatibility aliases or feature-gated parallel shapes ship.
