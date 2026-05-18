# Plan 051: Extract BuilderContext from Bot Constructor Wiring

**Status: COMPLETE**

## Implementation Notes

- `BuilderContext` is a frozen dataclass in `src/degenbot/builders/context.py`
- All 7 builders accept `ctx: BuilderContext` and unpack into existing attributes (no property indirection)
- `Erc20Builder` keeps its standalone constructor (it's a leaf dependency used *by* the context)
- V3/V4 builders assert `ctx.managed_pools is not None`
- Each variant builder (AerodromeV2Builder, CamelotBuilder) explicitly defines `__init__` calling `super().__init__(ctx)`

## Overview

Extract a `BuilderContext` frozen dataclass holding the shared dependencies that every
builder receives (connections, db, pools, tokens, erc20_builder, optionally managed_pools).
Bot's constructor currently wires 7 builders with 35 lines of near-identical dependency
passing. With `BuilderContext`, Bot creates one context object and passes it to all
builders. Adding a new builder type requires only the builder class and a
`register_builder` call — zero wiring lines.

## Files Involved

**Primary:**
- `src/degenbot/builders/v2_builder_base.py` — accept `BuilderContext` instead of 5–6 individual parameters
- `src/degenbot/builders/v2_pool_builder.py` — same
- `src/degenbot/builders/v3_pool_builder.py` — same
- `src/degenbot/builders/v4_pool_builder.py` — same
- `src/degenbot/builders/aerodrome_v2_builder.py` — same
- `src/degenbot/builders/camelot_builder.py` — same
- `src/degenbot/builders/curve_pool_builder.py` — same
- `src/degenbot/builders/erc20_builder.py` — same (creates the `BuilderContext`; receives a subset)
- `src/degenbot/bot.py` — replace 7 builder constructions with `BuilderContext` + one-liners

**Secondary:**
- `src/degenbot/builders/protocol.py` — no change needed (PoolBuilder protocol is independent)
- `src/degenbot/async_bot.py` — will benefit when Plan 048 adds async builder wiring

**Tests:**
- `tests/builders/` — update builder construction to use `BuilderContext`
- `tests/test_bot.py` — verify no regression

## Problem

Bot's `__init__` constructs 7 builders with near-identical wiring:

```python
self._erc20_builder = Erc20Builder(
    connections=self.connections, db=self.db, tokens=self.tokens
)
self._v2_builder = V2PoolBuilder(
    connections=self.connections, db=self.db, pools=self.pools,
    tokens=self.tokens, erc20_builder=self._erc20_builder,
)
self._aerodrome_v2_builder = AerodromeV2Builder(
    connections=self.connections, db=self.db, pools=self.pools,
    tokens=self.tokens, erc20_builder=self._erc20_builder,
)
self._camelot_builder = CamelotBuilder(
    connections=self.connections, db=self.db, pools=self.pools,
    tokens=self.tokens, erc20_builder=self._erc20_builder,
)
self._v3_builder = V3PoolBuilder(
    connections=self.connections, db=self.db, pools=self.pools,
    tokens=self.tokens, managed_pools=self.managed_pools,
    erc20_builder=self._erc20_builder,
)
self._v4_builder = V4PoolBuilder(
    connections=self.connections, db=self.db, pools=self.pools,
    tokens=self.tokens, managed_pools=self.managed_pools,
    erc20_builder=self._erc20_builder,
)
self._curve_builder = CurvePoolBuilder(
    connections=self.connections, db=self.db, pools=self.pools,
    tokens=self.tokens, erc20_builder=self._erc20_builder,
)
```

Every builder takes `(connections, db, pools, tokens, erc20_builder)`. V3/V4 add
`managed_pools`. Every builder independently stores `self._connections`, `self._db`,
`self._pools`, `self._tokens`, `self._erc20_builder`. Adding a new builder requires
adding another 5-line construction call and another `register_builder` line.

The deletion test: if you removed the constructor parameter list from one builder, the
same 5 parameters would need to come from somewhere (method params, global state, or
context). The builders legitimately need these dependencies — the friction is in the
repetition, not the dependencies themselves.

## Solution

### Step 1: Define `BuilderContext`

```python
# src/degenbot/builders/context.py

from __future__ import annotations

import dataclasses

from degenbot.connection.connection_manager import ConnectionManager
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry

if TYPE_CHECKING:
    from degenbot.builders.erc20_builder import Erc20Builder


@dataclasses.dataclass(slots=True, frozen=True)
class BuilderContext:
    """Shared dependencies for all pool builders.

    Bot creates one BuilderContext and passes it to all builders.
    Each builder extracts what it needs. Adding a new builder requires
    zero additional wiring in Bot — just create the builder with the
    existing context.
    """

    connections: ConnectionManager
    db: DatabaseSessionManager
    pools: PoolRegistry
    tokens: TokenRegistry
    erc20_builder: Erc20Builder
    managed_pools: ManagedPoolRegistry | None = None
```

Design decisions:
- **Frozen dataclass.** Immutable — builders can't accidentally reassign dependencies.
  Attributes are read-only after construction.
- **`managed_pools` is optional.** Only V3/V4 builders need it. `None` by default.
  Builders that need it check `ctx.managed_pools is not None` or receive it in their
  constructor with a required parameter (enforced at construction time).
- **`erc20_builder` is included.** It's a builder, but also a dependency of all other
  builders. The `Erc20Builder` itself takes a subset (connections, db, tokens) — it
  doesn't require `pools` or `managed_pools` or itself. So `Erc20Builder` uses a
  different context or receives its dependencies directly (see Step 4).

### Step 2: Update base classes and builders to accept `BuilderContext`

```python
# Before (V2BuilderBase):
class V2BuilderBase:
    def __init__(
        self,
        *,
        connections: ConnectionManager,
        db: DatabaseSessionManager,
        pools: PoolRegistry,
        tokens: TokenRegistry,
        erc20_builder: Erc20Builder,
    ) -> None:
        self._connections = connections
        self._db = db
        self._pools = pools
        self._tokens = tokens
        self._erc20_builder = erc20_builder

# After:
class V2BuilderBase:
    def __init__(self, ctx: BuilderContext) -> None:
        self._ctx = ctx

    @property
    def _connections(self) -> ConnectionManager:
        return self._ctx.connections

    @property
    def _db(self) -> DatabaseSessionManager:
        return self._ctx.db

    @property
    def _pools(self) -> PoolRegistry:
        return self._ctx.pools

    @property
    def _tokens(self) -> TokenRegistry:
        return self._ctx.tokens

    @property
    def _erc20_builder(self) -> Erc20Builder:
        return self._ctx.erc20_builder
```

Wait — replacing direct attributes with properties accessed through `self._ctx` is more
indirection for every internal access. Every `self._connections` becomes
`self._ctx.connections`. This touches every line in every builder.

**Better approach**: Keep the direct attributes (no properties), but populate them from
`ctx`:

```python
class V2BuilderBase:
    def __init__(self, ctx: BuilderContext) -> None:
        self._connections = ctx.connections
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder
```

This preserves the current access pattern (`self._connections`, etc.) while simplifying
the constructor signature from 5 parameters to 1. The builder internals don't change at
all. The `BuilderContext` is just a "parameter pack" — it simplifies the calling site
(Bot), not the implementation site (builders).

### Step 3: Handle V3/V4 builders that need `managed_pools`

```python
class V3PoolBuilder:
    def __init__(self, ctx: BuilderContext) -> None:
        self._connections = ctx.connections
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._managed_pools = ctx.managed_pools  # type: ignore[attr-defined] — guaranteed non-None by Bot
        self._erc20_builder = ctx.erc20_builder
```

Or, since `managed_pools` is `ManagedPoolRegistry | None` on `BuilderContext`, V3/V4
builders can assert:

```python
assert ctx.managed_pools is not None, "V3PoolBuilder requires managed_pools in BuilderContext"
self._managed_pools = ctx.managed_pools
```

This is acceptable — Bot always provides `managed_pools`. The assert catches
misconstruction, not runtime conditions.

### Step 4: Handle `Erc20Builder` bootstrapping

`Erc20Builder` doesn't need `pools`, `managed_pools`, or itself. It takes
`(connections, db, tokens)`. Two options:

**Option A**: `Erc20Builder` also takes `BuilderContext` but only uses 3 of the fields.
Simple, consistent. Unused fields are ignored.

**Option B**: `Erc20Builder` keeps its current 3-parameter constructor. It's not a pool
builder — it's a different kind of builder. The `BuilderContext` is for pool builders.

**Decision: Option A.** Consistency beats purity. Every builder takes `BuilderContext`.
`Erc20Builder` ignores `pools`, `managed_pools`, and `erc20_builder` (which is itself).
The unused fields are inert — they don't cause errors, they just aren't accessed.

But `Erc20Builder` is constructed *before* `BuilderContext` exists (it's a field of
`BuilderContext`). So:

```python
# Bot.__init__:
self._erc20_builder = Erc20Builder(
    connections=self.connections, db=self.db, tokens=self.tokens
)
ctx = BuilderContext(
    connections=self.connections,
    db=self.db,
    pools=self.pools,
    tokens=self.tokens,
    erc20_builder=self._erc20_builder,
    managed_pools=self.managed_pools,
)
self._v2_builder = V2PoolBuilder(ctx)
# ...
```

`Erc20Builder` keeps its current constructor. It's the one builder that doesn't use
`BuilderContext` because it's a dependency *of* the context. This is the correct
layering: `Erc20Builder` is a leaf, `BuilderContext` is a composite.

### Step 5: Update Bot's `__init__`

```python
# Before: 35 lines of builder wiring
# After:

self._erc20_builder = Erc20Builder(
    connections=self.connections, db=self.db, tokens=self.tokens
)
ctx = BuilderContext(
    connections=self.connections,
    db=self.db,
    pools=self.pools,
    tokens=self.tokens,
    erc20_builder=self._erc20_builder,
    managed_pools=self.managed_pools,
)
self._v2_builder = V2PoolBuilder(ctx)
self._v3_builder = V3PoolBuilder(ctx)
self._v4_builder = V4PoolBuilder(ctx)
self._aerodrome_v2_builder = AerodromeV2Builder(ctx)
self._camelot_builder = CamelotBuilder(ctx)
self._curve_builder = CurvePoolBuilder(ctx)
```

From 35 lines to 13. Each builder constructor is one line. No more spelling out
5–6 dependencies per builder.

### Step 6: Future state — adding a new builder

```python
# To add a BalancerWeightedPool:
from degenbot.balancer import BalancerPoolBuilder

self._balancer_builder = BalancerPoolBuilder(ctx)
self.register_builder(BalancerWeightedPool, self._balancer_builder)
```

Two lines in Bot. No new wiring. The `BuilderContext` already has everything.

## Implementation Order

### Phase 1: Define `BuilderContext` (additive)

1. Create `src/degenbot/builders/context.py` with `BuilderContext` frozen dataclass
2. Run tests — zero regression (nobody uses it yet)

### Phase 2: Migrate builders one at a time (each is an independent commit)

3. `V2BuilderBase.__init__(ctx: BuilderContext)` — unpack ctx into existing attributes
4. `V2PoolBuilder` — inherits from V2BuilderBase, no constructor change needed
5. `AerodromeV2Builder` — same as V2PoolBuilder
6. `CamelotBuilder` — same
7. `V3PoolBuilder.__init__(ctx: BuilderContext)` — unpack + assert managed_pools
8. `V4PoolBuilder.__init__(ctx: BuilderContext)` — same
9. `CurvePoolBuilder.__init__(ctx: BuilderContext)` — unpack
10. After each builder migration, run tests — zero regression

### Phase 3: Update Bot

11. Construct `BuilderContext` in Bot's `__init__`
12. Pass `ctx` to all builder constructors
13. Remove individual parameter passing
14. Run all tests

### Phase 4: Clean up

15. Remove `ConnectionManager`, `DatabaseSessionManager`, `PoolRegistry`, `TokenRegistry`,
    `Erc20Builder` from individual builder constructor type annotations (they still
    reference them via `ctx.XXX` type)
16. Run `ruff`, `mypy`, full test suite
17. Update any relevant tests that construct builders directly

## Benefits

- **Bot's constructor shrinks by ~22 lines.** 7 multi-line builder constructions → 7
  one-liners + one `BuilderContext`.
- **Adding a builder requires 2 lines, not 7.** One construction, one registration.
- **No more spelling out 5–6 deps per builder.** One `ctx` parameter.
- **`BuilderContext` documents the shared dependency surface.** Anyone reading the
  codebase sees at a glance what builders have access to.
- **Future `AsyncBuilderContext`.** When Plan 048 adds async builders, `AsyncBot` creates
  an `AsyncBuilderContext` (same shape but with `AsyncConnectionManager`). The builder
  constructors accept either.
- **Testing.** Builder tests construct a `BuilderContext` with fakes — one object instead
  of 5–6 separate fakes. Less boilerplate in test setup.

## Risks

- **Indirection.** `V2PoolBuilder(ctx)` hides which dependencies the builder uses. With
  the explicit parameter list, a reader sees `connections, db, pools, tokens, erc20_builder`
  at a glance. With `ctx`, they must read the builder's `__init__` body. Mitigated by
  the unpacking pattern (each builder still assigns `self._connections = ctx.connections`,
  etc. — the mapping is visible).
- **`Erc20Builder` bootstrapping.** `Erc20Builder` can't use `BuilderContext` because it's
  a field of `BuilderContext`. This means one builder is always "special." Acceptable —
  it's a leaf dependency, not a peer.
- **`managed_pools` optionality.** V3/V4 builders assert `ctx.managed_pools is not None`.
  If someone constructs a `BuilderContext` without `managed_pools` and passes it to V3,
  they get a runtime error. This is correct behavior — V3 requires managed pools.
  The assert makes the requirement explicit.
- **Frozen dataclass with mutable fields.** `BuilderContext` is frozen, but its fields
  (`ConnectionManager`, `PoolRegistry`, etc.) are mutable. The freeze prevents
  reassignment of the references, not mutation of the referenced objects. This is the
  correct semantics — builders share the same registries and connections.

## Relationship to Other Plans

- **Plan 001** (Extract Pool Builders): Created the builder classes that this plan
  simplifies. The builder extraction is the foundation; `BuilderContext` is the
  next deepening step.
- **Plan 035** (Builder Protocol): Defined the `PoolBuilder` protocol that builders
  implement. `BuilderContext` is orthogonal — it's about *construction*, not *behavior*.
- **Plan 048** (Async Builder Shared): Will benefit from `BuilderContext` — AsyncBot
  creates an `AsyncBuilderContext` and passes it to async builders. The same simplification
  (one context instead of 5–6 parameters) applies.
- **Plan 043** (Extract V2 Variant Builders): Extracted Aerodrome and Camelot builders
  from the monolithic V2PoolBuilder. `BuilderContext` makes those extractions cleaner
  — each variant builder is a one-line construction.
