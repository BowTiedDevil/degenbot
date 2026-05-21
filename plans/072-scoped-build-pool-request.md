# Plan 072: Scoped Build Pool Request Options

## Overview

Refactor `BuildPoolRequest` so that builder-specific options live in scoping sub-objects (e.g. `V2Options`, `V3Options`, `BalancerOptions`) instead of as flat fields on a single shared dataclass. Each builder reads only its own sub-object, eliminating cross-family field pollution and making the type self-documenting.

## Problem

### Deletion test

If you deleted all the builder-specific fields from `BuildPoolRequest`, only `silent`, `state_block`, and `state_cache_depth` would remain — the truly universal options. Every other field is family-specific noise on a shared type.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| V2 fields on a type used by V3/V4/Curve/Balancer | `builders/request.py` — `deployer_address`, `init_hash` | V3 builder ignores them; misleading API surface |
| V3/V4 tick fields on a type used by V2 | `builders/request.py` — `tick_bitmap`, `tick_data` | V2 builder ignores them; type grows with every new pool family |
| V4-specific fields (`pool_id`, `hook_address`) mixed in | `builders/request.py` | Each new pool family adds more flat fields; no scoping boundary |
| Balancer-specific fields (`bpt_idx`, `invariant_version`) added by Plan 070 | `builders/request.py` | Same pattern continues; `BuildPoolRequest` becomes a grab-bag |
| No type-safety for misassigned fields | Any builder | A V2 builder could accidentally read `bpt_idx` and get `None` instead of a type error |

## Solution

### Step 1: Create per-family option dataclasses

```python
# In builders/request.py

@dataclass(slots=True, frozen=True, kw_only=True)
class _BaseBuilderOptions:
    """Options shared by all builder families — none currently.
    Added for type-safety so that builder-specific options can be
    distinguished from the universal BuildPoolRequest fields.
    """

@dataclass(slots=True, frozen=True, kw_only=True)
class V2BuildOptions(_BaseBuilderOptions):
    deployer_address: str | None = None
    init_hash: str | None = None

@dataclass(slots=True, frozen=True, kw_only=True)
class V3BuildOptions(_BaseBuilderOptions):
    tick_bitmap: dict[int, Any] | None = None
    tick_data: dict[int, Any] | None = None

@dataclass(slots=True, frozen=True, kw_only=True)
class V4BuildOptions(_BaseBuilderOptions):
    pool_id: str | bytes | None = None
    state_view_address: str | None = None
    tokens: Sequence[str] | None = None
    fee: int | None = None
    tick_spacing: int | None = None
    hook_address: str | None = None

@dataclass(slots=True, frozen=True, kw_only=True)
class BalancerBuildOptions(_BaseBuilderOptions):
    bpt_idx: int | None = None
    invariant_version: int | None = None
```

### Step 2: Replace flat fields with scoping sub-objects

```python
# Before
@dataclass(slots=True, frozen=True, kw_only=True)
class BuildPoolRequest:
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

# After
@dataclass(slots=True, frozen=True, kw_only=True)
class BuildPoolRequest:
    silent: bool = False
    state_block: int | None = None
    state_cache_depth: int = 8

    v2: V2BuildOptions | None = None
    v3: V3BuildOptions | None = None
    v4: V4BuildOptions | None = None
    balancer: BalancerBuildOptions | None = None
```

### Step 3: Update builders to read from their scoped option

Each builder reads from its own sub-object instead of flat fields:

```python
# In V2PoolBuilder:
# Before:
deployer = request.deployer_address
init_hash = request.init_hash

# After:
v2_opts = request.v2 or V2BuildOptions()
deployer = v2_opts.deployer_address
init_hash = v2_opts.init_hash
```

### Step 4: Add convenience property for backward compatibility (optional)

If external callers construct `BuildPoolRequest` directly with flat field access, add a deprecation path:

```python
@property
def deployer_address(self) -> str | None:
    """Deprecated — use request.v2.deployer_address."""
    return self.v2.deployer_address if self.v2 is not None else None
```

This step is optional and should only be included if there are many external callers to migrate.

### Design decisions

- **Sub-objects over flat fields**: Each builder reads only its own sub-object. The type self-documents which fields belong to which family. New families add a new sub-object instead of polluting the shared type.
- **`_BaseBuilderOptions` base class**: Provides a common type for `Union[V2BuildOptions, V3BuildOptions, ...]` if needed. Currently empty but extensible for cross-family option sharing (e.g. `pool_id` might be shared by V4 and Balancer in the future).
- **Optional sub-objects**: `None` by default means "no family-specific options." Builders that don't need options don't need to construct a sub-object. This matches the current `None` default for all flat fields.
- **Migration strategy**: Replace flat fields with sub-objects field-by-field within each builder. Each field migration is a single red-green cycle. No compatibility shim unless external callers are discovered.
- **`pool_id` moves to V4BuildOptions**: `pool_id` was added for V4 managed-pool semantics. Balancer also has a `pool_id` concept but should construct it independently in `BalancerBuildOptions` if needed (the semantics differ — V4's is a PoolManager key, Balancer's is a Vault pool identifier).

## Files Involved

**Primary:**
- `src/degenbot/builders/request.py` — refactor `BuildPoolRequest`, add option dataclasses

**Secondary:**
- `src/degenbot/builders/v2_pool_builder.py` — read from `request.v2`
- `src/degenbot/builders/v3_pool_builder.py` — read from `request.v3`
- `src/degenbot/builders/v4_pool_builder.py` — read from `request.v4`
- `src/degenbot/builders/curve_pool_builder.py` — verify no family-specific fields used
- `src/degenbot/builders/balancer_builder.py` — read from `request.balancer` (after Plan 070)
- `src/degenbot/bot.py` — update `BuildPoolRequest` construction sites

## Implementation Order

### Slice 1: Create option dataclasses, add sub-object fields to BuildPoolRequest

1. Add `V2BuildOptions`, `V3BuildOptions`, `V4BuildOptions`, `BalancerBuildOptions` to `request.py`
2. Add `v2`, `v3`, `v4`, `balancer` optional fields to `BuildPoolRequest`
3. Keep flat fields for now (dual-write period)
4. Run: `just test-python` — expect all green

### Slice 2: Migrate V2 builder

1. Update `V2PoolBuilder` to read from `request.v2` instead of flat fields
2. Update `Bot.build_pool()` V2 construction sites to use `V2BuildOptions`
3. Run: `just test-python` — expect all green

### Slice 3: Migrate V3/V4 builders

1. Update `V3PoolBuilder` to read from `request.v3`
2. Update `V4PoolBuilder` to read from `request.v4`
3. Update `Bot.build_pool()` V3/V4 construction sites
4. Run: `just test-python` — expect all green

### Slice 4: Migrate Balancer builder (after Plan 070)

1. Update `BalancerBuilder` to read from `request.balancer`
2. Run: `just test-python` — expect all green

### Slice 5: Remove flat fields

1. Remove all family-specific flat fields from `BuildPoolRequest`
2. Remove any backward-compat properties
3. Run: `just lint` + `just test-all`

## Testing

### Per-slice test runs

Each slice runs `just test-python`.

### New unit tests

```python
# tests/builders/test_build_pool_request.py


def test_v2_options_scoped():
    """V2BuildOptions fields are isolated from V3/V4/Balancer."""

def test_none_default():
    """BuildPoolRequest sub-objects default to None."""

def test_builder_reads_scoped_options():
    """V2PoolBuilder reads from request.v2, not flat fields."""
```

### Integration tests

Existing `Bot.build_pool()` integration tests cover the builder paths. No new integration tests needed — each slice should pass the existing test suite.

## Benefits

- **Locality**: Each builder's options are defined next to its logic, not in a shared grab-bag.
- **Depth**: `BuildPoolRequest` is a shallow seam; the sub-objects are deep seams that match the builder's actual interface.
- **Type-safety**: A V2 builder reading `request.balancer.bpt_idx` would be a code smell; `request.v2` is self-documenting.
- **Extensibility**: New pool families add a new sub-object instead of flat fields.

## Risks

- **Migration surface**: Every `BuildPoolRequest(...)` construction site must be updated. Mitigation: greppable — search for `deployer_address=`, `tick_bitmap=`, etc.
- **Backward compatibility**: External callers constructing `BuildPoolRequest` with flat fields will break. Mitigation: dual-write period in Slice 1, deprecation warnings if needed.
- **Over-engineering**: For a small number of families (4-5), flat fields are tolerable. The tipping point is ~15-20 fields (we're at ~13 now and growing with Plan 070).

## Relationship to Other Plans

- **Plan 070** (Balancer Builder): Plan 070 adds `bpt_idx` and `invariant_version` as flat fields. Plan 072 migrates them into `BalancerBuildOptions` in Slice 4. Plan 070 must land first.
- **Plan 014** (Async REPL): Orthogonal — same request type used by async builders, same migration applies.

## Status

[ ] Slice 1: Create option dataclasses, add sub-object fields to BuildPoolRequest
[ ] Slice 2: Migrate V2 builder
[ ] Slice 3: Migrate V3/V4 builders
[ ] Slice 4: Migrate Balancer builder (after Plan 070)
[ ] Slice 5: Remove flat fields
