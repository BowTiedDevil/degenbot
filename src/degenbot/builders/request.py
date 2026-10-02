"""BuildPoolRequest — typed request object for pool construction."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Sequence


@dataclass(slots=True, frozen=True, kw_only=True)
class BuildPoolRequest:
    """Typed request object carrying optional parameters for pool construction.

    Carries all optional parameters for build_pool() and its dispatched
    builders. Required parameters (address, chain_id, io) remain on
    builder.build() as positional/keyword arguments.

    Builders read the fields they recognize and ignore the rest.
    """

    # Common options
    silent: bool = False
    state_block: int | None = None
    state_cache_depth: int = 8

    # V3 tick options
    tick_bitmap: dict[int, Any] | None = None
    tick_data: dict[int, Any] | None = None

    # The resolved construction route (GLOSSARY.md, Construction route) — the
    # cockpit's policy ordering the core route entry walks. `None` = the
    # generic-only route (the behavior-preserving default).
    construction_route: ConstructionRoute | None = None

    # Balancer options (flat fields matching existing pattern)
    bpt_idx: int | None = None  # Override BPT index detection
    invariant_version: int | None = None  # Override: INVARIANT_V1 or INVARIANT_V2


@dataclass(slots=True, frozen=True, kw_only=True)
class ConstructionRoute:
    """The resolved construction-route policy (GLOSSARY.md, Construction route).

    ONE ordered attempt policy for constructing a family-unknown single-address
    pool: the factory rungs in policy order, terminating in the generic
    builder rung. A driver VALUE the cockpit resolves — the core route entry
    walks it and classifies every failure on the build-refusal taxonomy; the
    driver never re-implements the chain (the retired tracker fallback chain
    is the bug this replaces).
    """

    factories: tuple[str, ...] = ()
    generic: bool = True


@dataclass(slots=True, frozen=True, kw_only=True)
class BuildManagedPoolRequest:
    """Typed request object for V4 managed-pool construction.

    ``pool_id`` is required — V4 pools cannot be discovered without it.
    Immutable data (``state_view_address``, ``tokens``, ``fee``,
    ``tick_spacing``, ``hook_address``) is required when the pool is not
    in the database; otherwise it is fetched from DB.
    """

    # Required — V4 pools cannot be discovered without a pool ID
    pool_id: str | bytes

    # Common options (mirrors BuildPoolRequest's universal fields)
    silent: bool = False
    state_block: int | None = None
    state_cache_depth: int = 8

    # V4 immutable data — required if not in DB
    state_view_address: str | None = None
    tokens: Sequence[str] | None = None
    fee: int | None = None
    tick_spacing: int | None = None
    hook_address: str | None = None

    # Pre-fetched tick data (DB snapshot or test fixtures)
    tick_bitmap: dict[int, Any] | None = None
    tick_data: dict[int, Any] | None = None


# Union type for dispatch methods that accept either request shape.
BuildRequest = BuildPoolRequest | BuildManagedPoolRequest
