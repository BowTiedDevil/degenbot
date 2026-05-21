# Plan 067: Replace `build_pool()` Kwargs Tunnel with BuildPoolRequest

## Overview

Introduce a `BuildPoolRequest` frozen dataclass that carries all optional parameters for pool construction, replacing the `dispatch_kwargs` / `v4_kwargs` dictionary construction in Bot and AsyncBot. Both Bot and AsyncBot construct one `BuildPoolRequest` and pass it via `_dispatch_build()` to builders, replacing `**kwargs` forwarding with a typed seam. Builders are migrated one-shot: individual optional kwargs and `**kwargs` are replaced by `request: BuildPoolRequest` in a single step.

## Problem

### Deletion test

If you delete the `dispatch_kwargs` and `v4_kwargs` construction logic (~40 lines across both files that manually filter non-None kwargs into dicts), the same parameter-passing would need to happen some other way — the data has to reach the builder. But the current approach is a kwargs tunnel: `build_pool()` receives 12 optional kwargs → filters into dicts → forwards as `**kwargs` → builder receives and interprets them. This is a shallow pass-through that adds no behavior, only indirection.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|-------------|
| 12 optional kwargs on `build_pool()` | `bot.py:214–234`, `async_bot.py:163–183` | Callers must know which kwargs are relevant; IDE autocompletion shows all 12 regardless of pool type |
| Manual `dispatch_kwargs` construction | `bot.py:311–324`, `async_bot.py:251–264` | 14 lines of `if x is not None: dispatch_kwargs["x"] = x` — fragile, must be kept in sync between Bot and AsyncBot |
| V4 fast-path duplicates the filtering | `bot.py:248–269`, `async_bot.py:197–218` | 22 lines of `if foo is not None: v4_kwargs["foo"] = foo` — second copy of the same logic, and bypasses `_dispatch_build` entirely |
| Curve fallback also bypasses `_dispatch_build` | `bot.py:286–293` | Direct `self._curve_builder.build(address, ...)` call with its own hand-picked kwargs — third code path |
| `**kwargs` forwarding to builders | `bot.py:326–333`, `async_bot.py:266–277` | Builders accept `**kwargs: Any` (with `# noqa: ARG002`) — unknown kwargs are silently swallowed, typos go undetected |
| Adding a pool-specific parameter | Both `build_pool()` signatures | Must touch bot.py, async_bot.py, both `dispatch_kwargs` dicts, and the V4 fast-path kwargs |

## Solution

### `BuildPoolRequest` — optional-params-only frozen dataclass

`BuildPoolRequest` carries **only** the optional parameters. Required parameters (`address`, `chain_id`, `io`) remain positional on both `build_pool()` and `builder.build()`. This avoids the semantic-overload problem (V4 `address` = PoolManager vs. non-V4 `address` = pool contract) — `address` stays a plain positional arg with its existing semantics.

```python
# src/degenbot/builders/request.py

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Sequence

if TYPE_CHECKING:
    pass


@dataclass(slots=True, frozen=True, kw_only=True)
class BuildPoolRequest:
    """Typed request object carrying optional parameters for pool construction.

    Carries all optional parameters for build_pool() and its dispatched
    builders. Required parameters (address, chain_id, io) remain on
    builder.build() as positional/keyword arguments.

    Builders read the fields they recognize and ignore the rest.
    When ``pool_id`` is not None, the caller's ``address`` refers to the
    PoolManager contract (V4 managed-pool semantics).
    """

    # Common options
    silent: bool = False
    state_block: int | None = None
    state_cache_depth: int = 8

    # V2-family options
    deployer_address: str | None = None
    init_hash: str | None = None

    # V3/V4 tick options
    tick_bitmap: dict[int, Any] | None = None
    tick_data: dict[int, Any] | None = None

    # V4-specific options
    pool_id: str | bytes | None = None
    state_view_address: str | None = None
    tokens: Sequence[str] | None = None
    fee: int | None = None
    tick_spacing: int | None = None
    hook_address: str | None = None
```

### All dispatch paths route through `_dispatch_build`

Currently there are three separate code paths that construct kwargs dicts and call builders:

1. **V4 fast-path** (`bot.py:248–269`): constructs `v4_kwargs`, calls `self._v4_builder.build(address, io=io, **v4_kwargs)` directly
2. **Curve fallback** (`bot.py:286–293`): calls `self._curve_builder.build(address, ...)` directly on type-resolution failure
3. **General dispatch** (`bot.py:311–333`): constructs `dispatch_kwargs`, calls `self._dispatch_build(builder=builder, ...)`

After this plan, all three paths construct a single `BuildPoolRequest` and route through `_dispatch_build`:

```python
def build_pool(self, address, *, ...12 kwargs...) -> AbstractLiquidityPool:
    address = get_checksum_address(address)
    chain_id = chain_id or self.connections.default_chain_id
    io = SyncPoolIO(self.connections.get_provider(chain_id))

    request = BuildPoolRequest(
        silent=silent,
        state_block=state_block,
        state_cache_depth=state_cache_depth,
        deployer_address=deployer_address,
        init_hash=init_hash,
        tick_bitmap=tick_bitmap,
        tick_data=tick_data,
        pool_id=pool_id,
        state_view_address=state_view_address,
        tokens=tokens,
        fee=fee,
        tick_spacing=tick_spacing,
        hook_address=hook_address,
    )

    # V4 fast path: pool_id discriminates V4 managed pools
    if pool_id is not None:
        return self._dispatch_build(
            builder=self._v4_builder,
            address=address,
            chain_id=chain_id,
            io=io,
            request=request,
        )

    # Check pool registry — return existing pool if already built
    existing = self.pools.get(chain_id=chain_id, pool_address=address)
    if existing is not None:
        return existing

    # Resolve the pool type and dispatch to the appropriate builder
    try:
        pool_type = _resolve_pool_type_impl(
            address, chain_id=chain_id, io=io, db=self.db
        )
    except DegenbotValueError:
        # Fallback: try Curve builder as last resort
        return self._dispatch_build(
            builder=self._curve_builder,
            address=address,
            chain_id=chain_id,
            io=io,
            request=request,
        )

    pool_class = pool_class_for_descriptor(pool_type, chain_id=chain_id)
    builder = self._builders.get(pool_class)
    # ... MRO walk ...
    return self._dispatch_build(
        builder=builder, address=address,
        chain_id=chain_id, io=io, request=request,
    )


@staticmethod
def _dispatch_build(
    *,
    builder: PoolBuilder,
    address: ChecksumAddress,
    chain_id: ChainId,
    io: PoolIO,
    request: BuildPoolRequest,
) -> AbstractLiquidityPool:
    """Dispatch to the builder with a typed request."""
    return builder.build(address, chain_id=chain_id, io=io, request=request)
```

This eliminates both `dispatch_kwargs` and `v4_kwargs` — one `BuildPoolRequest` constructed once, passed through one method.

### Builders: one-shot migration to `request: BuildPoolRequest`

Each builder replaces its individual optional kwargs and `**kwargs` with `request: BuildPoolRequest` in a single step. No intermediate state where both old and new params coexist.

```python
# Before
class V2PoolBuilder(V2BuilderBase):
    def build(
        self, address, *, chain_id=None, deployer_address=None, init_hash=None,
        state_block=None, silent=False, state_cache_depth=8, io, **kwargs,
    ):
        ...

# After
class V2PoolBuilder(V2BuilderBase):
    def build(
        self, address, *, chain_id=None, io, request: BuildPoolRequest,
    ):
        chain_id = chain_id or self._default_chain_id
        state_block = request.state_block if request.state_block is not None else io.get_block_number()
        silent = request.silent
        state_cache_depth = request.state_cache_depth
        deployer_address = request.deployer_address
        init_hash = request.init_hash
        ...
```

The V4 builder reads `request.pool_id`, `request.state_view_address`, etc. When `request.pool_id is not None`, the caller's `address` arg is the PoolManager — this matches current behavior (the V4 fast-path sets `v4_kwargs["pool_manager_address"] = address`). No `pool_manager_address` field needed on `BuildPoolRequest`.

Unelevant fields are simply not read by the builder — a V2 builder ignores `request.pool_id`, a V4 builder ignores `request.deployer_address`. This is the god-object trade-off (see Design Decisions).

### Design decisions

- **`BuildPoolRequest` carries only optional parameters**: Required params (`address`, `chain_id`, `io`) stay as positional/keyword args on `build()`. This avoids the V4 `address`-semantics problem and keeps the required/optional boundary explicit. Every field on `BuildPoolRequest` has a `None` or non-`None` default.

- **One-shot builder migration, not gradual**: Builders are internal (not public API). Replacing individual kwargs + `**kwargs` with `request: BuildPoolRequest` in one step avoids the half-migration state where builders have both old and new params. The `PoolBuilder` protocol is updated once.

- **All dispatch paths route through `_dispatch_build`**: The V4 fast-path and Curve fallback currently bypass `_dispatch_build`. After this plan, they route through it like the general path. This reduces three kwargs-construction sites to one `BuildPoolRequest` construction. The V4 builder is already in the builder registry — no special handling needed.

- **God-object dataclass accepted**: `BuildPoolRequest` has 13 fields spanning V2/V3/V4/Curve concerns. Builders read what they need and ignore the rest. The alternative — per-family subclasses like `V2BuildPoolRequest(BuildPoolRequest)` — reintroduces isinstance dispatch and adds a class per pool family. The flat dataclass trades field-noise for structural simplicity and zero dispatch.

- **`pool_manager_address` not on `BuildPoolRequest`**: When `request.pool_id is not None`, the caller's `address` arg is the PoolManager address. This matches current runtime behavior (the V4 fast-path already does this). The V4 builder reads `address` as `pool_manager_address` internally. Adding a redundant field would be `None` for all non-V4 pools.

- **Frozen dataclass, not a dict**: A frozen dataclass guarantees immutability and provides type-checked field access. A dict would be no better than `dispatch_kwargs`. Frozen also enables use as dict keys and set membership if needed.

- **`build_pool()` caller-facing signature unchanged**: Callers still pass 12 individual kwargs. `build_pool()` constructs `BuildPoolRequest` internally. A future plan can expose `BuildPoolRequest` directly to callers, but that's out of scope — this plan targets the internal kwargs-tunnel friction.

- **Typed attribute access catches typos**: Today `**kwargs: Any` silently swallows unknown kwargs (all builders have `# noqa: ARG002`). With `request.field` access, a typo raises `AttributeError` on a frozen dataclass — immediate signal instead of silent failure.

## Files Involved

**Primary:**
- `src/degenbot/builders/request.py` — new file; `BuildPoolRequest` frozen dataclass
- `src/degenbot/bot.py` — construct `BuildPoolRequest` in `build_pool()`, delete `dispatch_kwargs` and `v4_kwargs`, route all paths through `_dispatch_build`
- `src/degenbot/async_bot.py` — same changes; note: Curve fallback is a `raise DegenbotValueError` (no async Curve builder), not a builder call
- `src/degenbot/builders/protocol.py` — update `PoolBuilder.build()` and `AsyncPoolBuilder.build()` signatures: replace `**kwargs` with `request: BuildPoolRequest`

**Secondary (builder one-shot migration):**
- `src/degenbot/builders/v2_pool_builder.py` — replace individual kwargs + `**kwargs` with `request: BuildPoolRequest`
- `src/degenbot/builders/v3_pool_builder.py` — same
- `src/degenbot/builders/v4_pool_builder.py` — same
- `src/degenbot/builders/curve_pool_builder.py` — same
- `src/degenbot/builders/aerodrome_v2_builder.py` — same
- `src/degenbot/builders/camelot_builder.py` — same
- `src/degenbot/builders/async_v2_pool_builder.py` — same
- `src/degenbot/builders/async_v3_pool_builder.py` — same
- `src/degenbot/builders/async_v4_pool_builder.py` — same

**No change needed:**
- `src/degenbot/builders/erc20_builder.py` — no `build_pool` involvement
- `src/degenbot/builders/async_erc20_builder.py` — same

## Implementation Order

### Slice 1: Define `BuildPoolRequest`, update protocol, migrate Bot

1. Create `src/degenbot/builders/request.py` with the frozen dataclass
2. Update `PoolBuilder.build()` and `AsyncPoolBuilder.build()` protocol signatures: replace `**kwargs: Any` with `request: BuildPoolRequest`
3. In `bot.py`, construct `BuildPoolRequest` once at the top of `build_pool()`, delete `v4_kwargs` and `dispatch_kwargs` blocks, route V4 fast-path and Curve fallback through `_dispatch_build`
4. Update `_dispatch_build` signature: `(*, builder, address, chain_id, io, request: BuildPoolRequest)`
5. Run: `just test-python` — expect pytest errors (builders don't accept `request` yet)

### Slice 2: Migrate all 9 builders one-shot

1. For each builder, replace individual optional kwargs + `**kwargs` with `request: BuildPoolRequest`
2. Each builder reads the fields it needs from `request` and ignores the rest
3. Builders:
   - `V2PoolBuilder` → reads `silent`, `state_block`, `state_cache_depth`, `deployer_address`, `init_hash`
   - `AerodromeV2Builder` → same as V2
   - `CamelotBuilder` → same as V2
   - `V3PoolBuilder` → reads `silent`, `state_block`, `state_cache_depth`, `deployer_address`, `init_hash`, `tick_bitmap`, `tick_data`
   - `V4PoolBuilder` → reads `silent`, `state_block`, `state_cache_depth`, `tick_bitmap`, `tick_data`, `pool_id`, `state_view_address`, `tokens`, `fee`, `tick_spacing`, `hook_address`
   - `CurvePoolBuilder` → reads `silent`, `state_block`, `state_cache_depth`
   - `AsyncV2PoolBuilder` → same as sync V2
   - `AsyncV3PoolBuilder` → same as sync V3
   - `AsyncV4PoolBuilder` → same as sync V4
4. Run: `just test-python` — expect all green

### Slice 3: Migrate AsyncBot

1. In `async_bot.py`, construct `BuildPoolRequest` once, delete `v4_kwargs` and `dispatch_kwargs`
2. Route V4 fast-path through `_dispatch_build` (same pattern as Bot)
3. AsyncBot's Curve fallback remains a `raise DegenbotValueError` — no builder call, no `request` needed for that path
4. Run: `just test-python` — expect all green

### Slice 4: Validate, lint, update context docs

1. Run `just test-all`
2. Run `just lint`
3. Add `BuildPoolRequest` term to `src/degenbot/builders/CONTEXT.md`
4. Update `CONTEXT-MAP.md` cross-module relationships: `build_pool()` now dispatches via `BuildPoolRequest`
5. Update `docs/adr/ADR-001-io-free-pools.md` Phase 4

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Slices 1+2 may fail until both are complete (protocol → builders must change together).

### New unit tests

```python
# tests/builders/test_build_pool_request.py

from degenbot.builders.request import BuildPoolRequest
import pytest


def test_build_pool_request_frozen():
    """BuildPoolRequest is immutable after construction."""
    req = BuildPoolRequest()
    with pytest.raises(AttributeError):
        req.silent = True


def test_build_pool_request_defaults():
    """BuildPoolRequest has sensible defaults for all optional fields."""
    req = BuildPoolRequest()
    assert req.silent is False
    assert req.state_block is None
    assert req.state_cache_depth == 8
    assert req.deployer_address is None
    assert req.tick_bitmap is None
    assert req.pool_id is None


def test_build_pool_request_v4_fields():
    """BuildPoolRequest carries V4-specific fields."""
    req = BuildPoolRequest(pool_id=b"\x01\x02", tick_spacing=60)
    assert req.pool_id == b"\x01\x02"
    assert req.tick_spacing == 60


def test_build_pool_request_no_required_fields():
    """BuildPoolRequest has no required fields — all params are optional."""
    req = BuildPoolRequest()
    assert req.silent is False  # only non-None default
    # Every field is optional; constructing with no args is valid
```

### Integration tests

Existing `tests/test_bot.py` exercises `build_pool()` end-to-end. After the refactoring, these tests continue to pass with no changes — the caller-facing API is identical.

A `FakeBuilder` that captures the `request` parameter can verify the new flow:

```python
def test_build_pool_constructs_request():
    """build_pool() constructs BuildPoolRequest and passes it through _dispatch_build."""
    builder = FakeBuilder()  # records request
    bot = make_bot_with_builder(builder)
    bot.build_pool("0x...", deployer_address="0xabc", tick_spacing=60)
    assert builder.captured_request.deployer_address == "0xabc"
    assert builder.captured_request.tick_spacing == 60
```

## Benefits

- **Locality**: Adding a pool-specific parameter means adding a field to `BuildPoolRequest`, not touching two `build_pool()` signatures, two `dispatch_kwargs` dicts, and a `v4_kwargs` dict
- **Leverage**: One typed object flows through the build pipeline; builders destructure what they need
- **Depth**: The `dispatch_kwargs` / `v4_kwargs` dictionaries are shallow pass-throughs (interface = implementation). `BuildPoolRequest` is deeper — it carries typed data and hides the filtering logic
- **Type safety**: IDE autocompletion shows `BuildPoolRequest` fields; `request.field` access catches typos via `AttributeError` instead of silently swallowing them through `**kwargs`
- **Single dispatch path**: V4 fast-path and Curve fallback now route through the same `_dispatch_build`, eliminating three separate kwargs-construction sites
- **Future-facing**: Plan 070 (Balancer builder) adds Balancer-specific optional fields to `BuildPoolRequest` — no new kwargs on `build_pool()`, no new `dispatch_kwargs` entries

## Risks

- **Protocol + builders must change together**: Slice 1 changes the protocol; Slice 2 changes all 9 builders. Tests will fail between these two slices. Mitigation: combine Slices 1+2 into a single atomic commit, or complete them in the same session.
- **God-object dataclass**: All pool families see all fields. A V2 builder sees `pool_id` and `hook_address` — irrelevant but harmless. Alternative (per-family subclasses) was rejected as it reintroduces isinstance dispatch (see Design Decisions).
- **No immediate caller-facing API change**: `build_pool()` still accepts 12 kwargs internally. This is intentional — a future plan can expose `BuildPoolRequest` directly to callers.
- **Frozen dataclass overhead**: Negligible. Construction is a one-time cost per `build_pool()` call.

## Relationship to Other Plans

- **Plan 065** (AsyncBot inline I/O): Orthogonal — Plan 065 removed I/O methods from AsyncBot but didn't change `build_pool()`. AsyncBot is smaller, making this refactor slightly easier.
- **Plan 066** (Unify type resolution): Orthogonal — Plan 066 collapsed sync/async pure functions but didn't change the `build_pool()` kwargs flow. Type resolution calls remain the same.
- **Plan 048** (Async Builder Shared): Predecessor — Plan 048 introduced `PoolIO` as the builder-facing I/O seam; this plan introduces `BuildPoolRequest` as the builder-facing params seam.
- **Plan 070** (Balancer Builder): Downstream consumer — Plan 070 will add Balancer-specific fields (`pool_id` for Balancer vault, `vault_address`, `weights`) to `BuildPoolRequest` instead of adding kwargs to `build_pool()`.
- **Plan 014** (Async REPL): Orthogonal — no interaction with `build_pool()` kwargs.

## Status

[x] Slice 1: Define `BuildPoolRequest`, update protocol, migrate Bot
[x] Slice 2: Migrate all 9 builders one-shot
[x] Slice 3: Migrate AsyncBot
[x] Slice 4: Validate, lint, update context docs
