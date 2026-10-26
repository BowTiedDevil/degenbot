"""Curve StableSwap pool builder (sync)."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.builders.request import BuildPoolRequest, BuildRequest
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.deployments import CURVE_V1_FACTORY_ADDRESS, CURVE_V1_REGISTRY_ADDRESS
from degenbot.exceptions.pool import BrokenPool
from degenbot.logging import logger

if TYPE_CHECKING:
    from degenbot._ffi import BotIo
    from degenbot.builders.context import BuilderContext
    from degenbot.types import Pool
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
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

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: BotIo,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Construct an I/O-free CurveStableswapPool through the ONE core entry.

        Delegates construction to ``Bot.construct_pool``'s Curve arm (the core
        ``build_curve_pool`` detection choreography + native
        ``RpcCurveDataProvider``, identity stored in Rust state), registers the
        ERC20 token/LP companions so the handle's registration-gated getters
        resolve, then wraps the handle via ``from_handle``.

        Returns:
            The computed value.

        """
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"
        state_block = (
            request.state_block if request.state_block is not None else io.get_block_number()
        )

        # The ONE core construction entry (its Curve arm) — never a second
        # Python construction path.
        outcome = self._py_bot.construct_pool(
            pool_address,
            family="stableswap",
            block=state_block,
            registries=list(_REGISTRY_ADDRESSES),
        )
        handle = outcome.pool

        self.finalize(
            handle,
            chain_id=chain_id,
            state_block=state_block,
            silent=request.silent,
            io=io,
        )

        pool = CurveStableswapPool.from_handle(handle)

        # Register pool
        self._pools.add(pool, chain_id=chain_id, pool_address=pool.address)

        if not request.silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Tokens: {[t.symbol for t in pool.tokens]}")
            logger.info(f"• A: {pool.a_coefficient}")
            logger.info(f"• Fee: {100 * pool.fee / pool.FEE_DENOMINATOR:.4f}%")

        return pool

    def finalize(
        self,
        handle: Pool,
        *,
        chain_id: ChainId,
        state_block: int,
        silent: bool,
        io: BotIo,
    ) -> None:
        """Register a Curve pool's companions on the already-constructed handle.

        Runs the metapool base-pool recursion (so the metapool handle's
        ``curve_base_pool()`` resolves) and builds the coin / underlying / LP
        ERC20 companions (so the handle's registration-gated getters resolve).
        Construction itself is the ONE core entry's now; this is the companion
        choreography only, shared by the facade's build path and this builder's
        own entry.

        Raises:
            BrokenPool: If the pool has fewer than 2 coins.

        """
        # Metapool: build + register the base pool first so the metapool
        # handle's `curve_base_pool()` resolves for `from_handle`.
        base_address = handle.curve_base_pool_address()
        if base_address is not None:
            self.build(
                base_address,
                chain_id=chain_id,
                io=io,
                request=BuildPoolRequest(state_block=state_block, silent=silent),
            )

        # ERC20 companions for the pool coins, sourced from the handle's raw
        # token addresses (the registration-gated `get_curve_tokens` returns
        # None until these are built). Skip broken pools (< 2 coins).
        token_addresses = handle.curve_token_addresses()
        assert token_addresses is not None
        min_tokens = 2
        if len(token_addresses) < min_tokens:
            raise BrokenPool
        for addr in token_addresses:
            self._erc20_builder.build(addr, chain_id=chain_id, silent=silent, io=io)

        # Metapool underlying + dedicated LP token companions — built purely
        # for registration (Rust already knows their addresses), so
        # `get_curve_tokens_underlying()` / `get_curve_lp_token()` resolve in
        # `from_handle`.
        for addr in handle.curve_token_addresses_underlying() or ():
            self._erc20_builder.build(addr, chain_id=chain_id, silent=silent, io=io)
        lp_address = handle.curve_lp_token_address()
        if lp_address is not None:
            self._erc20_builder.build(
                lp_address,
                chain_id=chain_id,
                silent=silent,
                io=io,
            )
