"""Path-composition rejection predicates.

A :class:`~degenbot.arbitrage.EngineRegistry` can refuse an
:func:`~degenbot.arbitrage.EngineRegistry.register_path` candidate by
*policy* — token denylist/allowlist, hop-count min/max, min-liquidity gate,
duplicate-pool guard. This is the distinct, policy-layer concern; the Rust
core's pool *admission* floor (hooked/dynamic-fee V4 refusal, surfaced as
``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError``) stays in the
Rust core (ADR-005 standalone-core constraint).

Rejections surface as the typed :class:`~degenbot.exceptions.PathRejectedError`
family (subclassing :class:`~degenbot.exceptions.ArbitrageError`, NOT
``ValueError``) so callers classify by type — the same typed-exception
pattern used for pool admission + verification.

The predicate is a pluggable protocol: callers inject a
:class:`PathPolicy` (or any structural implementation of
:class:`PathCompositionPredicate`) into ``EngineRegistry``.
"""

from __future__ import annotations

import dataclasses
import sys
from typing import TYPE_CHECKING, Protocol, runtime_checkable

from degenbot.builders.erc20_builder import NATIVE_SENTINEL_ADDRESSES
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import (
    DegenbotValueError,
    DuplicatePoolError,
    HopCountExceededError,
    HopCountInsufficientError,
    InsufficientLiquidityError,
    TokenDenylistedError,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from degenbot.types.chain import ChecksummedAddress

__all__ = [
    "NoOpPathPredicate",
    "PathCompositionPredicate",
    "PathPolicy",
    "touched_tokens",
]

#: The concrete pool union ``register_path`` accepts.
type ArbPathPool = UniswapV2Pool | UniswapV3Pool | UniswapV4Pool


@runtime_checkable
class PathCompositionPredicate(Protocol):
    """Pluggable path-composition policy.

    Evaluate a path candidate ``(pool, zero_for_one)`` sequence. Accept by
    returning ``None``; reject by raising a
    :class:`~degenbot.exceptions.PathRejectedError` subtype. Rejections
    propagate out of
    :meth:`~degenbot.arbitrage.EngineRegistry.register_path` so callers
    classify by type.
    """

    def evaluate(self, pools_and_zfos: Sequence[tuple[ArbPathPool, bool]]) -> None:
        """Evaluate the path; raise ``PathRejectedError`` to reject."""
        ...


class NoOpPathPredicate:
    """Default predicate — accepts every path (no policy enforced)."""

    def evaluate(
        self,
        pools_and_zfos: Sequence[tuple[ArbPathPool, bool]],
    ) -> None:
        """Accept the path (no-op)."""
        # Intentionally empty: the default predicate enforces no policy.


def _pool_identity(pool: ArbPathPool) -> str:
    """Return the stable identity key for duplicate detection.

    V2/V3 pools: checksummed ``address``. V4 pools: ``pool_id`` hex (the V4
    address is the shared ``PoolManager`` — not unique per pool). Mirrors the
    example's ``build_paths`` ``pool_sig`` logic.

    Returns:
        The pool's identity key (checksummed address or V4 pool_id hex).

    """
    if isinstance(pool, UniswapV4Pool):
        return to_0x_hex(pool.pool_id)
    return get_checksum_address(pool.address)


def _policy_address(token: ChecksummedAddress | str) -> ChecksummedAddress:
    """Return the checksummed token address a policy entry names.

    Returns:
        The checksummed address the entry names.

    Raises:
        DegenbotValueError: If the entry is a raw protocol native-asset
            sentinel — one protocol's wire spelling, never a way to name the
            native asset in an input (ADR-097 D1).

    """
    address = get_checksum_address(token)
    if address in NATIVE_SENTINEL_ADDRESSES:
        msg = (
            f"Policy token {address} is a protocol native-asset sentinel, "
            "not a token contract address; name the native asset through "
            "NativeToken (Bot.get_native_token)."
        )
        raise DegenbotValueError(message=msg)
    return address


def _hop_token_pair(pool: ArbPathPool, zfo: bool) -> tuple[str, str]:  # ruff:ignore[boolean-type-hint-positional-argument]
    """Return the ``(token_in, token_out)`` wire addresses a hop swaps between.

    ``zfo=True`` → token0→token1; ``zfo=False`` → token1→token0. Each leg
    carries the wire spelling its pool's protocol gives it — the same key
    space the core's ``touched_tokens`` (``policy.rs``) reads off the hop
    facts. A V4 pair's key already carries its currencies (a native leg is
    that protocol's ``address(0)`` currency there); a raw sentinel never
    NAMES the native asset in a policy input (ADR-097 D1), and
    :func:`_policy_address` refuses one there.

    Returns:
        The ``(token_in, token_out)`` wire addresses for this hop.

    """
    if isinstance(pool, UniswapV4Pool):
        currency0, currency1 = pool.pool_key.currency0, pool.pool_key.currency1
        return (currency0, currency1) if zfo else (currency1, currency0)
    token_in = pool.token0 if zfo else pool.token1
    token_out = pool.token1 if zfo else pool.token0
    # A V2/V3/Aerodrome pair holds only ERC-20 contracts — the native asset
    # gets a wire spelling only from a protocol that carries one (the V4
    # currencies above) (ADR-097 D3).
    assert isinstance(token_in, Erc20Token)
    assert isinstance(token_out, Erc20Token)
    return token_in.address, token_out.address


def touched_tokens(pools_and_zfos: Sequence[tuple[ArbPathPool, bool]]) -> list[str]:
    """Return the ordered, de-duplicated token addresses a path touches.

    Captures the input token (first hop's ``token_in``) + every hop's
    ``token_out`` (which includes all intermediate connecting tokens and the
    final profit token). Order: input first, then each hop's output in path
    order; duplicates removed preserving first occurrence. The addresses are
    the hops' wire spellings (:func:`_hop_token_pair`) — the same key space
    the core's ``touched_tokens`` (``policy.rs``) reads.

    Returns:
        The ordered, de-duplicated list of touched token addresses (input +
        intermediates + profit token).

    """
    touched: list[str] = []
    seen: set[str] = set()
    for idx, (pool, zfo) in enumerate(pools_and_zfos):
        token_in, token_out = _hop_token_pair(pool, zfo)
        # The first hop's token_in is the path's input token.
        if idx == 0 and token_in not in seen:
            seen.add(token_in)
            touched.append(token_in)
        if token_out not in seen:
            seen.add(token_out)
            touched.append(token_out)
    return touched


@dataclasses.dataclass(frozen=True)
class PathPolicy:
    """Composable path-composition policy implementing the predicate protocol.

    Every rule is opt-in via its fields; the defaults accept any path. Rules
    evaluate in a fixed order so the *first* violation raises (deterministic
    for callers' classification):

    1. hop-count bounds (min then max)
    2. token denylist / allowlist
    3. duplicate-pool guard
    4. min-liquidity gate (only when ``liquidity_of`` is supplied)

    Token-address inputs are normalized by checksum so denylist/allowlist
    entries supplied in any case match the hops' wire token addresses. A raw
    protocol sentinel is refused loudly (ADR-097 D1): one protocol's wire
    spelling is never a way to name the native asset in an input.

    The library deliberately does NOT encode pool-type-specific liquidity
    math (V2 reserves vs. V3/V4 ``liquidity``/``sqrt_price_x96``). Supply a
    ``liquidity_of`` callable that returns the liquidity proxy for a pool, or
    leave it ``None`` to skip the gate.
    """

    disallowed_tokens: frozenset[str] = frozenset()
    allowed_tokens: frozenset[str] | None = None
    min_hops: int = 1
    max_hops: int = sys.maxsize
    min_liquidity: int = 0
    liquidity_of: Callable[[ArbPathPool], int] | None = None
    reject_duplicate_pools: bool = True

    def evaluate(self, pools_and_zfos: Sequence[tuple[ArbPathPool, bool]]) -> None:
        """Evaluate all enabled rules; raise on the first violation.

        Raises:
            HopCountInsufficientError: If the hop count is below ``min_hops``.
            HopCountExceededError: If the hop count exceeds ``max_hops``.
            TokenDenylistedError: If a touched token is denylisted, or (when an
                allowlist is set) absent from the allowlist.
            DuplicatePoolError: If the same pool appears more than once and
                ``reject_duplicate_pools`` is set.
            InsufficientLiquidityError: If a pool's ``liquidity_of`` proxy is
                below ``min_liquidity`` (only when ``liquidity_of`` is set).

        """
        hop_count = len(pools_and_zfos)

        # 1. Hop-count bounds.
        if hop_count < self.min_hops:
            raise HopCountInsufficientError(
                hop_count=hop_count,
                min_hops=self.min_hops,
            )
        if hop_count > self.max_hops:
            raise HopCountExceededError(
                hop_count=hop_count,
                max_hops=self.max_hops,
            )

        # Normalize denylist/allowlist to checksummed addresses once.
        disallowed = frozenset(_policy_address(t) for t in self.disallowed_tokens)
        allowed = (
            None
            if self.allowed_tokens is None
            else frozenset(_policy_address(t) for t in self.allowed_tokens)
        )

        # 2. Token denylist / allowlist (input + intermediates + profit).
        if disallowed or allowed is not None:
            for token in touched_tokens(pools_and_zfos):
                if disallowed and token in disallowed:
                    raise TokenDenylistedError(token=token)
                if allowed is not None and token not in allowed:
                    raise TokenDenylistedError(token=token)

        # 3. Duplicate-pool guard.
        if self.reject_duplicate_pools:
            seen_pools: set[str] = set()
            for pool, _zfo in pools_and_zfos:
                identity = _pool_identity(pool)
                if identity in seen_pools:
                    raise DuplicatePoolError(pool=identity)
                seen_pools.add(identity)

        # 4. Min-liquidity gate (caller-supplied extractor only).
        if self.liquidity_of is not None and self.min_liquidity > 0:
            for pool, _zfo in pools_and_zfos:
                liquidity = self.liquidity_of(pool)
                if liquidity < self.min_liquidity:
                    raise InsufficientLiquidityError(
                        liquidity=liquidity,
                        min_liquidity=self.min_liquidity,
                    )
