"""Factory helpers for I/O-free Balancer V2 pool construction in tests.

Mirrors ``tests/helpers/curve_pool_factory.py`` (and ``v3_pool_factory.py`` /
``v4_pool_factory.py``) — ADR-005 slice 12b: every direct
``BalancerV2Pool(...)`` construction in the test suite routes through
``make_balancer_weighted_pool`` so the ``PyLiquidityPool`` handle is wired
through ``PyBot::register_balancer_weighted_pool`` → ``get_pool`` →
companion, matching the ``Bot.build_pool()`` flow (ADR-005).

Each call creates its own short-lived ``PyBot`` (the returned handle holds an
``Arc`` clone of the underlying ``Bot``, so it outlives the ``PyBot``) — so
each test pool is fully isolated (no shared mutable state across tests).

Sub-slice scope: weighted variant only (ADR-005 slice 12b). The stable
variant factory (``make_balancer_stable_pool``) lands in slice 12d alongside
the stable companion rewrite.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.balancer.libraries.scaling_helpers import _compute_scaling_factor
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.builders.balancer_builder_base import BalancerBuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot

if TYPE_CHECKING:
    from degenbot.degenbot_rs import PyLiquidityPool
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.aliases import BlockNumber, ChainId


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
    chain_id: ChainId | None = None,
    state_block: BlockNumber | None = None,
    py_bot: PyBot | None = None,
    pool_class: type[BalancerV2Pool] = BalancerV2Pool,
) -> BalancerV2Pool:
    """Construct an I/O-free Balancer V2 weighted companion over a fresh PyLiquidityPool handle.

    Registers the pool in a short-lived ``PyBot`` (the returned handle holds an
    ``Arc`` clone of the underlying ``Bot``, so it outlives the ``PyBot``) — so
    each test pool is fully isolated (no shared mutable state across tests).
    The token companions passed in may live in a different ``Bot`` (their own
    ``make_erc20`` ``PyBot``); that's fine — the pool reads balances from its
    own ``Bot`` and token metadata from the token handle, independently.

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
    resolved_chain_id = chain_id if chain_id is not None else tokens[0].chain_id
    state_block_int = state_block if state_block is not None else 0

    bot = py_bot if py_bot is not None else PyBot()

    scaling_factors = [_compute_scaling_factor(token) for token in tokens]
    # encode the Fraction fee the Python-side companion keeps target-perfect as
    # int(fee * FEE_DENOMINATOR) (1e18 denominator), mirroring on-chain swap
    # fee representation.
    fee_scaled_int = int(fee * BalancerV2Pool.FEE_DENOMINATOR)
    pow_version_rust = BalancerBuilderBase.pow_version_to_rust(pow_version)

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
    handle: PyLiquidityPool | None = bot.get_pool(pool_id_int)
    assert handle is not None, "register_balancer_weighted_pool returned a pool_id with no handle"

    return pool_class(
        handle,
        address=address_checksum,
        pool_id=pool_id,
        vault=vault,
        tokens=tokens,
        fee=fee,
        weights=weights,
        pow_version=pow_version,
        chain_id=resolved_chain_id,
        state_block=state_block_int,
    )


__all__ = [
    "BalancerV2Pool",
    "make_balancer_weighted_pool",
]
