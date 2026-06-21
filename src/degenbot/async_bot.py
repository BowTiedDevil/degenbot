"""AsyncBot — the async counterpart to Bot.

Single-chain (ADR-006 D5): one ``AsyncProviderAdapter`` per Bot, the chain
identity from ``config.default_chain_id``. Returns the same I/O-free domain
objects as Bot.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Self

from hexbytes import HexBytes

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.bot_lifecycle import close as _close_handles
from degenbot.bot_lifecycle import (
    release_python_state as _release_python_state,
)
from degenbot.builders.async_context import AsyncBuilderContext
from degenbot.builders.async_erc20_builder import AsyncErc20Builder
from degenbot.builders.async_v2_pool_builder import AsyncV2PoolBuilder
from degenbot.builders.async_v3_pool_builder import AsyncV3PoolBuilder
from degenbot.builders.async_v4_pool_builder import AsyncV4PoolBuilder
from degenbot.builders.pool_io import AsyncPoolIO
from degenbot.builders.request import BuildManagedPoolRequest, BuildPoolRequest, BuildRequest
from degenbot.builders.type_resolution import (
    pool_class_for_descriptor,
)
from degenbot.builders.type_resolution import (
    resolve_pool_type_async as _resolve_pool_type_async_impl,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.degenbot_rs import PyBot
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import TrackerAlreadyInitialized
from degenbot.provider.factory import get_async_provider_from_config
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolTracker
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from collections.abc import Sequence

    from eth_typing import ChecksumAddress
    from web3.types import BlockIdentifier

    from degenbot.builders.protocol import AsyncPoolBuilder
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.provider.async_adapter import AsyncProviderAdapter
    from degenbot.types.aliases import ChainId


class AsyncBot:
    """Async session object — single-chain facade over one AsyncProviderAdapter."""

    def __init__(
        self,
        config: DegenbotConfig,
        *,
        provider: AsyncProviderAdapter,
    ) -> None:
        """Initialize the single-chain async Bot session.

        One Bot per chain (ADR-006 D5). The chain identity comes from
        ``config.default_chain_id``. The provider's ``eth_chainId`` is NOT
        eagerly enforced here — async providers can't read ``chain_id``
        synchronously (their sync ``chain_id`` property raises
        ``NotImplementedError``). Construction routes through the awaited
        factories (:meth:`from_provider` / :meth:`from_config_file_async`) so
        the ``await provider.get_chain_id()`` check runs once, at build time,
        before any pool/token I/O.

        Raises:
            DegenbotValueError: If ``config.default_chain_id`` is ``None``.

        """
        self.config = config

        if config.default_chain_id is None:
            msg = (
                "AsyncBot requires a default_chain_id in the config. Set "
                "`default_chain_id` in your config file or pass a config with it set."
            )
            raise DegenbotValueError(message=msg)
        self._chain_id: ChainId = config.default_chain_id
        self._provider = provider

        # Polars-inspired three-layer architecture (ADR-005).
        self._py_bot = PyBot()

        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=config.database.path)
        )
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._trackers: dict[str, Any] = {}
        # Idempotency flag for close(); mirrored by bot_lifecycle.close.
        self._closed: bool = False

        self._erc20_builder = AsyncErc20Builder(
            default_chain_id=self._chain_id, db=self.db, tokens=self.tokens, py_bot=self._py_bot
        )
        ctx = AsyncBuilderContext(
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
            py_bot=self._py_bot,
            default_chain_id=self._chain_id,
            managed_pools=self.managed_pools,
        )
        self._v2_builder = AsyncV2PoolBuilder(ctx)
        self._v3_builder = AsyncV3PoolBuilder(ctx)
        self._v4_builder = AsyncV4PoolBuilder(ctx)

        self._async_builders: dict[type, AsyncPoolBuilder] = {}
        self._register_builder(LiquidityPool, self._v2_builder)
        self._register_builder(UniswapV3Pool, self._v3_builder)
        self._register_builder(UniswapV4Pool, self._v4_builder)
        self._register_builder(AerodromeV2Pool, self._v2_builder)

    @classmethod
    async def from_provider(
        cls,
        config: DegenbotConfig,
        *,
        provider: AsyncProviderAdapter,
    ) -> AsyncBot:
        """Build an AsyncBot from a config + an injected async provider.

        The single enforcement seam for async providers: awaits
        ``provider.get_chain_id()`` and compares to ``config.default_chain_id``
        (fail-fast on a misconfigured endpoint — async providers can't read
        ``chain_id`` synchronously, so this runs once at construction, before
        any pool/token I/O).

        Returns:
            An AsyncBot bound to ``provider``.

        Raises:
            DegenbotValueError: If ``config.default_chain_id`` is ``None`` or
                the provider's ``chain_id`` mismatches the configured chain.

        """
        chain_id = config.default_chain_id
        if chain_id is None:
            msg = (
                "AsyncBot requires a default_chain_id in the config. Set "
                "`default_chain_id` in your config file or pass a config with it set."
            )
            raise DegenbotValueError(message=msg)
        actual = await provider.get_chain_id()
        if actual != chain_id:
            msg = (
                f"Provider chain_id ({actual}) does not match the configured "
                f"default_chain_id ({chain_id}). Refusing to start an AsyncBot "
                f"against the wrong chain."
            )
            raise DegenbotValueError(message=msg)
        return cls(config, provider=provider)

    @classmethod
    async def from_config_file_async(cls) -> AsyncBot:
        """Build an AsyncBot from the config file's default chain.

        Constructs the async provider from ``config.rpc[default_chain_id]``
        (via :func:`get_async_provider_from_config`, which enforces the
        connected RPC's ``eth_chainId`` matches).

        Returns:
            An AsyncBot built from the config file's default chain.

        Raises:
            DegenbotValueError: If ``config.default_chain_id`` is ``None``.

        """
        config = _init_config()
        chain_id = config.default_chain_id
        if chain_id is None:
            msg = (
                "AsyncBot requires a default_chain_id in the config. Set "
                "`default_chain_id` in your config file or pass a config with it set."
            )
            raise DegenbotValueError(message=msg)
        provider = await get_async_provider_from_config(chain_id=chain_id, config=config)
        return cls(config, provider=provider)

    @property
    def chain_id(self) -> ChainId:
        """The single chain this AsyncBot targets."""
        return self._chain_id

    @property
    def provider(self) -> AsyncProviderAdapter:
        """The single async provider for this Bot's chain."""
        return self._provider

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
        """Add a pool manager to this bot session. Same as Bot.add_tracker.

        Returns:
            The computed value.

        Raises:
            TrackerAlreadyInitialized: See function documentation.

        """
        kwargs["bot"] = self
        factory_address = kwargs.get("factory_address")
        if factory_address is not None:
            key = get_checksum_address(factory_address)
            if key in self._trackers:
                raise TrackerAlreadyInitialized(
                    message=(
                        f"A {manager_cls.__name__} is already registered"
                        f" for factory {factory_address}"
                    )
                )
            manager = manager_cls(*args, **kwargs)
            self._trackers[key] = manager
            return manager

        return manager_cls(*args, **kwargs)

    # ------------------------------------------------------------------
    # Lifecycle: Python-handle teardown (shared with Bot via bot_lifecycle)
    # ------------------------------------------------------------------

    def release_python_state(self) -> None:
        """Drop Python-side pool/token/tracker caches once Rust owns canonical state.

        Mid-lifecycle handshake mirroring :meth:`Bot.release_python_state`.
        See :func:`degenbot.bot_lifecycle.release_python_state` for details.
        """
        _release_python_state(self)

    def aclose(self) -> None:
        """Release all Python handles owned by this AsyncBot session.

        End-of-life teardown mirroring :meth:`Bot.close` (release python state,
        remove the scoped DB session, close the provider, drop refs). The async
        provider's ``close()`` is synchronous, so ``aclose`` is itself
        synchronous — async-ness is only required at the ``__aexit__`` boundary.
        Idempotent via the ``_closed`` flag.
        """
        _close_handles(self)

    def __enter__(self) -> Self:
        """Enter the (sync) context manager.

        Returns:
            This AsyncBot, so callers can bind it: ``with AsyncBot(...) as bot:``.

        """
        return self

    def __exit__(self, *exc: object) -> None:
        """Exit the (sync) context manager; release all Python handles. Never suppresses."""
        self.aclose()

    async def __aenter__(self) -> Self:
        """Enter the async context manager.

        Returns:
            This AsyncBot: ``async with AsyncBot.from_provider(...) as bot:``.

        """
        return self

    async def __aexit__(self, *exc: object) -> None:
        """Exit the async context manager; release all Python handles. Never suppresses."""
        self.aclose()

    # ------------------------------------------------------------------
    # ERC-20 token factory
    # ------------------------------------------------------------------

    async def build_erc20token(
        self,
        address: str,
        *,
        silent: bool = False,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token.

        Returns:
            The computed value.

        """
        io = AsyncPoolIO(self._provider)
        return await self._erc20_builder.build(
            address, chain_id=self._chain_id, silent=silent, io=io
        )

    def get_token(self, address: str) -> Erc20Token | None:
        """Get a token from the registry (sync — no async I/O).

        Returns:
            The computed value.

        """
        return self.tokens.get(token_address=address, chain_id=self._chain_id)

    # ------------------------------------------------------------------
    # Pool factory
    # ------------------------------------------------------------------

    async def build_pool(
        self,
        address: str,
        *,
        state_block: int | None = None,
        silent: bool = False,
        tick_bitmap: dict[int, Any] | None = None,
        tick_data: dict[int, Any] | None = None,
        state_cache_depth: int = 8,
    ) -> AbstractLiquidityPool:
        """Build a pool from an address, automatically resolving its type.

        V4 managed pools should use ``build_managed_pool()`` instead.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: See function documentation.

        """
        address = get_checksum_address(address)
        chain_id = self._chain_id
        io = AsyncPoolIO(self._provider)

        request = BuildPoolRequest(
            silent=silent,
            state_block=state_block,
            state_cache_depth=state_cache_depth,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
        )

        existing = self.pools.get(chain_id=chain_id, pool_address=address)
        if existing is not None:
            return existing

        try:
            pool_type = await _resolve_pool_type_async_impl(
                address, chain_id=chain_id, io=io, db=self.db
            )
        except DegenbotValueError:
            msg = f"Cannot resolve pool type for address {address} on chain {chain_id}"
            raise DegenbotValueError(message=msg) from None

        pool_class = pool_class_for_descriptor(pool_type, chain_id=chain_id)
        builder = self._async_builders.get(pool_class)
        if builder is None:
            for base in pool_class.__mro__:
                builder = self._async_builders.get(base)
                if builder is not None:
                    break

        if builder is None:
            raise DegenbotValueError(
                message=f"No async builder for pool class {pool_class.__name__}"
            )

        return await self._dispatch_build(
            builder=builder,
            address=address,
            chain_id=chain_id,
            io=io,
            request=request,
        )

    @staticmethod
    async def _dispatch_build(
        *,
        builder: AsyncPoolBuilder,
        address: ChecksumAddress,
        chain_id: ChainId,
        io: AsyncPoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Dispatch to the async builder with a typed request.

        Returns:
            The computed value.

        """
        return await builder.build(address, chain_id=chain_id, io=io, request=request)

    # ------------------------------------------------------------------
    # V4 managed pool factory
    # ------------------------------------------------------------------

    async def build_managed_pool(
        self,
        address: str,
        pool_id: str | bytes,
        *,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
        state_view_address: str | None = None,
        tokens: Sequence[str] | None = None,
        fee: int | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
        tick_bitmap: dict[int, Any] | None = None,
        tick_data: dict[int, Any] | None = None,
    ) -> UniswapV4Pool:
        """Build a V4 managed pool from a PoolManager address and pool ID.

        Returns:
            The computed value.

        """
        address = get_checksum_address(address)
        chain_id = self._chain_id

        pool_id_bytes = HexBytes(pool_id)
        existing = self.managed_pools.get(
            chain_id=chain_id,
            pool_manager_address=address,
            pool_id=pool_id_bytes,
        )
        if existing is not None:
            if TYPE_CHECKING:
                assert isinstance(existing, UniswapV4Pool)
            return existing

        io = AsyncPoolIO(self._provider)

        request = BuildManagedPoolRequest(
            pool_id=pool_id,
            silent=silent,
            state_block=state_block,
            state_cache_depth=state_cache_depth,
            state_view_address=state_view_address,
            tokens=tokens,
            fee=fee,
            tick_spacing=tick_spacing,
            hook_address=hook_address,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
        )

        return await self._dispatch_build(  # ty: ignore[invalid-return-type]
            builder=self._v4_builder,
            address=address,
            chain_id=chain_id,
            io=io,
            request=request,
        )

    @staticmethod
    def _resolve_deployment(
        *,
        chain_id: ChainId,
        factory: ChecksumAddress,
        default_init_hash: str,
    ) -> tuple[str, str]:
        """Resolve deployer address and pool init-hash from the pool type registry.

        Returns:
            The computed value.

        """
        resolved_deployer: str = factory
        resolved_init_hash = default_init_hash
        deployment = pool_type_registry.get_deployment(chain_id, factory)
        if deployment is not None:
            resolved_deployer = deployment.deployer
            resolved_init_hash = deployment.pool_init_hash or default_init_hash
        return resolved_deployer, resolved_init_hash

    # ------------------------------------------------------------------
    # I/O methods
    # ------------------------------------------------------------------

    async def get_token_balance(
        self,
        token_address: str,
        holder_address: str,
        *,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address.

        Returns:
            The computed integer value.

        """
        token_address = get_checksum_address(token_address)
        token = self.tokens.get(token_address=token_address, chain_id=self._chain_id)
        if token is None:
            token = await self.build_erc20token(token_address)

        io = AsyncPoolIO(self._provider)
        return await self._erc20_builder.get_token_balance(
            token, holder_address, block_identifier=block_identifier, io=io
        )

    async def get_token_approval(
        self,
        token_address: str,
        owner: str,
        spender: str,
        *,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`.

        Returns:
            The computed integer value.

        """
        token_address = get_checksum_address(token_address)
        token = self.tokens.get(token_address=token_address, chain_id=self._chain_id)
        if token is None:
            token = await self.build_erc20token(token_address)

        io = AsyncPoolIO(self._provider)
        return await self._erc20_builder.get_token_approval(
            token, owner, spender, block_identifier=block_identifier, io=io
        )

    async def get_token_total_supply(
        self,
        token_address: str,
        *,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token.

        Returns:
            The computed integer value.

        """
        token_address = get_checksum_address(token_address)
        token = self.tokens.get(token_address=token_address, chain_id=self._chain_id)
        if token is None:
            token = await self.build_erc20token(token_address)

        io = AsyncPoolIO(self._provider)
        return await self._erc20_builder.get_token_total_supply(
            token, block_identifier=block_identifier, io=io
        )

    async def get_ether_balance(
        self,
        address: str,
        *,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address.

        Returns:
            The computed integer value.

        """
        io = AsyncPoolIO(self._provider)
        return await self._erc20_builder.get_ether_balance(
            self._chain_id, address, block_identifier=block_identifier, io=io
        )

    def get_provider(self) -> AsyncProviderAdapter:
        """Return this Bot's single async provider.

        Returns:
            The computed value.

        """
        return self._provider
