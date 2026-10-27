"""Factory helpers for I/O-free Balancer V2 pool construction in tests.

Mirrors ``tests/helpers/curve_pool_factory.py`` (and ``v3_pool_factory.py``
/ ``v4_pool_factory.py``): every direct ``BalancerV2Pool(...)`` /
``BalancerV2StablePool(...)``
construction in the test suite routes through ``make_balancer_weighted_pool``
/ ``make_balancer_stable_pool`` so the ``Pool`` handle is wired
through ``Bot::register_balancer_weighted_pool`` /
``register_balancer_stable_pool`` → ``get_pool`` → companion, matching the
``Bot.build_pool()`` flow (ADR-005).

Each call mints a FRESH ``Bot`` (via ``new_pool_bot``) so every built pool is
isolated. The register entry is get-or-register — a same address + same
family + same identity build answers the EXISTING handle — while a genuine
conflict (a different family, or a mismatched identity = wire drift) still
refuses loudly. Pools are MUTABLE, so isolation is the default; a caller that
WANTS to share one bot passes ``py_bot=`` explicitly.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.balancer.libraries.constants import PowVersion
from degenbot.balancer.libraries.scaling_helpers import _compute_scaling_factor
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import INVARIANT_V2, BalancerV2StablePool
from degenbot.checksum_cache import get_checksum_address
from tests.helpers.pool_bot import new_pool_bot

if TYPE_CHECKING:
    from degenbot.balancer.stable_pools import BalancerRateProvider
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.types.aliases import BlockNumber
    from tests.helpers.seam import Bot


def _pool_id_hex(pool_id: bytes) -> str:
    """Encode a 32-byte Balancer V2 pool_id as a lowercase hex string."""
    return "0x" + pool_id.hex()


def make_balancer_weighted_pool(
    address: str,
    *,
    pool_id: bytes,
    vault: str,
    tokens: list[Erc20Token] | tuple[Erc20Token, ...],
    balances: list[int] | tuple[int, ...],
    fee,
    weights: list[int] | tuple[int, ...],
    pow_version: int = 1,
    state_block: BlockNumber | None = None,
    py_bot: Bot | None = None,
    pool_class: type[BalancerV2Pool] = BalancerV2Pool,
) -> BalancerV2Pool:
    """Construct an I/O-free Balancer V2 weighted companion over a fresh Pool handle.

    Mints a fresh ``Bot`` unless ``py_bot`` is injected, so each pool is
    isolated. Registration is get-or-register (a same-identity re-build
    answers the EXISTING handle); a caller that wants to share one bot passes
    ``py_bot=``. The token companions passed in may live in a different
    ``Bot`` (their own ``make_erc20`` ``Bot``); that's fine — the pool reads
    balances from its own ``Bot`` and token metadata from the token handle,
    independently.

    ``Bot.build_pool()`` is the production path (registers in the session's
    shared ``_py_bot``); this helper is the test-only equivalent.

    The immutable config (tokens, weights, scaling_factors, fee, pow_version)
    stays Python-side on the companion; the mutable ``balances`` +
    ``update_block`` + the reorg journal live in Rust. The factory derives
    ``scaling_factors`` via the shared ``_compute_scaling_factor`` helper and
    forwards them to ``register_balancer_weighted_pool`` so the Rust core
    stores the same values the companion keeps (consumed by the future Rust
    ``WeightedMath``).
    """
    address_checksum = get_checksum_address(address)
    state_block_int = state_block if state_block is not None else 0

    bot = py_bot if py_bot is not None else new_pool_bot()

    scaling_factors = [_compute_scaling_factor(token) for token in tokens]
    # encode the Fraction fee the Python-side companion keeps target-perfect as
    # int(fee * FEE_DENOMINATOR) (1e18 denominator), mirroring on-chain swap
    # fee representation.
    fee_scaled_int = int(fee * BalancerV2Pool.FEE_DENOMINATOR)
    # The Rust core carries PowVersion as an opaque u8 (V1=1 / V2=2).
    pow_version_rust = (
        (1 if pow_version is PowVersion.V1 else 2)
        if isinstance(pow_version, PowVersion)
        else int(pow_version)
    )

    pool_id_int = bot.register_balancer_weighted_pool(
        address=address_checksum,
        vault=get_checksum_address(vault),
        pool_id_hex=_pool_id_hex(pool_id),
        tokens=[t.address for t in tokens],
        weights=list(weights),
        scaling_factors=scaling_factors,
        swap_fee=fee_scaled_int,
        pow_version=pow_version_rust,
        balances=list(balances),
        update_block=state_block_int,
    )
    handle: Pool | None = bot.get_pool(pool_id_int)
    assert handle is not None, "register_balancer_weighted_pool returned a pool_id with no handle"

    # ADR-005 sealed seam: register tokens in the same Bot as the pool.
    for tok in tokens:
        if bot.get_token(tok.address) is None:
            bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, tok.chain_id)
    return pool_class.from_handle(handle)


def make_balancer_stable_pool(
    address: str,
    *,
    pool_id: bytes,
    vault: str,
    tokens: list[Erc20Token] | tuple[Erc20Token, ...],
    balances: list[int] | tuple[int, ...],
    fee,
    amp: int,
    scaling_factors: list[int] | tuple[int, ...],
    bpt_idx: int | None = None,
    base_scaling_factors: list[int] | tuple[int, ...] | None = None,
    rate_provider: BalancerRateProvider | None = None,
    invariant_version: int = INVARIANT_V2,
    state_block: BlockNumber | None = None,
    py_bot: Bot | None = None,
    pool_class: type[BalancerV2StablePool] = BalancerV2StablePool,
) -> BalancerV2StablePool:
    """Construct an I/O-free Balancer V2 stable companion over a fresh Pool handle.

    Production-path twin of ``BalancerBuilder._build_stable``: mints a fresh
    ``Bot`` unless ``py_bot`` is injected, so each
    pool is isolated; registration is get-or-register (a same-identity
    re-build answers the EXISTING handle). A caller that wants to share one
    bot passes ``py_bot=``. The token companions may live in a different
    ``Bot``; that's fine — the pool reads balances from its own ``Bot`` and
    token metadata from the token handle.

    The immutable config (tokens, amp, scaling_factors, swap_fee, bpt_idx,
    invariant_version, base_scaling_factors, rate_provider) stays Python-side
    on the companion; the mutable ``balances`` + ``update_block`` + the reorg
    journal live in Rust. ``rate_provider`` is the per-block rate resolver
    (``BalancerRateProvider`` Protocol) — stays Python-side because it may be
    I/O-bound (the ``requires_io_at_calculation_time`` property flags this);
    the rate-cache-aware ``CacheAwareRateProvider`` test fixture plugs in
    here, mirroring on-chain ``_cacheTokenRateIfNecessary``.
    """
    address_checksum = get_checksum_address(address)
    state_block_int = state_block if state_block is not None else 0

    bot = py_bot if py_bot is not None else new_pool_bot()

    # The sealed seam ignores ``base_scaling_factors`` (the companion derives
    # base SF from token decimals in ``from_handle``); retained on the
    # factory signature for API compatibility with callers that pass it.

    # encode the Fraction fee the Python-side companion keeps, target-perfect
    # as ``int(fee * FEE_DENOMINATOR)`` (1e18 denominator), mirroring on-chain
    # swap fee representation. Forwarded to the Rust core for the (future,
    # 12e) ``StableMath`` port; the companion also re-derives it from
    # ``self.fee`` per-call (single source of truth: the Python Fraction).
    fee_scaled_int = int(fee * BalancerV2StablePool.FEE_DENOMINATOR)

    # ADR-006: tokens must be in the same Bot as the pool (the sealed seam
    # resolves Erc20Token companions off the handle via
    # balancer_stable().get_tokens, which requires registered tokens).
    for tok in tokens:
        if bot.get_token(tok.address) is None:
            bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, tok.chain_id)

    pool_id_int = bot.register_balancer_stable_pool(
        address=address_checksum,
        vault=get_checksum_address(vault),
        pool_id_hex=_pool_id_hex(pool_id),
        tokens=[t.address for t in tokens],
        amp=amp,
        scaling_factors=list(scaling_factors),
        swap_fee=fee_scaled_int,
        bpt_idx=bpt_idx,
        invariant_version=invariant_version,
        balances=list(balances),
        update_block=state_block_int,
        rate_provider=rate_provider,
    )
    handle: Pool | None = bot.get_pool(pool_id_int)
    assert handle is not None, "register_balancer_stable_pool returned a pool_id with no handle"

    return pool_class.from_handle(handle)


__all__ = [
    "BalancerV2Pool",
    "BalancerV2StablePool",
    "make_balancer_stable_pool",
    "make_balancer_weighted_pool",
]
