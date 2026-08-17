"""Curve StableSwap pool builder (sync)."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.builders.request import BuildPoolRequest, BuildRequest
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import (
    CurveStableswapPool,
    CurveStableswapPoolExternalUpdate,
)
from degenbot.curve.deployments import CURVE_V1_FACTORY_ADDRESS, CURVE_V1_REGISTRY_ADDRESS
from degenbot.exceptions.pool import BrokenPool
from degenbot.logging import logger

if TYPE_CHECKING:
    from degenbot.bot import RustBotIo
    from degenbot.builders.context import BuilderContext
    from degenbot.types import LiquidityPool
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId
    from degenbot.types.rpc_types import BlockIdentifier


_REGISTRY_ADDRESSES = (CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS)


class CurvePoolBuilder:
    """Builds and updates Curve StableSwap pools.

    Thin delegating shell (ADR-005): drives the Rust core's
    ``build_curve_pool`` (detection choreography + native ``RpcCurveDataProvider``),
    builds the ERC20 companion tokens, then wraps the registered handle via the
    single-arg ``_from_py_pool`` seam. No Python-side Curve detection remains.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        """Initialize the instance."""
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder
        self._py_bot = ctx.py_bot

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: RustBotIo,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Construct an I/O-free CurveStableswapPool via the Rust core.

        Delegates the full detection + registration to ``RustBot.build_curve_pool``
        (which attaches a native ``RpcCurveDataProvider`` and stores the complete
        identity in Rust state), builds the ERC20 token/LP companions so the
        handle's registration-gated getters resolve, then wraps the handle via
        ``_from_py_pool``.

        Returns:
            The computed value.

        Raises:
            BrokenPool: If the pool has fewer than 2 coins.

        """
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"
        state_block = (
            request.state_block if request.state_block is not None else io.get_block_number()
        )

        # Core registration: the Rust builder runs the full detection
        # choreography (coin discovery, A/fee/admin_fee, per-coin decimals,
        # ramping/crypto/lending/lp/metapool probes) and attaches a native
        # `RpcCurveDataProvider`. The complete identity is stored in Rust state,
        # so the single-arg `_from_py_pool(handle)` reads everything back off
        # the handle.
        pool_id = self._py_bot.build_curve_pool(
            pool_address,
            list(_REGISTRY_ADDRESSES),
            state_block,
        )
        handle = self._py_bot.get_pool(pool_id)
        assert handle is not None, "build_curve_pool returned a pool_id with no handle"

        # Metapool: build + register the base pool first so the metapool
        # handle's `curve_base_pool()` resolves for `_from_py_pool`.
        self._resolve_metapool_base(
            handle,
            chain_id=chain_id,
            state_block=state_block,
            request=request,
            io=io,
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
            self._erc20_builder.build(addr, chain_id=chain_id, silent=request.silent, io=io)

        # Metapool underlying + dedicated LP token companions — built purely
        # for registration (Rust already knows their addresses), so
        # `get_curve_tokens_underlying()` / `get_curve_lp_token()` resolve in
        # `_from_py_pool`.
        for addr in handle.curve_token_addresses_underlying() or ():
            self._erc20_builder.build(addr, chain_id=chain_id, silent=request.silent, io=io)
        lp_address = handle.curve_lp_token_address()
        if lp_address is not None:
            self._erc20_builder.build(
                lp_address,
                chain_id=chain_id,
                silent=request.silent,
                io=io,
            )

        pool = CurveStableswapPool._from_py_pool(handle)  # ruff:ignore[private-member-access]

        # Register pool
        self._pools.add(pool, chain_id=chain_id, pool_address=pool.address)

        if not request.silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Tokens: {[t.symbol for t in pool.tokens]}")
            logger.info(f"• A: {pool.a_coefficient}")
            logger.info(f"• Fee: {100 * pool.fee / pool.FEE_DENOMINATOR:.4f}%")

        return pool

    def _resolve_metapool_base(
        self,
        handle: LiquidityPool,
        *,
        chain_id: ChainId,
        state_block: int,
        request: BuildRequest,
        io: RustBotIo,
    ) -> None:
        """Build + register a metapool's base pool (recursion over the Rust path).

        The metapool handle already stores the base-pool address (Rust
        detection). Building the base pool recursively registers it in the same
        ``RustBot``, so the metapool handle's ``curve_base_pool()`` go-between
        resolves during ``_from_py_pool``.
        """
        base_address = handle.curve_base_pool_address()
        if base_address is None:
            return
        self.build(
            base_address,
            chain_id=chain_id,
            io=io,
            request=BuildPoolRequest(
                state_block=state_block,
                silent=request.silent,
                state_cache_depth=request.state_cache_depth,
            ),
        )

    @staticmethod
    def update(
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
        io: RustBotIo | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool.

        Returns:
            The computed value.

        Raises:
            TypeError: If the operation fails.

        """
        if not isinstance(pool, CurveStableswapPool):
            msg = f"CurvePoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert io is not None
        raw_block_number = block_number if block_number is not None else io.get_block_number()
        block_number_: int = (
            raw_block_number if isinstance(raw_block_number, int) else int(raw_block_number)
        )

        # Fetch balances for each token in the pool
        # ADR-005 slice 14s: delegate the full loop to Rust (RustBotIo is the
        # only executor; the Python parity-gate fallback is retired).
        balances_result = io.fetch_curve_balances(
            pool.address,
            len(pool.tokens),
            block=block_number_,
        )
        new_balances = [int(b) for b in balances_result]

        if pool.balances == tuple(new_balances):
            return False

        update = CurveStableswapPoolExternalUpdate(
            block_number=block_number_,
            balances=tuple(new_balances),
        )
        pool.external_update(update)
        return True
