"""Bot: central session manager for pool/token construction and registries."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any, Self, cast

from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory
from hexbytes import HexBytes

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.balancer.deployments import BALANCER_V2_VAULT_ADDRESS, BROKEN_BALANCER_V2_POOLS
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.bot import PyBot, PyBotIo
from degenbot.bot_lifecycle import close as _close_handles
from degenbot.bot_lifecycle import (
    release_python_state as _release_python_state,
)
from degenbot.builders.balancer_builder import BalancerBuilder
from degenbot.builders.context import BuilderContext
from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.request import BuildManagedPoolRequest, BuildPoolRequest, BuildRequest
from degenbot.builders.tick_data_fetcher import (
    FetchedTickData,
    TickDataTypes,
    make_tick_data_fetcher,
)
from degenbot.builders.type_resolution import (
    pool_class_for_descriptor,
)
from degenbot.builders.type_resolution import (
    resolve_pool_type as _resolve_pool_type_impl,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.database.operations import get_alembic_config, get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import BrokenPool, TrackerAlreadyInitialized
from degenbot.logging import logger
from degenbot.provider import (
    AlloyProvider,
    AsyncAlloyProvider,
)
from degenbot.provider.factory import get_provider_from_config
from degenbot.provider.subscription import (
    Subscription,  # ruff:ignore[typing-only-first-party-import]
)
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import UniswapV3PoolExternalUpdate
from degenbot.uniswap.v4_liquidity_pool import ProtocolFee, UniswapV4Pool
from degenbot.uniswap.v4_types import UniswapV4PoolExternalUpdate
from degenbot.version import __version__

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from eth_typing import ChecksumAddress

    from degenbot.builders.protocol import PoolBuilder
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.abstract.pool_tracker import AbstractPoolTracker
    from degenbot.types.rpc_types import BlockIdentifier

from degenbot.types.aliases import ChainId  # ruff:ignore[typing-only-first-party-import]


def _update_pool(
    pool: AbstractLiquidityPool,
    *,
    block_number: BlockIdentifier | None,
    io: PyBotIo,
) -> bool:
    """Fetch the current chain state and push an update to a V2/V3/V4 pool.

    T4 / 4GQWZ4 (builder-deletion blocker): the per-family refresh that
    previously lived on the `V2PoolBuilder` / `V3PoolBuilder` /
    `V4PoolBuilder` `update()` methods now lives in this single dispatcher in
    the `Bot` delegating shell — all I/O flows through the `PyBotIo` Rust seam
    (`fetch_v2_reserves` / `fetch_v3_slot0_liquidity` /
    `fetch_v4_slot0_liquidity`), matching the archival behavior. The builders'
    `update()` therefore become orphaned and can be retired with the builders.

    Returns:
        ``True`` if the state changed (an ``external_update`` was applied),
        ``False`` if the on-chain state matches the pool's current state.

    Raises:
        TypeError: If ``pool`` is not a V2/V3/V4 pool (callers dispatch only
            those families here; Aerodrome/Curve/Balancer keep the builder's
            `update()` until SSSXG6).

    """
    if not isinstance(pool, (UniswapV2Pool, UniswapV3Pool, UniswapV4Pool)):
        msg = f"_update_pool cannot update {type(pool).__name__}"
        raise TypeError(msg)
    block_number_ = block_number if block_number is not None else io.get_block_number()
    block_number_ = int(block_number_) if not isinstance(block_number_, int) else block_number_

    if isinstance(pool, UniswapV2Pool):
        reserves0, reserves1 = io.fetch_v2_reserves(pool.address, block=block_number_)
        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False
        pool.external_update(
            UniswapV2PoolExternalUpdate(
                block_number=block_number_,
                reserves_token0=reserves0,
                reserves_token1=reserves1,
            )
        )
        return True

    if isinstance(pool, UniswapV4Pool):
        sqrt_price_x96, tick_raw, _protocol_fee_raw, _lp_fee, liquidity = (
            io.fetch_v4_slot0_liquidity(
                pool._state_view_address,  # ruff:ignore[private-member-access]
                pool.pool_id,
                block=block_number_,
            )
        )
        tick = int(tick_raw)
        if (
            pool.sqrt_price_x96 == int(sqrt_price_x96)
            and pool.liquidity == liquidity
            and pool.tick == tick
        ):
            return False
        pool.external_update(
            UniswapV4PoolExternalUpdate(
                block_number=block_number_,
                sqrt_price_x96=int(sqrt_price_x96),
                tick=tick,
                liquidity=liquidity,
            )
        )
        return True

    # V3 (last family).
    sqrt_price_x96, tick, liquidity = io.fetch_v3_slot0_liquidity(
        pool.address,
        block=block_number_,
    )
    if pool.sqrt_price_x96 == sqrt_price_x96 and pool.liquidity == liquidity and pool.tick == tick:
        return False
    pool.external_update(
        UniswapV3PoolExternalUpdate(
            block_number=block_number_,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            liquidity=liquidity,
        )
    )
    return True


class Bot:
    """Explicit session object that owns the runtime state for a degenbot run.

    Replaces the four module-level singletons (`config`, `db_session`,
    `connection_manager`, `pool_registry`/`token_registry`/`managed_pool_registry`)
    with per-session instances owned by this class.

    Bot is:
    - **Factory** — creates pools/tokens via managers, doing all I/O to fetch data
    - **Registry** — tracks what it's created
    - **I/O boundary** — all RPC calls and database access flow through Bot
    - **Session** — the lifetime scope for the entire run
    """

    def __init__(
        self,
        config: DegenbotConfig,
        *,
        provider: AlloyProvider | None = None,
    ) -> None:
        """Initialize the single-chain Bot session.

        One Bot per chain (ADR-006 D5). The chain identity comes from
        ``config.default_chain_id`` — a ``Bot`` refuses to construct without
        it. Two construction modes:

        - ``provider`` given (injection seam — for fork tests or a caller-built
          Web3/Alloy backend): enforce `provider.chain_id` ==
          ``config.default_chain_id`` (fail-fast), use it directly.
        - ``provider`` omitted: build one from ``config.rpc[default_chain_id]``
          via :func:`get_provider_from_config`, which itself enforces the match.

        Raises:
            DegenbotValueError: If ``config.default_chain_id`` is ``None``, or
                an injected provider's ``chain_id`` mismatches the configured
                chain.

        """
        self.config = config

        if config.default_chain_id is None:
            msg = (
                "Bot requires a default_chain_id in the config. Set "
                "`default_chain_id` in your config file or pass a config with it set."
            )
            raise DegenbotValueError(message=msg)
        self._chain_id: ChainId = config.default_chain_id

        if provider is not None:
            # Explicit injection — enforce chain_id == config.default_chain_id.
            self._enforce_provider_chain(provider, self._chain_id)
            self._provider = provider
        else:
            # Build from config — the factory enforces the chain match itself.
            self._provider = get_provider_from_config(chain_id=self._chain_id, config=config)

        # Polars-inspired three-layer architecture (ADR-005): a ``PyBot``
        # PyO3 wrapper owns the Rust ``Bot`` state behind an ``RwLock``.
        # Multiple Python handles (this session, plus any ``Pool``/``Token``
        # handles it vends) share the same Rust-owned ``Bot`` thread-safely.
        #
        # ADR-006 slice 8b: the facade is single-chain, so the configured
        # ``default_chain_id`` is wired into the Rust ``Bot`` here (D4).
        self._py_bot = PyBot(self._chain_id)

        # JUCFCB (epic P73ER6): eagerly load the V3+V4 DB snapshot into the
        # core ``BotState`` at construction time (Shape 2). This makes the DB
        # a construction-time property of the Bot — correct use is structural:
        # the Bot is born with its snapshot or born cold-start, nothing in
        # between. The core ``Bot::load_snapshot_from_db`` streams V3+V4 into
        # the core ``SnapshotStore`` + records ``S = min(newest_update_block)``.
        # ``None``/cold-start (no pools) is NOT an error. The file/memory
        # snapshot path stays non-DB-only (loaded at ``engine_registry.start``
        # via ``load_*_from_py``).
        if config.database.path is not None:
            db_path = config.database.path
            # The DB file may not exist yet (SQLAlchemy creates it lazily on
            # the first write). A missing file is a cold-start: no snapshot
            # pools to load, `S = None`. The store stays empty; pool
            # registration falls back to sparse. The file will be created by
            # the first write, at which point a `Bot` restart will load it.
            if Path(db_path).exists():
                self._py_bot.load_snapshot_from_db(str(db_path), self._chain_id)
            else:
                logger.debug(
                    "DB file %s does not exist; cold-start (no snapshot loaded).",
                    db_path,
                )

        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=config.database.path),
        )
        # Architecture review 2025-07-18 / candidate 1: attach the core
        # `ConstructionIo` handle to `PyBot` (built from the extracted
        # `AlloyProvider` + an optional held `DegenbotDb`). The 7 generic RPC
        # + 12 DB atomic methods on `PyBotIo` delegate through this; the 27
        # choreography wrappers stay on `PyBotIo` for now (deleted with the
        # builder-choreography port). Alloy-only — a non-alloy provider raises
        # `RuntimeError`.
        self._py_bot.attach_construction_io(
            provider=self._provider,
            database_path=str(config.database.path) if config.database.path else None,
        )
        # The single I/O seam for this Bot (architecture review candidate #2):
        # built once here and reused by every build/update/balance method instead
        # of reconstructing PyBotIo per call. Swapping the I/O seam (fork tests,
        # a different executor) changes one site, not eight.
        self._io = PyBotIo(
            provider=self._provider,
            db=self.db,
            database_path=str(config.database.path),
        )
        # Wire the `ConstructionIo` handle attached above onto `PyBotIo` so its
        # 12 DB + 7 generic RPC methods delegate through the core trait objects.
        self._io.attach_construction_io(self._py_bot)
        self.pools = PoolRegistry(py_bot=self._py_bot)
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._trackers: dict[str, AbstractPoolTracker[Any]] = {}
        # Idempotency flag for close(); mirrored by bot_lifecycle.close.
        self._closed: bool = False

        # Builders own I/O orchestration; Bot hands them its I/O dependencies.
        # Erc20Builder is a leaf — constructed before BuilderContext.
        self._erc20_builder = Erc20Builder(
            default_chain_id=self._chain_id,
            db=self.db,
            tokens=self.tokens,
            py_bot=self._py_bot,
        )
        ctx = BuilderContext(
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
            py_bot=self._py_bot,
            default_chain_id=self._chain_id,
        )
        self._curve_builder = CurvePoolBuilder(ctx)
        self._balancer_builder = BalancerBuilder(ctx)

        # Builder registry: concrete pool type → builder
        # Used by update() for O(1) dict lookup instead of isinstance chain
        self._builders: dict[type, PoolBuilder] = {}

        # Async adapter for subscriptions (single chain; created on demand)
        self._async_adapter: AsyncAlloyProvider | None = None
        self.register_builder(CurveStableswapPool, self._curve_builder)
        self.register_builder(BalancerV2Pool, self._balancer_builder)
        self.register_builder(BalancerV2StablePool, self._balancer_builder)
        # All V2-family DEXes (Uniswap/Sushi/Pancake/Swapbased/Camelot) now
        # register the canonical ``UniswapV2Pool`` for their factories
        # (ADR-005 slice 7 step 4b) — the single V2 builder handles them.

        # Check database migration version
        self._check_database_version()

    @staticmethod
    def _enforce_provider_chain(provider: AlloyProvider, expected: ChainId) -> None:
        """Raise if the provider's chain_id doesn't match ``expected``.

        Fail-fast on a misconfigured endpoint: if the RPC reports a different
        ``eth_chainId`` than the one declared in the config, the Bot cannot
        safely continue (pools/tokens would be built against the wrong chain).

        Raises:
            DegenbotValueError: If the provider's ``chain_id`` != ``expected``.

        """
        actual = provider.chain_id
        if actual != expected:
            msg = (
                f"Provider chain_id ({actual}) does not match the configured "
                f"default_chain_id ({expected}). Refusing to start a Bot "
                f"against the wrong chain."
            )
            raise DegenbotValueError(message=msg)

    @property
    def chain_id(self) -> ChainId:
        """The single chain this Bot targets (ADR-006 D5)."""
        return self._chain_id

    @property
    def provider(self) -> AlloyProvider:
        """The single RPC provider for this Bot's chain."""
        return self._provider

    def _check_database_version(self) -> None:
        """Warn if the database schema is out of date."""
        try:
            with self.db():
                current_version = MigrationContext.configure(
                    connection=self.db.connection(),
                ).get_current_revision()
        except Exception:  # ruff:ignore[blind-except]
            return

        latest_version = ScriptDirectory.from_config(
            config=get_alembic_config(database_path=self.config.database.path),
        ).get_current_head()

        if current_version is not None and current_version != latest_version:
            logger.warning(
                f"The current database revision ({current_version}) does not match the latest "
                f"({latest_version}) for {__package__} version {__version__}!"
                "\n"
                "Database-related features may raise exceptions if you continue. Perform database "
                "migrations with 'degenbot database upgrade'.",
            )

    @classmethod
    def from_config_file(cls) -> Bot:
        """From config file.

        Builds a single-chain Bot from the config's ``default_chain_id``
        (ADR-006 D5). The provider is constructed from ``config.rpc`` and its
        ``eth_chainId`` is enforced to match.

        Returns:
            An instance wrapping the given config_file.

        """
        return cls(config=_init_config())

    def add_tracker[M: AbstractPoolTracker[Any]](
        self,
        manager_cls: type[M],
        *,
        factory_address: str,
        **kwargs: Any,
    ) -> M:
        """Create a pool manager within this bot's session.

        Returns:
            The computed value.

        Raises:
            TrackerAlreadyInitialized: See function documentation.

        """
        factory_address = get_checksum_address(factory_address)

        key = factory_address
        if key in self._trackers:
            raise TrackerAlreadyInitialized(
                message="A manager has already been initialized for this address. "
                "Access it using the bot's manager registry.",
            )

        manager = manager_cls(
            factory_address=factory_address,
            chain_id=self.chain_id,
            bot=self,
            **kwargs,
        )
        self._trackers[key] = manager
        return manager

    def release_python_state(self) -> None:
        """Drop Python-side pool/token/tracker caches once Rust owns canonical state.

        After the Rust engine has taken ownership of all pool state (snapshots
        streamed, pools registered, backfill complete), the Python-side
        tracker caches, snapshots, and pool/token registries are redundant —
        the hot loop only needs the engine and the async web3 handle. This
        drops them so they stop pinning pool objects in memory.

        Idempotent; safe to call once, at the end of the startup handshake
        (after ``build_paths`` completes). Concrete trackers that carry a
        snapshot (e.g. ``UniswapV3StateTB``) have ``unload_snapshot()``
        called to release the snapshot reference.
        """
        _release_python_state(self)

    def close(self) -> None:
        """Release all Python handles owned by this Bot session.

        End-of-life teardown that composes :meth:`release_python_state` and
        adds the connection teardown the Bot was previously missing: the
        provider connection is closed, the scoped DB session is removed,
        and the ``PyBot`` / provider references are dropped. Idempotent —
        safe to call directly and again from a ``with`` block's ``__exit__``.

        The Rust ``PyBot`` is reference-counted; closing this Python wrapper
        only drops *this* Bot's ref. A running engine that took its own ref
        (via ``EngineRegistry(bot=bot)`` → ``ArbitrageEngine(py_bot=...)``)
        is unaffected.

        For the *mid-lifecycle* "drop redundant Python caches while the Bot
        keeps running" handshake, call :meth:`release_python_state` directly;
        ``close()`` is for end-of-life.
        """
        _close_handles(self)

    def __enter__(self) -> Self:
        """Enter the context manager.

        Returns:
            This Bot, so callers can bind it: ``with Bot(...) as bot:``.

        """
        return self

    def __exit__(self, *exc: object) -> None:
        """Exit the context manager; release all Python handles. Never suppresses."""
        self.close()

    def register_builder(
        self,
        pool_class: type[AbstractLiquidityPool],
        builder: PoolBuilder,
    ) -> None:
        """Register a builder for a concrete pool type.

        After registration, ``update()`` will use ``type(pool)`` dict lookup
        instead of isinstance chains to find the right builder.

        Args:
            pool_class: The concrete pool class (e.g. UniswapV2Pool, AerodromeV2Pool).
            builder: The builder instance that handles construction and updates
                for this pool type.

        """
        self._builders[pool_class] = builder

    def build_erc20token(
        self,
        address: str,
        *,
        silent: bool = False,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token.

        Returns:
            The computed value.

        """
        io = self._io
        return self._erc20_builder.build(address, chain_id=self.chain_id, silent=silent, io=io)

    def get_token(self, address: str) -> Erc20Token:
        """Get or create a token. Bot handles DB lookup, RPC calls, and registration.

        Returns:
            The computed value.

        """
        return self.build_erc20token(address)

    def build_pool(
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
        chain_id = self.chain_id
        io = self._io

        request = BuildPoolRequest(
            silent=silent,
            state_block=state_block,
            state_cache_depth=state_cache_depth,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
        )

        # Check pool registry — return existing pool if already built
        existing = self.pools.get(chain_id=chain_id, pool_address=address)
        if existing is not None:
            return existing

        # Resolve the pool type and dispatch to the appropriate builder
        #
        # If type resolution fails (e.g. Curve pools lack a factory() method),
        # fall back to the Curve builder which handles its own discovery.
        try:
            pool_type = _resolve_pool_type_impl(address, chain_id=chain_id, io=io)
        except DegenbotValueError:
            # Fallback: try Curve builder as last resort
            return self._dispatch_build(
                builder=self._curve_builder,
                address=address,
                chain_id=chain_id,
                io=io,
                request=request,
            )

        # Look up the concrete pool class from the registry
        pool_class = pool_class_for_descriptor(pool_type, chain_id=chain_id)

        # V2/V3 families delegate to the Rust `PoolBuilder` (T4 / 4GQWZ4 full
        # delegation): the core builder owns ALL the io choreography (immutables,
        # reserves/state, DEX resolve incl. Camelot, CREATE2 verify, V3
        # tick-map DB-first) and registers directly into `BotState`. The Python
        # side then registers the two Erc20Tokens in the same `Bot` (ADR-006 —
        # `_from_py_pool` resolves them off the handle) and wraps the structural
        # handle with the companion pool class.
        #
        # V3 wires a `tick_data_fetcher` (the legacy web3-sync fetcher) into
        # `build_v3_pool` so swaps can lazily pull neighbouring tick words
        # beyond the single word the Rust builder bootstraps — full parity with
        # the retired builder's attached fetcher.
        #
        # Curve/Aerodrome/Balancer keep their builders (non-goal, retired under
        # SSSXG6) — except Aerodrome V2 and both Balancer families, which
        # delegate through the Rust PoolBuilder (their own PoolEntry families).
        if issubclass(
            pool_class,
            (UniswapV2Pool, UniswapV3Pool, AerodromeV2Pool, BalancerV2Pool, BalancerV2StablePool),
        ):
            return self._build_delegated(pool_class, address, chain_id, request)

        builder = self._builders.get(pool_class)
        if builder is None:
            # Fallback: walk MRO of pool_class looking for a registered builder
            for base in pool_class.__mro__:
                builder = self._builders.get(base)
                if builder is not None:
                    break

        if builder is None:
            raise DegenbotValueError(message=f"No builder for pool class {pool_class.__name__}")

        return self._dispatch_build(
            builder=builder,
            address=address,
            chain_id=chain_id,
            io=io,
            request=request,
        )

    def _make_v3_tick_data_fetcher(
        self,
        pool_address: str,
        chain_id: ChainId,
    ) -> Callable[[int, int], FetchedTickData | None]:
        """Create the V3 tick-data backfill fetcher for a pool address.

        T4 / 4GQWZ4: the legacy web3-sync fetcher factory formerly on the
        retired `V3PoolBuilder`, relocated into this delegating shell. The
        returned fetcher lazily pulls neighbouring tick words during swap
        boundary-crossing (ADR-005 sparse-map parity), attached to the
        Rust-registered pool as a `PyTickWordFetcher` so a Rust `block_on`
        helper would not re-enter the shared runtime during swap simulation.

        Returns:
            A callable pulling ``{tick: (liquidity_gross, liquidity_net,
            block)}`` for an out-of-range bitmap word, or ``None`` when the
            pool/bitmap is unavailable.

        """
        return make_tick_data_fetcher(
            pool_lookup=lambda _block: cast(
                "UniswapV3Pool | None",
                self.pools.get(
                    chain_id=chain_id,
                    pool_address=get_checksum_address(pool_address),
                ),
            ),
            io=self._io,
            types=TickDataTypes(
                bitmap_at_word=BitmapAtWord,
                liquidity_at_tick=LiquidityAtTick,
                tick_struct_types=UniswapV3Pool.TICK_STRUCT_TYPES,
            ),
        )

    def _make_v4_tick_data_fetcher(
        self,
        pool_id: HexBytes,
        pool_manager_address: str,
        state_view_address: str,
        chain_id: ChainId,
    ) -> Callable[[int, int], FetchedTickData | None]:
        """Create the V4 tick-data backfill fetcher for a managed pool.

        T4 / 4GQWZ4: the legacy web3-sync fetcher factory formerly on the
        retired `V4PoolBuilder`, relocated into this delegating shell. The
        returned fetcher lazily pulls neighbouring tick words during swap
        boundary-crossing via the state-view (`getTickBitmap` /
        `getTickLiquidity`), attached to the Rust-registered pool as a
        `PyTickWordFetcher` (ADR-005 sparse-map parity).

        Returns:
            A callable pulling ``{tick: (liquidity_gross, liquidity_net,
            block)}`` for an out-of-range bitmap word, or ``None`` when the
            pool/bitmap is unavailable.

        """
        pool_manager_address_ = get_checksum_address(pool_manager_address)
        return make_tick_data_fetcher(
            pool_lookup=lambda _: cast(
                "UniswapV4Pool | None",
                self.managed_pools.get(
                    chain_id=chain_id,
                    pool_manager_address=pool_manager_address_,
                    pool_id=pool_id,
                ),
            ),
            io=self._io,
            types=TickDataTypes(
                bitmap_at_word=BitmapAtWord,
                liquidity_at_tick=LiquidityAtTick,
                tick_struct_types=("uint128", "int128"),
            ),
            state_view_address=state_view_address,
            pool_id=bytes(pool_id),
        )

    @staticmethod
    def _dispatch_build(
        *,
        builder: PoolBuilder,
        address: ChecksumAddress,
        chain_id: ChainId,
        io: PyBotIo,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Dispatch to the builder with a typed request.

        Returns:
            The computed value.

        """
        return builder.build(address, chain_id=chain_id, io=io, request=request)

    def _build_delegated(
        self,
        pool_class: type[AbstractLiquidityPool],
        address: str,
        chain_id: ChainId,
        request: BuildPoolRequest,
    ) -> AbstractLiquidityPool:
        """Build a V2/V3/Balancer/Aerodrome pool via the Rust PoolBuilder.

        Thin delegating shell over `PyBot.build_v2_pool`/`build_v3_pool`: the
        core builder runs the full io choreography + registers into `BotState`
        and returns the pool id. This shell then registers the pool's two
        Erc20Tokens in the same `Bot` (ADR-006 — the companion's
        ``_from_py_pool`` resolves ``get_token0/get_token1`` off the handle and
        requires them registered) and wraps the structural handle with
        ``pool_class``.

        V3 additionally threads the legacy web3-sync `tick_data_fetcher` into
        `build_v3_pool` (attached as a `PyTickWordFetcher` on the registered
        pool) so swaps can lazily pull neighbouring tick words beyond the Rust
        builder's single-word Sparse bootstrap — full parity with the retired
        builder's attached fetcher.

        Returns:
            A ``pool_class`` companion wrapping the Rust-built pool.

        Raises:
            DegenbotValueError: If the Rust builder registered the pool but
                the resulting handle cannot be recovered.
            BrokenPool: If `pool_class` is a known-broken Balancer pool.

        """
        # Resolve the snapshot block like the legacy builders did: when no
        # explicit `state_block` is given, use the current chain head — the Rust
        # `PoolBuilder` fetches at this block (a `None` would otherwise degrade
        # to `block=0`).
        block: int | None = (
            request.state_block if request.state_block is not None else self._io.get_block_number()
        )
        builder_identity: tuple[int, str, str, str, str] | None = None
        if issubclass(pool_class, UniswapV3Pool):
            # The legacy web3-sync fetcher factory, relocated off the retired
            # V3 builder (4GQWZ4 deletion).
            fetcher = self._make_v3_tick_data_fetcher(address, chain_id)
            b_res = self._py_bot.build_v3_pool(
                address, block=block, db=True, tick_data_fetcher=fetcher
            )
            pool_id = b_res[0]
            builder_identity = b_res
        elif issubclass(pool_class, AerodromeV2Pool):
            # SSSXG6: Aerodrome V2 (shared volatile/stable factory) is a
            # distinct structural family from V2 — the Rust `build_aerodrome_v2`
            # reads `stable()`+`getFee()` and registers into PoolEntry::AerodromeV2.
            pool_id = self._py_bot.build_aerodrome_v2_pool(address, block=block)
        elif issubclass(pool_class, BalancerV2Pool):
            # SSSXG6: Balancer weighted — reads getPoolId + Vault getPoolTokens
            # + getSwapFeePercentage + getNormalizedWeights + bytecode PowVersion
            # + decimals() scaling factors; registers into PoolEntry::BalancerWeighted.
            pool_id = self._py_bot.build_balancer_weighted_pool(
                address, vault=BALANCER_V2_VAULT_ADDRESS, block=block
            )
        elif issubclass(pool_class, BalancerV2StablePool):
            # SSSXG6: Balancer stable — reads getPoolId + Vault getPoolTokens +
            # getSwapFeePercentage + getAmplificationParameter + BPT-detect +
            # rate-provider/rate + scaling factors + invariant_version;
            # registers into PoolEntry::BalancerStable.
            pool_id = self._py_bot.build_balancer_stable_pool(
                address,
                vault=BALANCER_V2_VAULT_ADDRESS,
                block=block,
                invariant_version=request.invariant_version,
            )
        else:
            b_res = self._py_bot.build_v2_pool(address, block=block)
            pool_id = b_res[0]
            builder_identity = b_res
        py_pool = self._py_bot.get_pool(pool_id)
        if py_pool is None:  # pragma: no cover
            msg = f"build_pool: register returned pool_id {pool_id} with no handle"
            raise DegenbotValueError(message=msg)

        # TF7RZB-S1 return-surface parity: the builder returns the Rust core's
        # own token0/token1 identity; it must equal what the registered handle
        # exposes (a divergence is a genuine core/driver seam bug — assert
        # loudly rather than silently re-deriving).
        if builder_identity is not None:
            _b_pid, b_t0, b_t1, _b_addr, _b_fam = builder_identity
            if (
                py_pool.token0_address.casefold() != b_t0.casefold()
                or py_pool.token1_address.casefold() != b_t1.casefold()
            ):
                raise DegenbotValueError(
                    message=(
                        f"build_pool: builder identity diverged from handle for "
                        f"{address}: builder token0={b_t0} token1={b_t1}, "
                        f"handle token0={py_pool.token0_address} "
                        f"token1={py_pool.token1_address}"
                    )
                )

        # V3: split-seed the reorg genesis anchor (mirrors the retired builder's
        # `seed_genesis(state_block)`). Without it the Sparse pool journals
        # nothing, so `discard_states_before_block(update_block + 1)` would not
        # raise — a behavior regression for every consumer that expects the
        # registration seed to be a known state boundary.
        if issubclass(pool_class, UniswapV3Pool) and block is not None and block > 0:
            py_pool.seed_genesis(block_number=block)

        # Register the pool's tokens in the same `Bot` (ADR-006): V2/V3/
        # Aerodrome expose exactly two (`token0_address`/`token1_address`);
        # Balancer exposes N via the family-specific token-address getter.
        if issubclass(pool_class, (BalancerV2Pool, BalancerV2StablePool)):
            # SSSXG6: preserve the broken-pool guard from the retired
            # `BalancerBuilder.build` (BrokenPool) so known-bad pools fail fast.
            if address in BROKEN_BALANCER_V2_POOLS:
                raise BrokenPool
            token_addresses = (
                py_pool.balancer_token_addresses
                if issubclass(pool_class, BalancerV2Pool)
                else py_pool.balancer_stable_token_addresses
            )
            for token_address in token_addresses:
                self._erc20_builder.build(
                    token_address,
                    chain_id=chain_id,
                    silent=request.silent,
                    io=self._io,
                )
        else:
            self._erc20_builder.build(
                py_pool.token0_address,
                chain_id=chain_id,
                silent=request.silent,
                io=self._io,
            )
            self._erc20_builder.build(
                py_pool.token1_address,
                chain_id=chain_id,
                silent=request.silent,
                io=self._io,
            )

        # `_from_py_pool` is a concrete-class classmethod (not on the base); cast
        # to `type[Any]` so the call is type-checkable + the union of the five
        # delegated families stays branch-free here.
        pool = cast("type[Any]", pool_class)._from_py_pool(py_pool)  # ruff:ignore[private-member-access]
        # Idempotent register (35NMBX Guard 1): a concurrent registration worker
        # may have built this same shared pool first; use the canonical instance
        # so THIS path still registers instead of being lossily skipped. (pool_id
        # is None on this delegated path, so get_or_add returns an
        # AbstractLiquidityPool, not a managed V4 pool.)
        return cast(
            "AbstractLiquidityPool",
            self.pools.get_or_add(pool_address=pool.address, chain_id=chain_id, pool=pool),
        )

    def build_managed_pool(
        self,
        address: str,
        pool_id: str | bytes,
        *,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
        # V4 immutable data — required if not in DB
        state_view_address: str | None = None,
        tokens: Sequence[str] | None = None,
        fee: int | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
        # Pre-fetched tick data
        tick_bitmap: dict[int, Any] | None = None,
        tick_data: dict[int, Any] | None = None,
    ) -> UniswapV4Pool:
        """Build a V4 managed pool from a PoolManager address and pool ID.

        ``address`` is the PoolManager contract. ``pool_id`` identifies the
        pool within the manager.

        When the pool is not in the database, ``state_view_address``,
        ``tokens``, ``fee``, ``tick_spacing`` must all be provided.

        Returns:
            The computed value.

        """
        address = get_checksum_address(address)
        chain_id = self.chain_id

        # Check managed pool registry — return existing pool if already built
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

        io = self._io

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

        return self._build_v4_managed(
            address,
            chain_id=chain_id,
            io=io,
            request=request,
        )

    def _build_v4_managed(
        self,
        address: str,
        *,
        chain_id: ChainId,
        io: PyBotIo,
        request: BuildManagedPoolRequest,
    ) -> UniswapV4Pool:
        """Build a V4 managed pool via the Rust `PoolBuilder` (T4 / 4GQWZ4).

        The thin delegating shell formerly `V4PoolBuilder.build()`: resolves
        the caller-supplied V4 identity (DB two-step, else caller kwargs),
        fetches the slot0 scalars for the Python-side companion overrides
        (`protocol_fee`/`lp_fee`/`state_view`/`_sparse_liquidity_map`, which
        the Rust handle does not expose), then delegates the actual build
        (live scalars + Db→Chain tick assembly + admission + registration)
        to `PyBot.build_v4_pool`.

        Returns:
            A `UniswapV4Pool` companion wrapping the Rust-registered pool.

        Raises:
            DegenbotValueError: If identity fields are missing for a pool not
                in the database.

        """
        pool_id_bytes = HexBytes(request.pool_id)
        pool_manager_address = get_checksum_address(address)

        state_block = (
            request.state_block if request.state_block is not None else io.get_block_number()
        )
        # The FRESH PRICE read block (two-stamp OB7UNY): the cheap slot0/price
        # read stamps `update_block` at the live head, while the liquidity
        # clock + assembled tick map anchor at `state_block`. When a caller
        # pins `request.state_block`, price = that same block (no split).
        head_block = state_block

        # TF7RZB-S3: V4 identity resolution moves CORE-side. The Rust
        # `resolve_v4_identity` performs the DB two-step (manager → v4 row →
        # per-FK tokens) first, else the caller-supplied overrides, and returns
        # the resolved identity (currency0/1, fee, tick_spacing, hook_flags,
        # state_view). The driver no longer reads the DB nor assembles the
        # kwargs identity itself.
        over_tokens = request.tokens
        over_currency0 = over_tokens[0] if over_tokens else None
        over_currency1 = over_tokens[1] if over_tokens else None
        try:
            (
                currency0_address,
                currency1_address,
                fee_for_pool,
                tick_spacing_for_pool,
                hook_flags,
                state_view_hex,
            ) = self._py_bot.resolve_v4_identity(
                chain_id=int(chain_id),
                pool_manager=pool_manager_address,
                pool_id_hex=pool_id_bytes.to_0x_hex(),
                currency0=over_currency0,
                currency1=over_currency1,
                fee=int(request.fee) if request.fee is not None else None,
                tick_spacing=(
                    int(request.tick_spacing) if request.tick_spacing is not None else None
                ),
                hook_address=request.hook_address,
                state_view_address=request.state_view_address,
            )
        except ValueError as exc:
            # The core raises a typed `MissingIdentity` (mapped to PyValueError)
            # when neither the DB two-step nor the overrides are complete.
            raise DegenbotValueError(
                message=exc.args[0] if exc.args else "V4 identity resolution failed"
            ) from exc
        state_view_address = get_checksum_address(state_view_hex)

        # Build both tokens — from the CORE-resolved currency addresses — in
        # ONE batched metadata read (CDJEPJ-2): build_many collapses the two
        # per-token fetch_erc20_metadata round-trips into a single Multicall3
        # aggregate3 eth_call for the network-missing metadata.
        token0, token1 = self._erc20_builder.build_many(
            [currency0_address, currency1_address],
            chain_id=chain_id,
            silent=request.silent,
            io=io,
        )

        # Delegate the build to the Rust PoolBuilder: core `build_v4` fetches
        # slot0/liquidity FRESH + assembles the tick map (Db → Chain
        # precedence) + registers into BotState atomically, using the
        # CORE-resolved identity above. Returns `(pool_id, coverage,
        # identity..., protocol_fee, lp_fee)` — coverage drives the companion's
        # `_sparse_liquidity_map`, and the fee overrides (protocol_fee / lp_fee)
        # ride back from the SAME head-stamped slot0 read, so the companion no
        # longer issues a second fetch_v4_slot0_liquidity per pool (CDJEPJ-1).
        (
            pool_handle_pool_id,
            coverage,
            b_cur0,
            b_cur1,
            b_pm,
            b_fee,
            b_ts,
            b_hf,
            b_pool_id_hex,
            b_protocol_fee,
            b_lp_fee,
        ) = self._py_bot.build_v4_pool(
            pool_manager=pool_manager_address,
            pool_id_hex=pool_id_bytes.to_0x_hex(),
            currency0=currency0_address,
            currency1=currency1_address,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            hook_flags=hook_flags,
            state_view_address=state_view_address,
            block=int(head_block) if head_block is not None else None,
            db=True,
            tick_data_fetcher=self._make_v4_tick_data_fetcher(
                pool_id_bytes,
                pool_manager_address,
                state_view_address,
                chain_id,
            ),
        )
        # TF7RZB-S2/S3 return-surface parity: the identity the builder echoes
        # back must match what the core resolver produced (a divergence is a
        # real seam bug), and the pool_id must round-trip the requested hash.
        b_identity_ok = all([
            b_cur0.lower() == currency0_address.lower(),
            b_cur1.lower() == currency1_address.lower(),
            b_pm.lower() == pool_manager_address.lower(),
            int(b_fee) == int(fee_for_pool),
            int(b_ts) == int(tick_spacing_for_pool),
            int(b_hf) == int(hook_flags),
            b_pool_id_hex.lower() == pool_id_bytes.to_0x_hex().lower(),
        ])
        if not b_identity_ok:
            raise DegenbotValueError(
                message=(
                    "V4 builder identity `build_v4_pool` diverged from the "
                    "resolved identity (currency0/1, pool_manager, fee, "
                    "tick_spacing, hook_flags, pool_id)."
                )
            )
        tick_map_is_tracked = coverage == "tracked"
        py_pool_handle = self._py_bot.get_pool(pool_handle_pool_id)
        assert py_pool_handle is not None, "build_v4_pool returned a pool_id with no handle"
        pool = UniswapV4Pool._from_py_pool(py_pool_handle)  # ruff:ignore[private-member-access]
        # Builder-supplied values the seam defaults; override from RPC.
        pool._state_view_address = (  # ruff:ignore[private-member-access]
            get_checksum_address(state_view_address)
        )
        pool.protocol_fee = ProtocolFee(
            zero_for_one=b_protocol_fee & 0xFFF,
            one_for_zero=b_protocol_fee >> 12,
        )
        pool.lp_fee = int(b_lp_fee)
        pool._sparse_liquidity_map = not tick_map_is_tracked  # ruff:ignore[private-member-access]

        # Register pool in managed pool registry
        pool = cast(
            "UniswapV4Pool",
            self.managed_pools.get_or_add(
                pool=pool,
                chain_id=chain_id,
                pool_manager_address=pool.address,
                pool_id=pool.pool_id,
            ),
        )

        if not request.silent:
            logger.info(pool.name)
            logger.info(f"• ID: {pool.pool_id.to_0x_hex()}")
            logger.info(f"• Token 0: {token0}")
            logger.info(f"• Token 1: {token1}")
            logger.info(f"• Liquidity: {pool.liquidity}")
            logger.info(f"• SqrtPrice: {pool.sqrt_price_x96}")
            logger.info(f"• Tick: {pool.tick}")

        return pool

    def get_token_balance(
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address.

        Returns:
            The computed integer value.

        """
        io = self._io
        return self._erc20_builder.get_token_balance(
            token,
            address,
            block_identifier=block_identifier,
            io=io,
        )

    def get_token_approval(
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`.

        Returns:
            The computed integer value.

        """
        io = self._io
        return self._erc20_builder.get_token_approval(
            token,
            owner,
            spender,
            block_identifier=block_identifier,
            io=io,
        )

    def get_token_total_supply(
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token.

        Returns:
            The computed integer value.

        """
        io = self._io
        return self._erc20_builder.get_token_total_supply(
            token,
            block_identifier=block_identifier,
            io=io,
        )

    def get_ether_balance(
        self,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address.

        Returns:
            The computed integer value.

        """
        io = self._io
        return self._erc20_builder.get_ether_balance(
            self.chain_id,
            address,
            block_identifier=block_identifier,
            io=io,
        )

    def get_provider(self) -> AlloyProvider:
        """Return this Bot's single provider.

        Returns:
            The computed value.

        """
        return self.provider

    async def start_listening(self) -> tuple[Subscription, Subscription]:
        """Start WS subscriptions for newHeads and unfiltered logs on this Bot's chain.

        Single chain (ADR-006 D5): no ``chain_id`` argument. Creates an
        AsyncAlloyProvider from the configured WS URI for this Bot's chain,
        subscribes to new block headers and unfiltered log events
        (``eth_subscribe("logs", {})``), and returns both subscriptions as a
        tuple ``(heads_sub, logs_sub)``. The adapter is cached on the Bot.

        Returns:
            Tuple of ``(heads_subscription, logs_subscription)``.

        Raises:
            DegenbotValueError: If no WS URI is configured for this chain.

        """
        # Reuse existing adapter if already created
        if self._async_adapter is not None:
            heads = await self._async_adapter.subscribe_blocks()
            logs = await self._async_adapter.subscribe_logs()
            return heads, logs

        # Look up WS URI from config
        ws_uri = self.config.ws.get(self.chain_id)
        if ws_uri is None:
            msg = (
                f"No WS URI configured for chain {self.chain_id}. "
                f"Add [ws.{self.chain_id}] to config."
            )
            raise DegenbotValueError(message=msg)

        # Create async alloy provider from WS URI
        adapter = await AsyncAlloyProvider.create(str(ws_uri))

        self._async_adapter = adapter

        heads = await adapter.subscribe_blocks()
        logs = await adapter.subscribe_logs()
        return heads, logs

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch the current state of a pool from the chain and apply it via.

        ``pool.external_update()``.

        Returns True if the state changed, False if unchanged.

        Returns:
            The computed boolean value.

        """
        resolved_block_number = (
            int(block_number)
            if block_number is not None and not isinstance(block_number, int)
            else block_number
        )
        io = self._io
        # T4 / 4GQWZ4: V2/V3/V4 refresh lives in the delegating shell
        # (`_update_pool`), no longer on the retired builders; Aerodrome /
        # Curve / Balancer keep their builders' `update()` until SSSXG6.
        if isinstance(pool, (UniswapV2Pool, UniswapV3Pool, UniswapV4Pool)):
            return _update_pool(
                pool,
                block_number=resolved_block_number,
                io=io,
            )
        builder = self._builder_for_pool(pool)
        return builder.update(pool, block_number=resolved_block_number, io=io)

    def _builder_for_pool(
        self,
        pool: AbstractLiquidityPool,
    ) -> PoolBuilder:
        """Select the appropriate builder for the pool type.

        Uses the builder registry (dict lookup on type(pool)) first, then
        falls back to isinstance checks for subclasses not explicitly
        registered.

        Returns:
            The computed value.

        Raises:
            TypeError: See function documentation.

        """
        # Fast path: exact type match in the registry
        builder = self._builders.get(type(pool))
        if builder is not None:
            return builder

        # Slow path: subclass match (e.g. AerodromeV2Pool subclasses UniswapV2Pool).
        # Walk the MRO looking for a registered builder.
        for base in type(pool).__mro__:
            builder = self._builders.get(base)
            if builder is not None:
                return builder

        msg = f"update() not implemented for pool type {type(pool).__name__}"
        raise TypeError(msg)
