"""Curve StableSwap pool builder (sync)."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.builders.construction import request_for
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.deployments import CURVE_V1_FACTORY_ADDRESS, CURVE_V1_REGISTRY_ADDRESS
from degenbot.exceptions.pool import BrokenPool
from degenbot.types.pool_type import PoolFamily

if TYPE_CHECKING:
    from degenbot.builders.context import BuilderContext
    from degenbot.types import Pool
    from degenbot.types.aliases import ChainId


_REGISTRY_ADDRESSES = (CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS)


class CurvePoolBuilder:
    """Builds Curve StableSwap pools.

    Thin delegating shell (ADR-005): construction runs through the ONE core
    entry (``Bot.construct_pool``'s Curve arm, which drives the core
    ``build_curve_pool`` detection choreography + native ``RpcCurveDataProvider``),
    then the ERC20 / LP-token companions are registered so the handle's
    registration-gated getters resolve, and the handle is wrapped via the
    single-arg ``from_handle`` seam. No Python-side Curve detection remains.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        """Initialize the instance."""
        self._default_chain_id = ctx.default_chain_id
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder
        self._py_bot = ctx.py_bot

    def finalize(
        self,
        handle: Pool,
        *,
        chain_id: ChainId,
        state_block: int,
        silent: bool,
    ) -> None:
        """Register a Curve pool's companions on the already-constructed handle.

        Runs the metapool base-pool recursion (so the metapool handle's
        ``curve().base_pool()`` resolves) and builds the coin / underlying / LP
        ERC20 companions (so the handle's registration-gated getters resolve).
        Construction itself is the ONE core entry's now; this is the companion
        choreography only, shared by the facade's build path and this builder's
        own entry.

        Raises:
            BrokenPool: If the pool has fewer than 2 coins.

        """
        # The Curve arm, narrowed once: every address read below is typed Curve
        # state, so a wrong-family handle refuses here instead of answering a
        # reserved-family sentinel.
        arm = handle.curve()

        # Metapool: build + register the base pool first so the metapool
        # handle's `curve().base_pool()` resolves for `from_handle`.
        base_address = arm.base_pool_address()
        if base_address is not None:
            # The base pool's construction is the driver's composition, not
            # this pool's arm (ADR-085 D5): the ONE core entry's Curve arm,
            # then this same companion phase, recursively.
            base_handle = self._py_bot.construct_pool(
                request_for(
                    get_checksum_address(base_address),
                    family=PoolFamily.STABLESWAP,
                    variant=None,
                    block=state_block,
                    db=True,
                    registries=_REGISTRY_ADDRESSES,
                )
            ).pool
            self.finalize(
                base_handle,
                chain_id=chain_id,
                state_block=state_block,
                silent=silent,
            )
            self._pools.add(
                CurveStableswapPool.from_handle(base_handle),
                chain_id=chain_id,
                pool_address=get_checksum_address(base_address),
            )

        # ERC20 companions for the pool coins, sourced from the curve arm's raw
        # token addresses (the registration-gated `curve().get_tokens` returns
        # None until these are built). Skip broken pools (< 2 coins).
        token_addresses = arm.token_addresses()
        min_tokens = 2
        if len(token_addresses) < min_tokens:
            raise BrokenPool
        for addr in token_addresses:
            self._erc20_builder.build(addr, chain_id=chain_id, silent=silent)

        # Metapool underlying + dedicated LP token companions — built purely
        # for registration (Rust already knows their addresses), so
        # `curve().get_tokens_underlying()` / `curve().get_lp_token()` resolve
        # in `from_handle`.
        for addr in arm.token_addresses_underlying() or ():
            self._erc20_builder.build(addr, chain_id=chain_id, silent=silent)
        lp_address = arm.lp_token_address()
        if lp_address is not None:
            self._erc20_builder.build(
                lp_address,
                chain_id=chain_id,
                silent=silent,
            )
