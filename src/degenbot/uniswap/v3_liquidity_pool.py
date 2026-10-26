"""UniswapV3Pool: concentrated liquidity AMM companion over a Pool handle.

The V3 companion sits on the same `Pool` handle topology as the V2
`UniswapV2Pool`. Rust `BotState` is the single
source of truth for V3 mutable state (scalars, tick data, reorg journal);
this companion reads it through `self._py_pool` (the atomic `snapshot_v3()`
for scalars + `tick_data()`/`tick_bitmap` for the tick maps). The companion is
a READ surface: state moves on the pump's own door (`Bot.dispatch_log` →
`LogDispatcher` decode → apply), never through a companion write method.
Immutable identity (tokens, factory, fee, tick_spacing) stays Python-side —
matches V2 (calc lives in the `UniswapV3PoolCalc` mixin).

`_state_mgr` / `_state_cache` / `state_cache_depth` are dropped — the
`StateCache` temporal-navigation layer lives in Rust now (journal +
discard/restore). V2 already has none; V3 follows.

Sparse-map bitmap note: Rust's tick bitmap is DERIVED from `tick_data` keys
(no separate bitmap store, and no stored bitmap VALUE — the word-value
machinery is retired), and the CHECKED words are tracked in Rust
(`known_bitmap_words` — seeded at Sparse registration from the caller's
checked-word set, grown by fetch-merge / full-sync — Sparse only, never
Tracked). `tick_bitmap` surfaces the
known WORD positions as a `set[int]`, so a checked-but-empty word reads as
present (not missing) and the fetch loop breaks with no client-side shadow. A
word ABSENT from the set is indeterminate on a Sparse pool (fetch it) and
known-empty on a Tracked pool (complete map).
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Any, ClassVar, Self, TypedDict, cast

from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import InvalidSwapInputAmount
from degenbot.uniswap import SwapRefused, swap_refusal_to_error
from degenbot.uniswap.concentrated_liquidity_base import ConcentratedLiquidityBase
from degenbot.uniswap.v3_pool_calc import UniswapV3PoolCalc
from degenbot.uniswap.v3_pool_state import V3PoolState
from degenbot.uniswap.v3_types import (
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)

if TYPE_CHECKING:
    from degenbot.types import Pool
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress


class LiquidityAtTickAsDict(TypedDict):
    """Serialized form of ``LiquidityAtTick`` for tick data interchange."""

    liquidity_net: int
    liquidity_gross: int
    block: BlockNumber


class UniswapV3Pool(
    V3PoolState,
    UniswapV3PoolCalc,
    ConcentratedLiquidityBase,
):
    """A Uniswap V3 concentrated-liquidity pool companion over a ``Pool`` handle.

    Rust owns the mutable state (scalars + tick data + reorg journal) as
    ``V3PoolState``; this companion reads it through ``self._py_pool`` (one
    atomic ``snapshot_v3()`` for scalars + ``tick_data_snapshot()`` /
    ``tick_data()``/``tick_bitmap`` for the tick maps). Immutable identity
    (tokens, factory, fee, tick_spacing) stays Python-side — matches V2.

    Construct via ``Bot.build_pool()`` (which registers in Rust and hands the
    handle here); tests use ``make_v3_pool``.
    """

    variant: ClassVar[str | None] = None

    type PoolState = UniswapV3PoolState

    # Instance attributes set in `from_handle` (the only construction seam —
    # `__init__` raises). Declared at class scope so the type checker tracks
    # them without inline annotations on the classmethod body.
    _py_pool: Pool
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
        ``Pool`` handle. The handle can only be produced by
        registering a pool in a ``Bot`` — there is no way for a caller to
        hand-build one. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_v3_pool(...)``

        Both register the pool in Rust, obtain the ``Pool``
        handle, and wrap it via :meth:`from_handle` (mirroring Polars'
        ``_from_pydf`` seam).

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or make_v3_pool(...) "
            "(tests) to register the pool in Rust and obtain the "
            "Pool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def from_handle(cls, py_pool: Pool) -> Self:
        """Wrap a Rust-owned ``Pool`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). The
        handle is self-describing: every identity field (address, factory,
        fee, tick_spacing, tokens) is read off it — no identity is passed as
        constructor args. Rust owns the mutable state (slot0 + tick_data +
        reorg journal) as ``V3PoolState`` and the immutable registration
        metadata as ``V3PoolIdentity``; this companion reads both through
        ``self._py_pool``.

        The sparse-tick fetcher is stored Rust-side on ``V3PoolState``
        (ADR-006 I/O trait object) — not a constructor arg.
        Checked words (bitmap words the caller has verified) live in Rust
        ``known_bitmap_words``; ``tick_bitmap`` surfaces them (word positions
        only), so there is no client-side bitmap shadow.

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
                "Pool handle is not a V3-family pool "
                f"(got pool_family {py_pool.pool_family!r}); "
                "UniswapV3Pool.from_handle requires a handle "
                "registered via register_v3_pool"
            )
            raise DegenbotValueError(message=msg)

        # Identity — all read off the handle (no shadow kwargs).
        self.address = get_checksum_address(py_pool.address)
        self.factory = get_checksum_address(py_pool.factory)
        view = py_pool.concentrated_liquidity()
        self._fee = view.fee
        self._tick_spacing = view.tick_spacing

        py_token0 = py_pool.get_token0()
        py_token1 = py_pool.get_token1()
        if py_token0 is None or py_token1 is None:
            msg = (
                "pool tokens must be registered in the same Bot as the pool "
                "(ADR-006): get_token0/get_token1 returned None"
            )
            raise DegenbotValueError(message=msg)
        self._token0 = Erc20Token.from_handle(py_token0)
        self._token1 = Erc20Token.from_handle(py_token1)

        # Deployer / init-hash: read off the Rust handle (Fork A).
        # The builder resolved the JSON-sourced deployer (effective deployer,
        # covering PancakeSwap V3's separate-deployer case) + init_hash at
        # registration; the companion reads them here instead of the retired
        # `UNISWAP_V3_MAINNET_POOL_INIT_HASH` ClassVar. Non-JSON V3 pools get
        # the factory as deployer + the mainnet fallback init hash.
        self.deployer_address = get_checksum_address(self._py_pool.deployer or self.factory)
        self.init_hash = self._py_pool.init_hash

        # The block of the registration snapshot (genesis journal delta).
        self._initial_state_block = view.update_block

        self.name = (
            f"{self._token0}-{self._token1} ({self.__class__.__name__}, "
            f"{100 * self._fee / self.FEE_DENOMINATOR:.2f}%)"
        )

        # The sparse-map fact is Rust-side (coverage — the
        # double-tracked companion flags are retired) and the sparse-word
        # fetcher is stored Rust-side on the V3 state (ADR-006 I/O trait
        # object).
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
        try:
            view = self._py_pool.concentrated_liquidity()
        except ValueError as exc:
            msg = "No V3 pool state available (pool not registered in Rust)"
            raise DegenbotValueError(message=msg) from exc
        return cast(
            "UniswapV3PoolState",
            self.PoolState.__value__(
                address=self.address,
                liquidity=view.liquidity,
                sqrt_price_x96=view.sqrt_price_x96,
                tick=view.tick,
                tick_bitmap=self.tick_bitmap,
                tick_data=self.tick_data,
                block=view.update_block,
            ),
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
            The simulation result with signed deltas and the end state.

        Raises:
            DegenbotValueError: If token_in is unknown.

        """
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message=f"Unknown token {token_in}")
        zero_for_one = token_in == self._token0
        return self._simulate_swap(
            zero_for_one=zero_for_one,
            amount_specified=-token_in_quantity,
            override_state=override_state,
            sqrt_price_limit_x96=sqrt_price_limit_x96,
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
            The simulation result with signed deltas and the end state.

        Raises:
            DegenbotValueError: If token_out is unknown.

        """
        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message=f"Unknown token {token_out}")
        zero_for_one = token_out == self._token1
        return self._simulate_swap(
            zero_for_one=zero_for_one,
            amount_specified=token_out_quantity,
            override_state=override_state,
            sqrt_price_limit_x96=sqrt_price_limit_x96,
        )

    def _simulate_swap(
        self,
        *,
        zero_for_one: bool,
        amount_specified: int,
        override_state: PoolState | None,
        sqrt_price_limit_x96: int | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Run one simulation through the Rust swap-simulation seam.

        The one verb behind both public entry points: exact-in passes a
        negative ``amount_specified``, exact-out a positive one, and a
        non-``None`` ``override_state`` selects the hypothetical path. The
        seam owns the sign mapping (deposited +, sent -) and the end state;
        this shell only assembles the result value.

        Returns:
            The simulation result with signed deltas and the end state.

        Raises:
            InvalidSwapInputAmount: On a zero-amount swap.
            swap_refusal_to_error: Renders the seam's typed refusal as
                IncompleteSwap (the real pool's revert-analog — the walk
                exhausted the tick range with input remaining) or
                MissingLiquidityData (the walk entered an unfetched word and
                the on-demand fetch failed).

        """
        initial_state = override_state if override_state is not None else self.state
        if amount_specified == 0:
            # The real pool reverts a zero-amount swap; mirror that honestly
            # instead of flattening it into a generic compute failure.
            raise InvalidSwapInputAmount
        # The typed read + the ONE refusal adapter (ADR-037/t2): the seam
        # raises `SwapRefused` with a typed kind; the adapter renders the
        # historical exceptions (IncompleteSwap for the revert-analog
        # verdict, MissingLiquidityData for the unfetchable word).
        try:
            read = self._py_pool.simulate(
                zero_for_one=zero_for_one,
                amount_specified=amount_specified,
                block=self.update_block,
                sqrt_price_limit_x96=sqrt_price_limit_x96,
                override_state=override_state,
            )
        except SwapRefused as exc:
            raise swap_refusal_to_error(exc) from exc
        assert read.liquidity is not None  # pragma: no cover — concentrated-liquidity arm asserted
        assert read.sqrt_price_x96 is not None  # pragma: no cover — CL arm asserted
        assert read.tick is not None  # pragma: no cover — concentrated-liquidity arm asserted
        return UniswapV3PoolSimulationResult(
            amount0_delta=int(read.amount0_delta),
            amount1_delta=int(read.amount1_delta),
            initial_state=initial_state,
            final_state=dataclasses.replace(
                initial_state,
                liquidity=read.liquidity,
                sqrt_price_x96=read.sqrt_price_x96,
                tick=read.tick,
                block=self.update_block if override_state is None else initial_state.block,
            ),
        )
