"""Bot: central session manager for pool/token construction and registries."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any, Self, cast

from degenbot._ffi import Bot as _Engine
from degenbot._ffi import BotIo

# Import-order anchor: the aerodrome package must initialize before the
# builders below pull in `degenbot.uniswap`; the facade does not name the
# class directly.
from degenbot.aerodrome.pools import AerodromeV2Pool  # ruff:ignore[unused-import]
from degenbot.balancer.deployments import BALANCER_V2_VAULT_ADDRESS
from degenbot.bot import driver_boot as _driver_boot
from degenbot.bot_lifecycle import close as _close_handles
from degenbot.bot_lifecycle import (
    release_python_state as _release_python_state,
)
from degenbot.builders.context import BuilderContext
from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.request import (
    BuildManagedPoolRequest,
    BuildPoolRequest,
    BuildRequest,
    ConstructionRoute,
)
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
from degenbot.config import resolve_chain_id, resolve_database_path
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.deployments import CURVE_V1_FACTORY_ADDRESS, CURVE_V1_REGISTRY_ADDRESS
from degenbot.discovery import PoolRef
from degenbot.discovery import pools_for_tokens as _pools_for_tokens
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger
from degenbot.provider import (
    AlloyProvider,  # ruff:ignore[typing-only-first-party-import]
    AsyncAlloyProvider,  # ruff:ignore[typing-only-first-party-import]
)
from degenbot.provider.factory import get_provider_from_config
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.types.pool_type import (
    PoolFamily,
    PoolTypeDescriptor,
    PoolVariant,
    classify_pool_variant,
    derive_kind,
)
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import ProtocolFee, UniswapV4Pool
from degenbot.utils.bytes import to_0x_hex, to_bytes

from ._account_queries import AccountQueryMixin

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from degenbot._ffi import BlockStream, IntakeReceipt
    from degenbot.builders.protocol import PoolBuilder
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.pathfinding import PoolKind
    from degenbot.types import Pool
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.types.rpc_types import BlockIdentifier

from degenbot.types.aliases import ChainId  # ruff:ignore[typing-only-first-party-import]

# The Curve registry/factory addresses the Curve construction arm's LP-token +
# metapool lookups consult — the driver-supplied deployment list (like the V3
# route's factory rungs), never a core-side constant.
_CURVE_REGISTRIES = (CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS)


class Bot(AccountQueryMixin):
    """Explicit session object that owns the runtime state for a degenbot run.

    Owns the per-session configuration, Rust database path, provider,
    registries, and engine handles instead of exposing module-level
    singletons.

    Bot is:
    - **Factory** — creates pools/tokens via managers, doing all I/O to fetch data
    - **Registry** — tracks what it's created
    - **I/O boundary** — all RPC calls and database access flow through Bot
    - **Session** — the lifetime scope for the entire run
    - **Account queries** — ERC-20/native balance, approval, and supply reads,
      inherited from `AccountQueryMixin` so this class body stays under the
      public-method complexity bar; the facade surface is unchanged
    """

    def __init__(  # ruff:ignore[too-many-arguments]
        self,
        *,
        chain_id: int | None = None,
        node: str | None = None,
        database: str | None = None,
        provider: AlloyProvider | None = None,
        py_bot: _Engine | None = None,
        io: BotIo | None = None,
        erc20_builder: Erc20Builder | None = None,
        provider_factory: Callable[..., Any] | None = None,
    ) -> None:
        """Initialize the single-chain Bot session.

        One Bot per chain (ADR-006 D5). The three keyword overrides are the
        explicit layer of the installed typed config's cascade; with none
        supplied, the chain id, the endpoint, and the database path all resolve
        through ``degenbot-config``'s four layers and report the layer that won.

        Two construction modes:

        - ``provider`` given (injection seam — for fork tests or a caller-built
          Web3/Alloy backend): enforce `provider.chain_id` == the resolved chain
          (fail-fast), use it directly.
        - ``provider`` omitted: build one through :func:`get_provider_from_config`
          with the resolved chain and the ``node`` override; the factory hands
          the chain to the core, which enforces the match itself.

        ``py_bot``/``io``/``erc20_builder`` are DI constructor seams:
        tests inject doubles at construction instead of patching module
        privates or assigning ``bot._*`` attributes; omitted kwargs keep
        the production bindings. An injected object skips the
        construction-time wiring that belongs to the default instance
        (engine DB snapshot load + ``ConstructionIo`` attach; ``BotIo``
        ``ConstructionIo`` attach) — the injector owns that wiring.

        Args:
            chain_id: The explicit session chain override.
            node: The explicit endpoint override, classified by its own value.
            database: The explicit database-path override.
            provider: An already-built provider to bind instead of resolving one.
            py_bot: An injected Rust engine handle.
            io: An injected ``BotIo`` handle.
            erc20_builder: An injected ERC-20 builder.
            provider_factory: The factory called as
                ``provider_factory(chain_id=..., node=...)`` when ``provider``
                is omitted (tests inject a recording factory to observe the
                override keywords); the default is
                :func:`~degenbot.provider.factory.get_provider_from_config`.

        The chain and database refusals (no layer names one) come from the
        cascade's :func:`resolve_chain_id` / :func:`resolve_database_path` as a
        :class:`ValueError`, each naming what to declare, export, or pass.

        Raises:
            DegenbotValueError: If an injected provider serves another chain
                than the resolved one, or provider boot fails — typed on the
                build-refusal taxonomy and immediate (a host that does not
                resolve is not retried).

        """
        # A driver session is constructing its engine: bind the shared
        # runtime + telemetry stack now (idempotent), not at import time.
        _driver_boot()

        self._chain_id: ChainId = resolve_chain_id(chain_id)
        self.database_path = Path(resolve_database_path(database))

        if provider is not None:
            # Explicit injection — enforce chain_id == the resolved session chain.
            self._enforce_provider_chain(provider, self._chain_id)
            self._provider = provider
        else:
            # Resolved from the cascade — the factory hands the chain to the core,
            # which enforces the match itself.
            try:
                self._provider = (provider_factory or get_provider_from_config)(
                    chain_id=self._chain_id, node=node
                )
            except DegenbotValueError:
                raise
            except Exception as exc:
                msg = (
                    f"Bot: cannot build a provider for chain {self._chain_id} "
                    f"(node={node!r}): {exc}"
                )
                raise DegenbotValueError(message=msg) from exc

        # Polars-inspired three-layer architecture (ADR-005): a the Rust ``Bot`` engine
        # PyO3 wrapper owns the Rust ``Bot`` state behind an ``RwLock``.
        # Multiple Python handles (this session, plus any ``Pool``/``Token``
        # handles it vends) share the same Rust-owned ``Bot`` thread-safely.
        #
        # ADR-006 slice 8b: the facade is single-chain, so the configured
        # ``default_chain_id`` is wired into the Rust ``Bot`` here (D4).
        if py_bot is None:
            self._py_bot = _Engine(self._chain_id)

            # Eagerly load the V3+V4 DB snapshot into the
            # core ``BotState`` at construction time (Shape 2). This makes the DB
            # a construction-time property of the Bot — correct use is structural:
            # the Bot is born with its snapshot or born cold-start, nothing in
            # between. The core ``Bot::load_snapshot_from_db`` streams V3+V4 into
            # the core ``SnapshotStore`` + records ``S = min(newest_update_block)``.
            # ``None``/cold-start (no pools) is NOT an error. The DB snapshot is
            # the sole S source on both real paths: the injected
            # ``load_*_from_py`` seam is retired (P2b), so no separate
            # file/memory snapshot path remains to load.
            # The DB file may not exist yet (the Rust database owner creates it on first
            # write). A missing file is a cold-start: no snapshot pools to load,
            # `S = None`. The store stays empty; pool registration falls back to
            # sparse. The file will be created by the first write, at which point a
            # `Bot` restart will load it.
            if Path(self.database_path).exists():
                self._py_bot.load_snapshot_from_db(str(self.database_path), self._chain_id)
            else:
                logger.debug(
                    "DB file %s does not exist; cold-start (no snapshot loaded).",
                    self.database_path,
                )
        else:
            # Injection seam (see __init__ docstring): the injector owns
            # this engine's construction-time wiring (DB snapshot load +
            # ``ConstructionIo`` attach), which runs for the default
            # engine only.
            self._py_bot = py_bot

        if py_bot is None:
            # Architecture review 2025-07-18 / candidate 1: attach the core
            # `ConstructionIo` handle to the `Bot` engine (built from the extracted
            # `AlloyProvider` + an optional held `DegenbotDb`). The 7 generic RPC
            # + 12 DB atomic methods on `BotIo` delegate through this; the 27
            # choreography wrappers stay on `BotIo` for now (deleted with the
            # builder-choreography port). Alloy-only — a non-alloy provider raises
            # `RuntimeError`.
            self._py_bot.attach_construction_io(
                provider=self._provider,
                database_path=str(self.database_path),
            )
        if io is not None:
            # Injection seam (see __init__ docstring): the injector owns
            # this I/O seam's ``ConstructionIo`` attach, which runs for
            # the default-constructed BotIo only.
            self._io = io
        else:
            # The single I/O seam for this Bot (architecture review candidate #2):
            # built once here and reused by every build/update/balance method instead
            # of reconstructing BotIo per call. Swapping the I/O seam (fork tests,
            # a different executor) changes one site, not eight.
            self._io = BotIo(
                provider=self._provider,
                database_path=str(self.database_path),
            )
            # Wire the `ConstructionIo` handle attached above onto `BotIo` so its
            # 12 DB + 7 generic RPC methods delegate through the core trait objects.
            self._io.attach_construction_io(self._py_bot)
        # Identity for the session's pools and tokens is the Rust session
        # registry's; these three are the Python companion side of it. The V4
        # companion store is constructed once and handed to `PoolRegistry` so
        # `bot.managed_pools` and `bot.pools`'s V4 branch are one store — a V4
        # pool registered through either is the same companion.
        self.managed_pools = ManagedPoolRegistry(py_bot=self._py_bot)
        self.pools = PoolRegistry(py_bot=self._py_bot, managed_pool_registry=self.managed_pools)
        self.tokens = TokenRegistry(py_bot=self._py_bot)
        # Idempotency flag for close(); mirrored by bot_lifecycle.close.
        self._closed: bool = False

        # Builders own I/O orchestration; Bot hands them its I/O dependencies.
        # Erc20Builder is a leaf — constructed before BuilderContext.
        self._erc20_builder = (
            erc20_builder
            if erc20_builder is not None
            else Erc20Builder(
                default_chain_id=self._chain_id,
                tokens=self.tokens,
                py_bot=self._py_bot,
            )
        )
        ctx = BuilderContext(
            database_path=self.database_path,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
            py_bot=self._py_bot,
            default_chain_id=self._chain_id,
        )
        self._curve_builder = CurvePoolBuilder(ctx)

        # Async adapter for subscriptions (single chain; created on demand)
        self._async_adapter: AsyncAlloyProvider | None = None

    def registration_fleet_hosted(self) -> bool:
        """Return the construction-time registration-intake stance (PRG-3/5).

        Thin pass-through to the core: True once the engine has installed the
        fleet intake boot (the module-init stance holder is read at FFI init,
        the boot descriptor installs at engine construction). The crawl
        driver requires this hard-True from pipeline construction on (PRG-5:
        the legacy crawl shell is retired — no getattr-default fallback).

        Returns:
            True once the fleet intake boot is installed (stance = fleet).

        """
        return self._py_bot.registration_fleet_hosted()

    def submit_registration_unit(self, fn: object) -> IntakeReceipt:
        """Submit ONE registration-intake unit for fleet-seat execution.

        The receipt exposes ``done()`` / ``result()`` / ``wait()`` /
        ``wait_async()``. The crawl's per-path units (build + verify
        lifecycles + path registration) ride the fleet's duty-counted
        ``PoolStateUpdater`` seats through this seam (PRG-5).

        On a host whose fleet boot was refused (detected
        CPU budget below the pinned-role floor, or a boot invariant), this
        submit — and every submit after it — raises the typed, sticky
        ``BootRefused`` from the FFI seam: the library never aborts the host
        process on the boot-refusal arm, and nothing is enqueued into a
        pipe that will not be drained.

        Returns:
            The unit's receipt (``IntakeReceipt``).

        """
        return self._py_bot.submit_registration_unit(fn)

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
        return cast("AlloyProvider", self._provider)

    def block_stream(self) -> BlockStream:
        """Fresh async iterator over ``newHeads`` block notifications.

        The settlement bot's authoritative block clock: ticked
        once per accepted header by the pump — NOT derived from
        ``ResultBatch.solve_block``, which lags by the send debounce.

        ADR-027: the block-clock pipe is
        coordinator-owned, so this surfaces on the ``Bot`` (the
        pump-lifecycle handle) rather than on the arbitrage engine, which is
        out of the block path entirely. Once-only: a second call raises
        ``RuntimeError``.

        Returns:
            BlockStream: Async iterator yielding one dict per accepted block
            header (``number``, ``timestamp``, ``base_fee_per_gas``,
            ``gas_used``, ``gas_limit``); ends when the pump stops.

        """
        return self._py_bot.block_stream()

    def release_python_state(self) -> None:
        """Drop Python-side pool/token caches once Rust owns canonical state.

        After the Rust engine has taken ownership of all pool state (snapshots
        streamed, pools registered, backfill complete), the Python-side
        pool/token registries are redundant — the hot loop only needs the
        engine and the async Alloy provider handle. This drops them so they
        stop pinning pool objects in memory.

        Idempotent; safe to call once, at the end of the startup handshake
        (after ``build_paths`` completes).
        """
        _release_python_state(self)

    def close(self) -> None:
        """Release all Python handles owned by this Bot session.

        End-of-life teardown that composes :meth:`release_python_state` and
        closes the provider connection and drops the Rust ``Bot`` engine /
        provider references. Idempotent —
        safe to call directly and again from a ``with`` block's ``__exit__``.

        The Rust ``Bot`` engine is reference-counted; closing this Python wrapper
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

    def resolve_pool_type(self, address: str) -> PoolTypeDescriptor:
        """Resolve a pool's type without building it.

        The same resolution ``build_pool()`` performs (DB kind, then the
        factory's registry entry, then on-chain probing): ask "what would be
        built?" before spending a build, and compare the preview against the
        identity a built pool binds.

        Returns:
            The resolved pool type descriptor.

        Raises:
            DegenbotValueError: If the pool's type cannot be determined.

        """
        try:
            return _resolve_pool_type_impl(
                get_checksum_address(address),
                chain_id=self.chain_id,
                io=self._io,
            )
        except DegenbotValueError:
            raise
        except Exception as exc:
            msg = f"resolve_pool_type: cannot determine the type of {address}: {exc}"
            raise DegenbotValueError(message=msg) from exc

    def build_pool(  # ruff:ignore[too-many-locals]
        self,
        address: str,
        *,
        state_block: int | None = None,
        silent: bool = False,
        construction_route: ConstructionRoute | None = None,
    ) -> AbstractLiquidityPool:
        """Build a pool from an address, automatically resolving its type.

        V4 managed pools should use ``build_managed_pool()`` instead.

        ``construction_route`` is the resolved construction-route policy the
        core route entry walks for the V3 arm (factory rungs + the generic
        builder); ``None`` = the generic-only route. The route is a driver
        VALUE — the core owns the walk and classifies every failure on the
        build-refusal taxonomy (an unsupported family raises the typed
        ``UnsupportedPoolFamilyError`` — the loud-abort rule).

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
            # state_cache_depth stays a request field; this facade entry point
            # carries only the knobs in-tree callers actually set.
            construction_route=construction_route,
        )

        # Check pool registry — return existing pool if already built
        existing = self.pools.get(chain_id=chain_id, pool_address=address)
        if existing is not None:
            return existing

        # Resolve the pool type; every family then builds through the ONE core
        # construction entry below. A discovery-only pool (Curve exposes no
        # factory(), so the resolver cannot classify it) resolves to the
        # generic Curve descriptor and takes the same ONE entry.
        try:
            pool_type = _resolve_pool_type_impl(address, chain_id=chain_id, io=io)
        except DegenbotValueError:
            # Discovery-only identity: Curve pools expose no `factory()`, so the
            # type resolver cannot classify them. Route through the ONE core
            # construction entry's generic Curve arm (never a second path).
            pool_type = None

        # The `(family, variant)` identity is what the core dispatches on. A
        # discovery-only pool (the resolver raised — Curve exposes no
        # `factory()`) is the generic Curve descriptor, routed through the ONE
        # core construction entry below like every other family.
        if pool_type is not None:
            family = pool_type.family
            variant = pool_type.variant
        else:
            family = PoolFamily.STABLESWAP
            variant = None

        is_curve = family is PoolFamily.STABLESWAP and variant != "balancer_stable"

        # Resolve the snapshot block: when no explicit `state_block` is given,
        # use the current chain head — the core fetches at this block (a `None`
        # would otherwise degrade to `block=0`).
        # Resolved to a concrete `int` at this seam: the `else` arm is the
        # live chain head, so `block` is never `None` in practice — the type is
        # narrowed here so it can be handed to the `int`-typed sinks
        # (`construct_pool`, `CurvePoolBuilder.finalize`).
        head = self._io.get_block_number()
        block: int = request.state_block if request.state_block is not None else head

        # DB-arm eligibility: the DB liquidity map is a CURRENT-state cache (no
        # per-block history), so it can only seed a HEAD construction. A pinned
        # `state_block` behind the current head is a HISTORICAL construction —
        # the head-stamped rows would fail the core's chain-truth intake gate
        # (any position minted or burned since the pinned block), and before
        # that gate existed they silently produced a mixed-state pool (head
        # tick map under historical scalars). `db=False` forces the core's
        # Chain-arm Sparse path, which assembles the tick map from the chain
        # at the pinned block.
        db_seed = block >= head

        # The construction route: the cockpit's resolved policy value — the core
        # route entry walks it (route order + get-or-register + build +
        # register) and classifies every failure on the taxonomy.
        route = request.construction_route or ConstructionRoute()

        # ONE core entry (ADR-059 D6: dispatch at the tier boundary). The
        # `(family, variant)` identity is DATA from the resolver, never a
        # subclass test. The core owns the per-family I/O choreography
        # (immutables, reserves/state, DEX resolve incl. Camelot, CREATE2
        # verify, the V3 DB-first tick map), registers into `BotState`, and
        # echoes the construction identity. A new family is a core arm plus a
        # species-manifest entry — never a new branch here. The V3 fork storage
        # layout is resolved core-side from the deployment table.
        is_concentrated_liquidity = family is PoolFamily.CONCENTRATED_LIQUIDITY
        pool_id, b_t0, b_t1, _b_addr, b_var = self._py_bot.construct_pool(
            address,
            family=family.value,
            variant=variant,
            block=block,
            db=db_seed,
            tick_data_fetcher=(
                self._make_v3_tick_data_fetcher(address, chain_id)
                if is_concentrated_liquidity
                else None
            ),
            factories=list(route.factories) or None,
            registries=list(_CURVE_REGISTRIES) if is_curve else None,
            vault=BALANCER_V2_VAULT_ADDRESS,
            invariant_version=request.invariant_version,
        )

        py_pool = self._py_bot.get_pool(pool_id)
        if py_pool is None:  # pragma: no cover
            msg = f"build_pool: register returned pool_id {pool_id} with no handle"
            raise DegenbotValueError(message=msg)

        # Return-surface parity: the core entry echoes its own token0/token1
        # identity, which must equal what the registered handle exposes (a
        # divergence is a genuine core/driver seam bug — assert loudly rather
        # than silently re-deriving). The multi-token families echo no token
        # pair, so the guard applies only where a pair is echoed.
        if (
            b_t0
            and b_t1
            and (
                py_pool.token0_address.casefold() != b_t0.casefold()
                or py_pool.token1_address.casefold() != b_t1.casefold()
            )
        ):
            raise DegenbotValueError(
                message=(
                    f"build_pool: core identity diverged from handle for "
                    f"{address}: core token0={b_t0} token1={b_t1}, "
                    f"handle token0={py_pool.token0_address} "
                    f"token1={py_pool.token1_address}"
                )
            )

        # V3: split-seed the reorg genesis anchor. Without it the Sparse pool
        # journals nothing, so a reorg restore targeting the registration seed
        # finds no prior state — a behavior regression for every consumer that
        # expects the registration seed to be a known state boundary.
        if is_concentrated_liquidity and block is not None and block > 0:
            py_pool.seed_genesis(block_number=block)

        if is_curve:
            # The Curve companions (metapool base-pool recursion + coin /
            # underlying / LP ERC20 builds) — the post-construction choreography
            # the retired Curve builder drove. Construction itself ran through
            # the ONE core entry above.
            self._curve_builder.finalize(
                py_pool,
                chain_id=chain_id,
                state_block=block,
                silent=request.silent,
                io=io,
            )
        else:
            self._register_delegated_tokens(
                py_pool,
                variant=variant,
                chain_id=chain_id,
                request=request,
            )

        # `from_handle` is a concrete-class classmethod (not on the base); cast
        # to `type[Any]` so the call is type-checkable.
        # Identity parity: the core echoes the variant it registered. Known
        # variants contradicting the resolver's are a core/driver seam bug —
        # never hand back a mislabeled pool. Canonical/unrecognized spellings
        # on either side are the registry census's business, not this guard's.
        echoed_variant = classify_pool_variant(b_var)
        resolved_raw = pool_type.variant if pool_type is not None else None
        resolved_variant = classify_pool_variant(resolved_raw)
        if (
            echoed_variant is not None
            and resolved_variant is not None
            and echoed_variant is not PoolVariant.UNRECOGNIZED
            and resolved_variant is not PoolVariant.UNRECOGNIZED
            and echoed_variant is not resolved_variant
        ):
            raise DegenbotValueError(
                message=(
                    f"build_pool: core identity diverged from the resolver for {address}: "
                    f"resolver variant {resolved_raw!r} vs core echo {b_var!r}"
                )
            )

        # The wrapper class is resolved AFTER the core answers: a family the
        # engine does not model (PoolFamily.UNMODELED — a positively probed
        # Balancer pool outside the weighted/stable families) has no class at
        # all, and its refusal is the construction switchboard's, not a class
        # lookup's.
        pool_class = (
            pool_class_for_descriptor(pool_type, chain_id=chain_id)
            if pool_type is not None
            else CurveStableswapPool
        )
        pool = cast("type[Any]", pool_class).from_handle(py_pool)

        # Bind the resolved identity onto the companion (builder knowledge:
        # the descriptor drove the class choice and the core's family/variant
        # registration). The class label alone is not identity — sushiswap v3
        # shares UniswapV3Pool — so the pool carries the frozen descriptor.
        pool.bind_pool_type(
            pool_type
            if pool_type is not None
            else PoolTypeDescriptor(
                family=family,
                variant=variant,
                kind=derive_kind(family, variant),
                factory=None,
            )
        )
        # Idempotent register: a concurrent registration worker may have built
        # this same shared pool first; use the canonical instance so THIS path
        # still registers instead of being lossily skipped.
        return cast(
            "AbstractLiquidityPool",
            self.pools.get_or_add(pool_address=pool.address, chain_id=chain_id, pool=pool),
        )

    def pools_for_tokens(
        self,
        token_a: str,
        token_b: str,
        *,
        pool_kinds: Sequence[PoolKind] | None = None,
    ) -> list[PoolRef]:
        """Discover the pools that trade a token pair, as inert identity refs.

        Discovery is a *database* question, not a derivation: a token pair is not
        a pool identity (a V2 pool, a PancakeSwap V3 pool, and a V4-managed pool
        can all trade the same pair, and V4 hashes its hooks into the PoolId). One
        graph read over this Bot's database answers it; nothing is constructed.

        The three-layer ladder:

        - **discovery finds** — this method returns ``list[PoolRef]`` (identity
          views: address/manager+pool_hash, family, tokens — no state, no handle);
        - **the registry remembers** — ``self.pools.get(...)`` returns an already
          built instance, or ``None``;
        - **construction commits** — ``self.build_pool(ref.address)`` materializes
          the pool (memoized: a registry hit is free, a miss is one loud build).

        Args:
            token_a: One token's checksummed (or lower/upper) address.
            token_b: The other token's address.
            pool_kinds: Optional family filter; defaults to every family.

        Returns:
            The pools trading the pair, each an identity ref. Empty when either
            token is unknown to the database or no pool trades the pair.

        """
        return _pools_for_tokens(
            self.database_path,
            self.chain_id,
            token_a,
            token_b,
            pool_kinds=pool_kinds,
        )

    def _make_v3_tick_data_fetcher(
        self,
        pool_address: str,
        chain_id: ChainId,
    ) -> Callable[[int, int], FetchedTickData | None]:
        """Create the V3 tick-data backfill fetcher for a pool address.

        The legacy web3-sync fetcher factory formerly on the
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
                liquidity_at_tick=LiquidityAtTick,
                tick_struct_types=UniswapV3Pool.TICK_STRUCT_TYPES,
            ),
        )

    def _make_v4_tick_data_fetcher(
        self,
        pool_id: bytes,
        pool_manager_address: str,
        state_view_address: str,
        chain_id: ChainId,
    ) -> Callable[[int, int], FetchedTickData | None]:
        """Create the V4 tick-data backfill fetcher for a managed pool.

        The legacy web3-sync fetcher factory formerly on the
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
        address: ChecksummedAddress,
        chain_id: ChainId,
        io: BotIo,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Dispatch to the builder with a typed request.

        Returns:
            The computed value.

        """
        return builder.build(address, chain_id=chain_id, io=io, request=request)

    def _register_delegated_tokens(
        self,
        py_pool: Pool,
        *,
        variant: str | None,
        chain_id: ChainId,
        request: BuildPoolRequest,
    ) -> None:
        """Register a delegated pool's tokens in this Bot (ADR-006).

        V2/V3 families expose exactly two (`token0_address`/`token1_address`);
        Balancer exposes N via the family-specific token-address getter, keyed
        by the descriptor `variant` (data), never a subclass test.

        """
        if variant in {"balancer_weighted", "balancer_stable"}:
            token_addresses = (
                py_pool.balancer_token_addresses
                if variant == "balancer_weighted"
                else py_pool.balancer_stable_token_addresses
            )
            for token_address in token_addresses:
                self._erc20_builder.build(
                    token_address,
                    chain_id=chain_id,
                    silent=request.silent,
                    io=self._io,
                )
            return

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

    def build_managed_pool(  # ruff:ignore[too-many-locals] - the single V4 facade delegation
        self,
        address: str,
        request: BuildManagedPoolRequest,
    ) -> UniswapV4Pool:
        """Build a V4 managed pool from a PoolManager address and pool ID.

        ``address`` is the PoolManager contract; ``request`` carries the pool
        ID plus everything the pre-fetched identity may need (silent/
        state_block common options, the V4 immutable data, and pre-fetched
        tick data) — see :class:`BuildManagedPoolRequest` for the per-field
        contract: when the pool is not in the database,
        ``state_view_address``, ``tokens``, ``fee``, ``tick_spacing`` must
        all be provided.

        The build is one delegation to the core: ``resolve_v4_identity``
        performs the DB two-step (manager → v4 row → per-FK tokens) first,
        else the request's caller-supplied overrides; the pool's two tokens
        build in ONE batched metadata read; then ``build_v4_pool`` fetches
        slot0/liquidity FRESH, assembles the tick map (Db → Chain precedence),
        and registers into ``BotState`` atomically, echoing the identity back
        for the parity guard. The fee overrides ride back from the SAME
        head-stamped slot0 read, so the companion never issues a second
        ``fetch_v4_slot0_liquidity`` per pool.

        Returns:
            A `UniswapV4Pool` companion wrapping the Rust-registered pool.

        Raises:
            DegenbotValueError: If neither the DB two-step nor the request
                overrides complete the identity, or if the builder-echoed
                identity diverges from the core-resolved identity.

        """
        address = get_checksum_address(address)
        chain_id = self.chain_id

        # Check managed pool registry — return existing pool if already built
        existing = self.managed_pools.get(
            chain_id=chain_id,
            pool_manager_address=address,
            pool_id=to_bytes(request.pool_id),
        )
        if existing is not None:
            if TYPE_CHECKING:
                assert isinstance(existing, UniswapV4Pool)
            return existing

        pool_id_bytes = to_bytes(request.pool_id)
        pool_manager_address = get_checksum_address(address)

        head = self._io.get_block_number()
        state_block = request.state_block if request.state_block is not None else head

        # DB-arm eligibility (the `build_pool` V3 twin): the DB liquidity map
        # is a CURRENT-state cache, so a pinned `state_block` behind the
        # current head is a HISTORICAL construction and must skip the DB arm —
        # `db=False` forces the core's Chain-arm Sparse path at the pinned
        # block instead of failing the chain-truth intake gate on head-stamped
        # rows.
        db_seed = state_block >= head
        # The FRESH PRICE read block (two-stamp): the cheap slot0/price
        # read stamps `update_block` at the live head, while the liquidity
        # clock + assembled tick map anchor at `state_block`. When the request
        # pins `state_block`, price = that same block (no split).
        # Identity: the core resolver performs the DB two-step (manager → v4
        # row → per-FK tokens) first, else the request's caller-supplied
        # overrides — the driver neither reads the DB nor assembles the
        # identity itself.
        over_tokens = request.tokens
        try:
            (
                currency0_address,
                currency1_address,
                fee_for_pool,
                tick_spacing_for_pool,
                hook_flags,
                hook_address,
                state_view_hex,
            ) = self._py_bot.resolve_v4_identity(
                chain_id=int(chain_id),
                pool_manager=pool_manager_address,
                pool_id_hex=to_0x_hex(pool_id_bytes),
                currency0=over_tokens[0] if over_tokens else None,
                currency1=over_tokens[1] if over_tokens else None,
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

        # The pool's two tokens build in ONE batched metadata read:
        # `build_many` collapses the two per-token fetch_erc20_metadata
        # round-trips into a single Multicall3 aggregate3 eth_call for
        # network-missing metadata.
        token0, token1 = self._erc20_builder.build_many(
            [currency0_address, currency1_address],
            chain_id=chain_id,
            silent=request.silent,
            io=self._io,
        )

        (
            pool_handle_pool_id,
            _coverage,
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
            pool_id_hex=to_0x_hex(pool_id_bytes),
            currency0=currency0_address,
            currency1=currency1_address,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            # The REAL hook address (pool-ID mismatch regression): the
            # core registers it in the pool key so the identity round-trips
            # keccak(abi.encode(pool_key)). The flag mask is derived core-side.
            hook_address=hook_address,
            state_view_address=state_view_address,
            block=int(state_block) if state_block is not None else None,
            db=db_seed,
            tick_data_fetcher=self._make_v4_tick_data_fetcher(
                pool_id_bytes,
                pool_manager_address,
                state_view_address,
                chain_id,
            ),
        )
        # Return-surface parity: the identity the builder echoes
        # back must match what the core resolver produced (a divergence is a
        # real seam bug), and the pool_id must round-trip the requested hash.
        # hook flags are derived from the same hook address on both sides.
        if not all([
            b_cur0.lower() == currency0_address.lower(),
            b_cur1.lower() == currency1_address.lower(),
            b_pm.lower() == pool_manager_address.lower(),
            int(b_fee) == int(fee_for_pool),
            int(b_ts) == int(tick_spacing_for_pool),
            int(b_hf) == int(hook_flags),
            b_pool_id_hex.lower() == to_0x_hex(pool_id_bytes).lower(),
        ]):
            raise DegenbotValueError(
                message=(
                    "V4 builder identity `build_v4_pool` diverged from the "
                    "resolved identity (currency0/1, pool_manager, fee, "
                    "tick_spacing, hook_flags, pool_id)."
                )
            )
        py_pool_handle = self._py_bot.get_pool(pool_handle_pool_id)
        assert py_pool_handle is not None, "build_v4_pool returned a pool_id with no handle"

        pool = UniswapV4Pool.from_handle(py_pool_handle)
        # Builder-supplied values the seam defaults; override from RPC.
        pool._state_view_address = state_view_address  # ruff:ignore[private-member-access]
        pool.protocol_fee = ProtocolFee(
            zero_for_one=b_protocol_fee & 0xFFF,
            one_for_zero=b_protocol_fee >> 12,
        )
        pool.lp_fee = int(b_lp_fee)

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
            self._log_v4_pool(pool, token0, token1)

        return pool

    @staticmethod
    def _log_v4_pool(pool: UniswapV4Pool, token0: Erc20Token, token1: Erc20Token) -> None:
        """Trace-log a freshly built V4 managed pool (the caller gates on `silent`)."""
        logger.info(pool.name)
        logger.info(f"• ID: {to_0x_hex(pool.pool_id)}")
        logger.info(f"• Token 0: {token0}")
        logger.info(f"• Token 1: {token1}")
        logger.info(f"• Liquidity: {pool.liquidity}")
        logger.info(f"• SqrtPrice: {pool.sqrt_price_x96}")
        logger.info(f"• Tick: {pool.tick}")

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch the current state of a pool from the chain and apply it.

        Delegates to the pool handle's core-owned refresh entry
        (``pool.refresh_via`` -> ``refresh_pool``): the per-family
        fetch/compare/apply choreography lives core-side, so this shell no
        longer selects a builder or branches on the pool family.

        `block_number` is a block number, one canonical tag string
        (``"earliest"`` / ``"latest"`` / ``"pending"``), or ``None`` for the
        chain head. A tag is handed through unchanged and resolves at the
        handle seam that owns the RPC clock — it is never pre-coerced to a
        number here.

        Returns:
            True if the state changed, False if the pool was already current.

        """
        return pool.refresh_via(self._io, block_number=block_number)
