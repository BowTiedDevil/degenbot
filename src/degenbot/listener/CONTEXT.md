# Context — Event Listener

**LogListener**:
A pure Python dispatch registry mapping `(address, topic0)` → handler set. Receives raw log dicts via `dispatch(log)`, looks up handlers, calls them sequentially. Exact match only. No wildcards. Handlers are sync `Callable[[dict], None]`. Exceptions propagate. ~200ns per miss. Created by the user; not owned by Bot.
_Avoid_: subscription handler, event dispatcher, log dispatcher

**LOG_HANDLERS**:
A `ClassVar[dict[str, Callable]]` on pool types that maps event topic0 → decoder function. Each decoder takes a raw log dict and returns a closure that applies the decoded update to a pool instance. V2/V3/V4/Aerodrome pools declare their events; Curve pools have `LOG_HANDLERS = {}` (polling only). The user wires LOG_HANDLERS to a LogListener after `build_pool()`.
_Avoid_: event handlers, event decoders, pool handlers

## Relationships

- A **LogListener** dispatches to **LOG_HANDLERS**-derived handlers registered by the user
- A **Subscription** supplies raw log dicts to a **LogListener** via the user's consume loop
- **Bot.start_listening()** creates **Subscriptions** that the user feeds into a **LogListener**
- **Pools** declare their **LOG_HANDLERS**; the user wires them to a **LogListener** after `build_pool()`

## Resolved Ambiguities

### LogListener vs SubscriptionManager

**Ruling: Use **LogListener**. **SubscriptionManager** deleted.** One layer of primitives (Subscription + LogListener) instead of three. The LogListener is a dumb dispatch registry — it owns no subscriptions and has no async tasks. The user drives the consume loop explicitly.

- ✅ "Register the pool's LOG_HANDLERS with the LogListener"
- ✅ "Drain the subscription and dispatch logs through the listener"
- ❌ "The SubscriptionManager handles the events" (deleted — use LogListener)

### Handler ownership: pool vs builder vs user

**Ruling: Pool declares, user wires.** Pools expose `LOG_HANDLERS` with decoder functions. The user reads LOG_HANDLERS from the pool and registers with the LogListener. No `listener` kwarg on `build_pool()`. No builder involvement in wiring.

- ✅ "After building the pool, register its LOG_HANDLERS with the listener"
- ❌ "The builder auto-registers the pool with the listener"
- ❌ "The pool registers itself with the listener"
