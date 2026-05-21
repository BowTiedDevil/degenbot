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
