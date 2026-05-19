"""AsyncBot — the async counterpart to Bot.

Owns an AsyncConnectionManager and provides async factory/I-O methods.
Returns the same I/O-free domain objects as Bot.
"""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
from web3 import Web3

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.builders.async_context import AsyncBuilderContext
from degenbot.builders.async_erc20_builder import AsyncErc20Builder
from degenbot.builders.async_v2_pool_builder import AsyncV2PoolBuilder
from degenbot.builders.async_v3_pool_builder import AsyncV3PoolBuilder
from degenbot.builders.async_v4_pool_builder import AsyncV4PoolBuilder
from degenbot.builders.pool_io import AsyncPoolIO
from degenbot.builders.type_resolution import (
    pool_class_for_descriptor,
)
from degenbot.builders.type_resolution import (
    resolve_pool_type_async as _resolve_pool_type_async_impl,
)
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.connection.async_connection_manager import AsyncConnectionManager
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import TrackerAlreadyInitialized
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolTracker
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS as _FACTORY_DEPLOYMENTS
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from collections.abc import Sequence

    from eth_typing import ChecksumAddress
    from web3.types import BlockIdentifier

    from degenbot.builders.protocol import AsyncPoolBuilder
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.provider.interface import AsyncProviderAdapter
    from degenbot.types.aliases import ChainId


class AsyncBot:
    """
    Async session object that owns the runtime state for a degenbot run.

    Mirrors Bot with AsyncConnectionManager and async factory/I-O methods.
    Returns the same I/O-free domain objects as Bot.
    """

    def __init__(self, config: DegenbotConfig) -> None:
        self.config = config
        self.connections = AsyncConnectionManager()
        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=config.database.path)
        )
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._trackers: dict[tuple[ChainId, str], Any] = {}

        # Async builders own I/O orchestration; AsyncBot hands them its I/O dependencies.
        # AsyncErc20Builder is a leaf — constructed before AsyncBuilderContext.
        self._erc20_builder = AsyncErc20Builder(
            default_chain_id=None, db=self.db, tokens=self.tokens
        )
        ctx = AsyncBuilderContext(
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
            default_chain_id=None,
            managed_pools=self.managed_pools,
        )
        self._v2_builder = AsyncV2PoolBuilder(ctx)
        self._v3_builder = AsyncV3PoolBuilder(ctx)
        self._v4_builder = AsyncV4PoolBuilder(ctx)

        # Builder registry: concrete pool type → builder
        self._async_builders: dict[type, AsyncPoolBuilder] = {}
        self._register_builder(UniswapV2Pool, self._v2_builder)
        self._register_builder(UniswapV3Pool, self._v3_builder)
        self._register_builder(UniswapV4Pool, self._v4_builder)
        self._register_builder(AerodromeV2Pool, self._v2_builder)
        self._register_builder(CamelotLiquidityPool, self._v2_builder)

    @classmethod
    def from_config_file(cls) -> AsyncBot:
        return cls(config=_init_config())

    def _register_builder(
        self,
        pool_class: type,
        builder: AsyncPoolBuilder,
    ) -> None:
        """Register an async builder for a concrete pool type."""
        self._async_builders[pool_class] = builder

    def add_tracker[M: AbstractPoolTracker[Any]](
        self,
        manager_cls: type[M],
        *args: Any,
        **kwargs: Any,
    ) -> M:
        """Add a pool manager to this bot session. Same as Bot.add_tracker."""
        # Inject bot reference
        kwargs["bot"] = self

        # Enforce one manager per (chain_id, factory) within this Bot
        chain_id = kwargs.get("chain_id")
        factory_address = kwargs.get("factory_address")
        if chain_id is not None and factory_address is not None:
            key = (chain_id, get_checksum_address(factory_address))
            if key in self._trackers:
                raise TrackerAlreadyInitialized(
                    message=(
                        f"A {manager_cls.__name__} is already registered"
                        f" for chain {chain_id}, factory {factory_address}"
                    )
                )
            manager = manager_cls(*args, **kwargs)
            self._trackers[key] = manager
            return manager

        return manager_cls(*args, **kwargs)

    # ------------------------------------------------------------------
    # ERC-20 token factory
    # ------------------------------------------------------------------

    async def build_erc20token(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token."""
        resolved_chain_id = chain_id or self.connections.default_chain_id
        io = AsyncPoolIO(self.connections.get_provider(resolved_chain_id))
        return await self._erc20_builder.build(address, chain_id=resolved_chain_id, silent=silent, io=io)

    def get_token(self, address: str, *, chain_id: ChainId | None = None) -> Erc20Token | None:
        """Get a token from the registry (sync — no async I/O)."""
        chain_id = chain_id or self.connections.default_chain_id
        return self.tokens.get(token_address=address, chain_id=chain_id)

    # ------------------------------------------------------------------
    # Pool factory
    # ------------------------------------------------------------------

    async def build_pool(
        self,
        address: str,
        *,
        pool_id: str | bytes | None = None,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        silent: bool = False,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        tick_bitmap: dict[int, Any] | None = None,
        tick_data: dict[int, Any] | None = None,
        state_cache_depth: int = 8,
        # V4-specific kwargs
        state_view_address: str | None = None,
        tokens: Sequence[str] | None = None,
        fee: int | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
    ) -> AbstractLiquidityPool:
        """
        Build a pool from an address, automatically resolving its type.

        When `pool_id` is provided, `address` is interpreted as a V4 PoolManager
        contract. Without it, `address` is a pool contract and the type is resolved
        from the pool registry, database, or factory address.
        """
        address = get_checksum_address(address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)
        io = AsyncPoolIO(provider)

        # V4 fast path: pool_id discriminates V4 managed pools
        if pool_id is not None:
            v4_kwargs: dict[str, Any] = {
                "pool_id": pool_id,
                "pool_manager_address": address,
                "chain_id": chain_id,
                "state_block": state_block,
                "silent": silent,
            }
            if state_view_address is not None:
                v4_kwargs["state_view_address"] = state_view_address
            if tokens is not None:
                v4_kwargs["tokens"] = tokens
            if fee is not None:
                v4_kwargs["fee"] = fee
            if tick_spacing is not None:
                v4_kwargs["tick_spacing"] = tick_spacing
            if hook_address is not None:
                v4_kwargs["hook_address"] = hook_address
            if tick_bitmap is not None:
                v4_kwargs["tick_bitmap"] = tick_bitmap
            if tick_data is not None:
                v4_kwargs["tick_data"] = tick_data
            return await self._v4_builder.build(address, io=io, **v4_kwargs)

        # Check pool registry — return existing pool if already built
        existing = self.pools.get(chain_id=chain_id, pool_address=address)
        if existing is not None:
            return existing

        # Resolve the pool type and dispatch to the appropriate builder
        try:
            pool_type = await _resolve_pool_type_async_impl(
                address, chain_id=chain_id, io=io, db=self.db
            )
        except DegenbotValueError:
            # Fallback: no Curve async builder yet — raise
            msg = f"Cannot resolve pool type for address {address} on chain {chain_id}"
            raise DegenbotValueError(message=msg) from None

        # Look up the concrete pool class from the registry
        pool_class = pool_class_for_descriptor(pool_type, chain_id=chain_id)
        builder = self._async_builders.get(pool_class)
        if builder is None:
            # Fallback: walk MRO of pool_class looking for a registered builder
            for base in pool_class.__mro__:
                builder = self._async_builders.get(base)
                if builder is not None:
                    break

        if builder is None:
            raise DegenbotValueError(
                message=f"No async builder for pool class {pool_class.__name__}"
            )

        # Build kwargs dict — only include non-None optional params
        dispatch_kwargs: dict[str, Any] = {
            "silent": silent,
            "state_cache_depth": state_cache_depth,
        }
        if deployer_address is not None:
            dispatch_kwargs["deployer_address"] = deployer_address
        if init_hash is not None:
            dispatch_kwargs["init_hash"] = init_hash
        if state_block is not None:
            dispatch_kwargs["state_block"] = state_block
        if tick_bitmap is not None:
            dispatch_kwargs["tick_bitmap"] = tick_bitmap
        if tick_data is not None:
            dispatch_kwargs["tick_data"] = tick_data

        return await self._dispatch_build(
            builder=builder,
            address=address,
            chain_id=chain_id,
            io=io,
            **dispatch_kwargs,
        )

    @staticmethod
    async def _dispatch_build(
        *,
        builder: AsyncPoolBuilder,
        address: ChecksumAddress,
        chain_id: ChainId,
        **kwargs: Any,
    ) -> AbstractLiquidityPool:
        """Dispatch to the async builder, forwarding all kwargs."""
        return await builder.build(address, chain_id=chain_id, **kwargs)

    # ------------------------------------------------------------------
    # Pool type resolution (async counterpart of Bot's resolution)
    # ------------------------------------------------------------------

    # ------------------------------------------------------------------
    # Deployment resolution helper
    # ------------------------------------------------------------------

    @staticmethod
    def _resolve_deployment(
        *,
        chain_id: ChainId,
        factory: ChecksumAddress,
        default_init_hash: str,
        deployer_address: str | None = None,
        init_hash: str | None = None,
    ) -> tuple[str, str]:
        """Resolve deployer address and pool init-hash from factory deployments."""
        resolved_deployer: str = factory
        resolved_init_hash = default_init_hash
        with contextlib.suppress(KeyError):
            factory_deployment = _FACTORY_DEPLOYMENTS[chain_id][factory]
            resolved_init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:
                resolved_deployer = get_checksum_address(factory_deployment.deployer)

        resolved_deployer = (
            get_checksum_address(deployer_address)
            if deployer_address is not None
            else resolved_deployer
        )
        resolved_init_hash = init_hash or resolved_init_hash
        return resolved_deployer, resolved_init_hash

    # ------------------------------------------------------------------
    # I/O methods (retain async query methods)
    # ------------------------------------------------------------------

    async def get_token_balance(
        self,
        token_address: str,
        holder_address: str,
        *,
        chain_id: ChainId | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address."""
        token_address = get_checksum_address(token_address)
        holder_address = get_checksum_address(holder_address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        token = self.tokens.get(token_address=token_address, chain_id=chain_id)
        if token is None:
            token = await self.build_erc20token(token_address, chain_id=chain_id)

        block_number = await self._resolve_block_number(provider, block_identifier)

        # Check cache
        if (balance := token.get_cached_balance(holder_address, block_number)) is not None:
            return balance

        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await provider.call(
                to=token.address,
                data=Web3.keccak(text="balanceOf(address)")[:4]
                + eth_abi.abi.encode(types=["address"], args=[holder_address]),
                block=block_number,
            ),
        )

        token.set_cached_balance(holder_address, block_number, cast("int", balance))
        return cast("int", balance)

    async def get_token_approval(
        self,
        token_address: str,
        owner: str,
        spender: str,
        *,
        chain_id: ChainId | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`."""
        token_address = get_checksum_address(token_address)
        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        token = self.tokens.get(token_address=token_address, chain_id=chain_id)
        if token is None:
            token = await self.build_erc20token(token_address, chain_id=chain_id)

        block_number = await self._resolve_block_number(provider, block_identifier)

        # Check cache
        if (approval := token.get_cached_approval(block_number, owner, spender)) is not None:
            return approval

        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await provider.call(
                to=token.address,
                data=Web3.keccak(text="allowance(address,address)")[:4]
                + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                block=block_number,
            ),
        )

        token.set_cached_approval(block_number, owner, spender, cast("int", approval))
        return cast("int", approval)

    async def get_token_total_supply(
        self,
        token_address: str,
        *,
        chain_id: ChainId | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token."""
        token_address = get_checksum_address(token_address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        token = self.tokens.get(token_address=token_address, chain_id=chain_id)
        if token is None:
            token = await self.build_erc20token(token_address, chain_id=chain_id)

        block_number = await self._resolve_block_number(provider, block_identifier)

        # Check cache
        if (total_supply := token.get_cached_total_supply(block_number)) is not None:
            return total_supply

        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await provider.call(
                to=token.address,
                data=Web3.keccak(text="totalSupply()")[:4],
                block=block_number,
            ),
        )

        token.set_cached_total_supply(block_number, cast("int", total_supply))
        return cast("int", total_supply)

    async def get_ether_balance(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address."""
        address = get_checksum_address(address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)
        block = block_identifier if isinstance(block_identifier, int) else None
        return await provider.get_balance(address, block=block)

    @staticmethod
    async def _resolve_block_number(
        provider: AsyncProviderAdapter, block_identifier: BlockIdentifier | None
    ) -> int:
        """Resolve a block identifier to a block number."""
        if block_identifier is None:
            return await provider.get_block_number()
        if isinstance(block_identifier, int):
            return block_identifier
        return await provider.get_block_number()

    def get_provider(self, *, chain_id: ChainId) -> AsyncProviderAdapter:
        return self.connections.get_provider(chain_id)
