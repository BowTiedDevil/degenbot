"""UniswapV3Pool: concentrated liquidity AMM companion over a LiquidityPool handle.

ADR-005 slice 8b — the V3 companion rewritten over the same `LiquidityPool`
handle topology as the V2 `UniswapV2Pool`. Rust `BotState` is the single
source of truth for V3 mutable state (scalars, tick data, reorg journal);
this companion reads it through `self._py_pool` (the atomic `snapshot_v3()`
for scalars + `tick_data_snapshot()`/`tick_bitmap_snapshot()` for the tick maps)
and delegates `external_update` (Swap) / `update_liquidity_map` (Mint/Burn) /
`update_tick_data` (sparse-map backfill) / discard / restore to the handle.
Immutable identity (tokens, factory, fee, tick_spacing) stays Python-side —
matches V2 (calc lives in the `UniswapV3PoolCalc` mixin).

`_state_mgr` / `_state_cache` / `state_cache_depth` are dropped — the
`StateCache` temporal-navigation layer lives in Rust now (journal +
discard/restore). V2 already has none; V3 follows.

Sparse-map bitmap note: Rust's tick bitmap is DERIVED from `tick_data` keys
(no separate bitmap store), and the CHECKED words are tracked in Rust
(`known_bitmap_words` — seeded at Sparse registration, grown by fetch-merge /
full-sync, and grown by the `update_tick_data` FFI seam from the caller's
tick_bitmap KEYS — Sparse only, never Tracked). `tick_bitmap_snapshot()`
surfaces a known-but-empty word as `(0, block)`, so the simulator sees it as
present-but-zero (not missing) and the fetch loop breaks with no client-side
shadow. A word ABSENT from the snapshot is indeterminate on a Sparse pool
(fetch it) and known-empty on a Tracked pool (complete map).
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Any, ClassVar, Self, TypedDict

from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    LiquidityPoolError,
)
from degenbot.uniswap.cl_companion import ConcentratedLiquidityCompanion
from degenbot.uniswap.v3_pool_calc import UniswapV3PoolCalc
from degenbot.uniswap.v3_pool_state import V3PoolState
from degenbot.uniswap.v3_types import (
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)

if TYPE_CHECKING:
    from degenbot.types import LiquidityPool
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress

type Token0Amount = int
type Token1Amount = int


class LiquidityAtTickAsDict(TypedDict):
    """Serialized form of ``LiquidityAtTick`` for tick data interchange."""

    liquidity_net: int
    liquidity_gross: int
    block: BlockNumber


class BitmapAtWordAsDict(TypedDict):
    """Serialized form of ``BitmapAtWord`` for tick bitmap interchange."""

    bitmap: int
    block: BlockNumber


class UniswapV3Pool(
    V3PoolState,
    UniswapV3PoolCalc,
    ConcentratedLiquidityCompanion,
):
    """A Uniswap V3 concentrated-liquidity pool companion over a ``LiquidityPool`` handle.

    Rust owns the mutable state (scalars + tick data + reorg journal) as
    ``V3PoolState``; this companion reads it through ``self._py_pool`` (one
    atomic ``snapshot_v3()`` for scalars + ``tick_data_snapshot()`` /
    ``tick_bitmap_snapshot()`` for the tick maps) and delegates
    ``external_update`` (Swap) / ``update_liquidity_map`` (Mint/Burn) /
    ``update_tick_data`` (sparse-map backfill) / discard / restore to the
    handle. Immutable identity (tokens, factory, fee, tick_spacing) stays
    Python-side — matches V2.

    Construct via ``Bot.build_pool()`` (which registers in Rust and hands the
    handle here); tests use ``make_v3_pool``.
    """

    variant: ClassVar[str | None] = None

    type PoolState = UniswapV3PoolState

    # Instance attributes set in `_from_py_pool` (the only construction seam —
    # `__init__` raises). Declared at class scope so the type checker tracks
    # them without inline annotations on the classmethod body.
    _py_pool: LiquidityPool
    address: ChecksummedAddress
    factory: ChecksummedAddress
    _fee: int
    _tick_spacing: int
    _token0: Erc20Token
    _token1: Erc20Token
    init_hash: str
    deployer_address: ChecksummedAddress
    name: str
    _initial_state_block: int

    TICK_STRUCT_TYPES = (
        "uint128",
        "int128",
        "uint256",
        "uint256",
        "int56",
        "uint160",
        "uint32",
        "bool",
    )
    SLOT0_STRUCT_TYPES = (
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "uint8",
        "bool",
    )

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``UniswapV3Pool`` is a Python companion over a Rust-owned
        ``LiquidityPool`` handle. The handle can only be produced by
        registering a pool in a ``Bot`` — there is no way for a caller to
        hand-build one. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_v3_pool(...)``

        Both register the pool in Rust, obtain the ``LiquidityPool``
        handle, and wrap it via :meth:`_from_py_pool` (mirroring Polars'
        ``_from_pydf`` seam).

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or make_v3_pool(...) "
            "(tests) to register the pool in Rust and obtain the "
            "LiquidityPool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_pool(cls, py_pool: LiquidityPool) -> Self:
        """Wrap a Rust-owned ``LiquidityPool`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). The
        handle is self-describing: every identity field (address, factory,
        fee, tick_spacing, tokens) is read off it — no identity is passed as
        constructor args. Rust owns the mutable state (slot0 + tick_data +
        reorg journal) as ``V3PoolState`` and the immutable registration
        metadata as ``V3PoolIdentity``; this companion reads both through
        ``self._py_pool``.

        The sparse-tick fetcher is stored Rust-side on ``V3PoolState``
        (ADR-006 I/O trait object, task MLJT4V) — not a constructor arg.
        Checked words (bitmap words the caller has verified) live in Rust
        ``known_bitmap_words``; ``tick_bitmap_snapshot()`` surfaces them, so
        there is no client-side bitmap shadow.

        Returns:
            A ``cls`` instance wrapping ``py_pool``.

        Raises:
            DegenbotValueError: If the handle is not a V3-family pool
                (``py_pool.pool_family`` is not ``"v3"``).

        """
        self = cls.__new__(cls)
        self._py_pool = py_pool

        # Variant-family guard (uniform precondition every seam uses).
        if py_pool.pool_family != "v3":
            msg = (
                "LiquidityPool handle is not a V3-family pool "
                f"(got pool_family {py_pool.pool_family!r}); "
                "UniswapV3Pool._from_py_pool requires a handle "
                "registered via register_v3_pool"
            )
            raise DegenbotValueError(message=msg)

        # Identity — all read off the handle (no shadow kwargs).
        self.address = get_checksum_address(py_pool.address)
        self.factory = get_checksum_address(py_pool.factory)
        self._fee = py_pool.fee
        self._tick_spacing = py_pool.tick_spacing

        py_token0 = py_pool.get_token0()
        py_token1 = py_pool.get_token1()
        if py_token0 is None or py_token1 is None:
            msg = (
                "pool tokens must be registered in the same Bot as the pool "
                "(ADR-006): get_token0/get_token1 returned None"
            )
            raise DegenbotValueError(message=msg)
        self._token0 = Erc20Token._from_py_token(py_token0)  # ruff:ignore[private-member-access]
        self._token1 = Erc20Token._from_py_token(py_token1)  # ruff:ignore[private-member-access]

        # Deployer / init-hash: read off the Rust handle (Fork A, P62DKO).
        # The builder resolved the JSON-sourced deployer (effective deployer,
        # covering PancakeSwap V3's separate-deployer case) + init_hash at
        # registration; the companion reads them here instead of the retired
        # `UNISWAP_V3_MAINNET_POOL_INIT_HASH` ClassVar. Non-JSON V3 pools get
        # the factory as deployer + the mainnet fallback init hash.
        self.deployer_address = get_checksum_address(self._py_pool.deployer or self.factory)
        self.init_hash = self._py_pool.init_hash

        # The block of the registration snapshot (genesis journal delta).
        self._initial_state_block = self._py_pool.update_block

        self.name = (
            f"{self._token0}-{self._token1} ({self.__class__.__name__}, "
            f"{100 * self._fee / self.FEE_DENOMINATOR:.2f}%)"
        )

        # The sparse-map fact is Rust-side (coverage — T2 FBJTUM: the
        # double-tracked companion flags are retired) and the sparse-word
        # fetcher is stored Rust-side on the V3 state (ADR-006 I/O trait
        # object, task MLJT4V).
        return self

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the pool.

        """
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1}, fee={100 * self._fee / self.FEE_DENOMINATOR:.2f}%, tick spacing={self._tick_spacing})"  # ruff:ignore[line-too-long]

    def __str__(self) -> str:
        """Return the canonical string representation.

        Returns:
            The pool name string.

        """
        return self.name

    @property
    def state(self) -> PoolState:
        """State.

        Returns:
            The current pool state, built from one atomic Rust scalar snapshot
            (``_py_pool.snapshot_v3()``) + the tick-map snapshots. The scalars
            (sqrt_price/liquidity/tick/block) cannot tear; the tick maps are
            deep-copied snapshots the simulation path can mutate freely.

        Raises:
            DegenbotValueError: If the pool is not registered in Rust.

        """
        snap = self._py_pool.snapshot_v3()
        if snap is None:
            msg = "No V3 pool state available (pool not registered in Rust)"
            raise DegenbotValueError(message=msg)
        sqrt_price_x96, liquidity, tick, block = snap
        return self.PoolState.__value__(
            address=self.address,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            tick_bitmap=self.tick_bitmap,
            tick_data=self.tick_data,
            block=block,
        )

    def simulate_exact_input_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Simulate an exact input swap.

        Returns:
            The simulation result with delta amounts and state transitions.

        Raises:
            DegenbotValueError: If token_in is unknown.
            LiquidityPoolError: If the simulated execution reverts.

        """
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message=f"Unknown token {token_in}")

        # Capture initial state before any potential modifications (e.g., tick data fetching)
        initial_state = override_state if override_state is not None else self.state

        zero_for_one = token_in == self._token0

        if override_state is None:
            # ADR-005 slice 3b/4: mainline exact-input swap (no override)
            # delegates to the Rust fetch+retry seam. For a sparse pool the
            # Rust path misses on an unknown word, fetches it via
            # ``_apply_fetched_tick_word`` (the return-data fetcher), merges,
            # and retries; for a dense pool it computes directly. A custom
            # ``sqrt_price_limit`` threads through (the seam honours it); the
            # override path routes to the Rust override seam
            # (``simulate_swap_with_override`` — see below).
            outcome = self._py_pool.simulate_swap_with_fetch(
                zero_for_one=zero_for_one,
                amount_in=token_in_quantity,
                block=self.update_block,
                sqrt_price_limit_x96=sqrt_price_limit_x96,
            )
            if outcome is None:
                raise LiquidityPoolError(
                    message=(
                        f"Simulated execution could not compute. "
                        f"pool={self.address} zfo={zero_for_one} "
                        f"amount_in={token_in_quantity}"
                    ),
                )
            (
                rust_amount0,
                rust_amount1,
                end_sqrt_price_x96,
                end_liquidity,
                end_tick,
            ) = (int(x) for x in outcome)
            # Rust returns UNSIGNED absolute amounts; the V3 convention is
            # signed deltas (deposited +, sent -). zfo exact-in deposits
            # token0 / sends token1; ofz exact-in deposits token1 / sends
            # token0. (Pinned by test_rust_seam_sign_mapping_dense.)
            amount0_delta = rust_amount0 if zero_for_one else -rust_amount0
            amount1_delta = -rust_amount1 if zero_for_one else rust_amount1
        else:
            # ADR-005 slice 4: exact-input swap over an override (arbitrage
            # hypothetical) delegates to the Rust fetch-enhanced override seam.
            # The override runs over a TRANSIENT state; sparse misses fetch+retry
            # into the transient state (NOT registered BotState). A custom
            # sqrt_price_limit threads through.
            outcome = self._py_pool.simulate_swap_with_override(
                zero_for_one=zero_for_one,
                amount_in=token_in_quantity,
                block=self.update_block,
                override_sqrt_price_x96=override_state.sqrt_price_x96,
                override_liquidity=override_state.liquidity,
                override_tick=override_state.tick,
                override_tick_data={
                    tick: (la.liquidity_gross, la.liquidity_net, la.block)
                    for tick, la in override_state.tick_data.items()
                },
                sqrt_price_limit_x96=sqrt_price_limit_x96,
            )
            if outcome is None:
                raise LiquidityPoolError(
                    message=(
                        f"Simulated execution could not compute. "
                        f"pool={self.address} zfo={zero_for_one} "
                        f"amount_in={token_in_quantity} override"
                    ),
                )
            (
                rust_amount0,
                rust_amount1,
                end_sqrt_price_x96,
                end_liquidity,
                end_tick,
            ) = (int(x) for x in outcome)
            amount0_delta = rust_amount0 if zero_for_one else -rust_amount0
            amount1_delta = -rust_amount1 if zero_for_one else rust_amount1
        return UniswapV3PoolSimulationResult(
            amount0_delta=amount0_delta,
            amount1_delta=amount1_delta,
            initial_state=initial_state,
            final_state=dataclasses.replace(
                initial_state,
                liquidity=end_liquidity,
                sqrt_price_x96=end_sqrt_price_x96,
                tick=end_tick,
                block=self.update_block if override_state is None else initial_state.block,
            ),
        )

    def simulate_exact_output_swap(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Simulate an exact output swap.

        Returns:
            The simulation result with delta amounts and state transitions.

        Raises:
            DegenbotValueError: If token_out is unknown.
            LiquidityPoolError: If the simulated execution reverts.

        """
        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message=f"Unknown token {token_out}")

        # Capture initial state before any potential modifications (e.g., tick data fetching)
        initial_state = override_state if override_state is not None else self.state

        zero_for_one = token_out == self._token1

        if override_state is None:
            # ADR-005 slice 4: mainline exact-output swap (no override) delegates
            # to the Rust exact-output fetch+retry seam. The V3 exact-output
            # sign (amountSpecified < 0) is handled in the Rust core; a custom
            # sqrt_price_limit threads through (the seam honours it). The
            # override path routes to the Rust exact-output override seam
            # (``simulate_exact_output_swap_with_override`` — see below).
            outcome = self._py_pool.simulate_exact_output_swap_with_fetch(
                zero_for_one=zero_for_one,
                amount_out=token_out_quantity,
                block=self.update_block,
                sqrt_price_limit_x96=sqrt_price_limit_x96,
            )
            if outcome is None:
                raise LiquidityPoolError(
                    message=(
                        f"Simulated execution could not compute. "
                        f"pool={self.address} zfo={zero_for_one} "
                        f"amount_out={token_out_quantity}"
                    ),
                )
            (
                rust_amount0,
                rust_amount1,
                end_sqrt_price_x96,
                end_liquidity,
                end_tick,
            ) = (int(x) for x in outcome)
            # Rust returns UNSIGNED absolute amounts; the V3 convention is
            # signed deltas (deposited +, sent -). zfo exact-out deposits
            # token0 (required input) / sends token1 (requested output);
            # ofz deposits token1 / sends token0. Same mapping as exact-input.
            amount0_delta = rust_amount0 if zero_for_one else -rust_amount0
            amount1_delta = -rust_amount1 if zero_for_one else rust_amount1
        else:
            # ADR-005 slice 4: exact-output swap over an override (arbitrage
            # hypothetical) delegates to the Rust fetch-enhanced exact-output
            # override seam. A custom sqrt_price_limit threads through.
            outcome = self._py_pool.simulate_exact_output_swap_with_override(
                zero_for_one=zero_for_one,
                amount_out=token_out_quantity,
                block=self.update_block,
                override_sqrt_price_x96=override_state.sqrt_price_x96,
                override_liquidity=override_state.liquidity,
                override_tick=override_state.tick,
                override_tick_data={
                    tick: (la.liquidity_gross, la.liquidity_net, la.block)
                    for tick, la in override_state.tick_data.items()
                },
                sqrt_price_limit_x96=sqrt_price_limit_x96,
            )
            if outcome is None:
                raise LiquidityPoolError(
                    message=(
                        f"Simulated execution could not compute. "
                        f"pool={self.address} zfo={zero_for_one} "
                        f"amount_out={token_out_quantity} override"
                    ),
                )
            (
                rust_amount0,
                rust_amount1,
                end_sqrt_price_x96,
                end_liquidity,
                end_tick,
            ) = (int(x) for x in outcome)
            amount0_delta = rust_amount0 if zero_for_one else -rust_amount0
            amount1_delta = -rust_amount1 if zero_for_one else rust_amount1
        return UniswapV3PoolSimulationResult(
            amount0_delta=amount0_delta,
            amount1_delta=amount1_delta,
            initial_state=initial_state,
            final_state=dataclasses.replace(
                initial_state,
                liquidity=end_liquidity,
                sqrt_price_x96=end_sqrt_price_x96,
                tick=end_tick,
                block=self.update_block if override_state is None else initial_state.block,
            ),
        )
