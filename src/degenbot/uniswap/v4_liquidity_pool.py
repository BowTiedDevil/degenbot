"""UniswapV4Pool: concentrated liquidity AMM companion over a LiquidityPool handle.

ADR-005 slice 9b — the V4 companion rewritten over the same `LiquidityPool`
handle topology as the V3 companion. Rust `BotState` is the single source of
truth for V4 mutable state (scalars, tick data, reorg journal); this companion
reads it through `self._py_pool` (atomic `snapshot_v3()` for scalars — already
V3/V4-generic via `get_v3_or_v4_pool` — + `tick_data_snapshot()`/
`tick_bitmap_snapshot()` for the tick maps) and delegates
`external_update` (Swap) / `update_liquidity_map` (ModifyLiquidity) /
`update_tick_data` (sparse-map backfill) / discard / restore to the handle.

`_state_mgr` / `_state_cache` / `state_cache_depth` are dropped — the
`StateCache` temporal-navigation layer lives in Rust now (journal +
discard/restore). V3 already has none; V4 follows.

V4-specific identity (pool_id, pool_manager_address, pool_key, hook_address,
protocol_fee, lp_fee, state_view_address) stays Python-side — matches V3
keeping tokens/factory/fee Python-side. The hook admission floor (reject
amount-modifying hooks + dynamic fees) lives in Rust
(`BotState::register_v4_pool`), surfaced at `Bot.register_v4_pool` (ADR-005
slice 9a) so the companion never holds a hooked pool.

`_bitmap_override` mirrors V3: the verbatim tick_bitmap words the
builder/snapshot/fetcher passed via `update_tick_data`, overlaid on the
Rust-derived bitmap so a snapshot's on-chain bitmap is preserved verbatim AND
a fetcher-checked empty word is seen as present-but-zero.
"""

import dataclasses
from enum import Enum
from typing import Any, Self
from weakref import WeakSet

from hexbytes import HexBytes

from degenbot._ffi import ChecksummedAddress
from degenbot.abi import encode
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.crypto import keccak256
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    ExternalUpdateError,
    HookedPoolResult,
    IncompleteSwap,
    LiquidityPoolError,
    NoPoolStateAvailable,
)
from degenbot.types import LiquidityPool
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.math import (
    get_tick_word_and_bit_position as cl_get_tick_word_and_bit_position,
)
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v4_pool_calc import UniswapV4PoolCalc
from degenbot.uniswap.v4_pool_state import V4PoolState
from degenbot.uniswap.v4_types import (
    InitializedTickMap,
    LiquidityMap,
    Pip,
    UniswapV4PoolExternalUpdate,
    UniswapV4PoolKey,
    UniswapV4PoolLiquidityMappingUpdate,
    UniswapV4PoolState,
    UniswapV4PoolStateUpdated,
)


@dataclasses.dataclass(slots=True)
class SwapResult:
    """SwapResult class."""

    sqrt_price_x96: int
    tick: int
    liquidity: int


@dataclasses.dataclass(slots=True, frozen=True)
class SwapDelta:
    """SwapDelta class."""

    currency0: int
    currency1: int

    @property
    def amount_in(self) -> int:
        """The deposited token amount."""
        return -min(self.currency0, self.currency1)

    @property
    def amount_out(self) -> int:
        """The withdrawn token amount."""
        return max(self.currency0, self.currency1)


@dataclasses.dataclass(slots=True, frozen=True)
class ProtocolFee:
    """ProtocolFee class."""

    zero_for_one: int
    one_for_zero: int


@dataclasses.dataclass(slots=True, frozen=True)
class Slot0:
    """Slot0 class."""

    sqrt_price_x96: int
    tick: int
    protocol_fee: ProtocolFee
    lp_fee: int


PIPS_DENOMINATOR = 1_000_000
NATIVE_CURRENCY_ADDRESS = ZERO_ADDRESS


class Hooks(Enum):
    """Hooks class."""

    # ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/Hooks.sol
    BEFORE_INITIALIZE = 1 << 13
    AFTER_INITIALIZE = 1 << 12
    BEFORE_ADD_LIQUIDITY = 1 << 11
    AFTER_ADD_LIQUIDITY = 1 << 10
    BEFORE_REMOVE_LIQUIDITY = 1 << 9
    AFTER_REMOVE_LIQUIDITY = 1 << 8
    BEFORE_SWAP = 1 << 7
    AFTER_SWAP = 1 << 6
    BEFORE_DONATE = 1 << 5
    AFTER_DONATE = 1 << 4
    BEFORE_SWAP_RETURNS_DELTA = 1 << 3
    AFTER_SWAP_RETURNS_DELTA = 1 << 2
    AFTER_ADD_LIQUIDITY_RETURNS_DELTA = 1 << 1
    AFTER_REMOVE_LIQUIDITY_RETURNS_DELTA = 1 << 0


class UniswapV4Pool(
    PublisherMixin,
    V4PoolState,
    UniswapV4PoolCalc,
    AbstractLiquidityPool,
):
    """A Uniswap V4 concentrated-liquidity pool companion over a ``LiquidityPool`` handle.

    Rust owns the mutable state (scalars + tick data + reorg journal) as
    ``V4PoolState``; this companion reads it through ``self._py_pool`` (one
    atomic ``snapshot_v3()`` for scalars — already V3/V4-generic via
    ``get_v3_or_v4_pool`` — + ``tick_data_snapshot()`` /
    ``tick_bitmap_snapshot()`` for the tick maps) and delegates
    ``external_update`` (Swap) / ``update_liquidity_map`` (ModifyLiquidity) /
    ``update_tick_data`` (sparse-map backfill) / discard / restore to the
    handle. V4-specific identity (pool_id, pool_manager, pool_key, hooks,
    protocol_fee, lp_fee, state_view_address) stays Python-side — matches V3.

    Construct via the V4 builder (which registers in Rust and hands the handle
    here); tests use ``make_v4_pool``.

    Hook admission floor: pools with amount-modifying hooks (`hook_flags & 0xCC
    != 0`) or dynamic fees (`fee == 0x100000`) are rejected in Rust
    (`BotState::register_v4_pool`), surfaced at `Bot.register_v4_pool` as
    typed exceptions — so this companion never holds a hooked/dynamic-fee pool.
    """

    type PoolState = UniswapV4PoolState

    # Instance attributes set in `_from_py_pool` (the only construction seam).
    _py_pool: LiquidityPool
    _pool_id: HexBytes
    _pool_manager_address: ChecksummedAddress
    hook_address: ChecksummedAddress
    _state_view_address: ChecksummedAddress
    active_hooks: frozenset["Hooks"]
    _token0: Erc20Token
    _token1: Erc20Token
    _pool_key: UniswapV4PoolKey
    name: str
    protocol_fee: ProtocolFee
    lp_fee: int
    _initial_state_block: int
    _sparse_liquidity_map: bool
    _bitmap_override: dict[int, Any]
    _tick_data_fetcher: Any
    _subscribers: WeakSet[Subscriber]

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``UniswapV4Pool`` is a Python companion over a Rust-owned
        ``LiquidityPool`` handle. The handle can only be produced by
        registering a pool in a ``Bot`` — there is no way for a caller to
        hand-build one. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_v4_pool(...)``

        Both register the pool in Rust, obtain the ``LiquidityPool``
        handle, and wrap it via :meth:`_from_py_pool` (mirroring Polars'
        ``_from_pydf`` seam).

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or make_v4_pool(...) "
            "(tests) to register the pool in Rust and obtain the "
            "LiquidityPool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_pool(cls, py_pool: LiquidityPool) -> Self:
        """Wrap a Rust-owned ``LiquidityPool`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). The
        handle is self-describing: every identity field (pool_manager,
        pool_id, pool_key, hooks, tokens, fee, tick_spacing) is read off it
        — no identity is passed as constructor args. Rust owns the mutable
        state (slot0 + tick_data + reorg journal) as ``V4PoolState`` and the
        immutable registration metadata as ``V4PoolIdentity``; this companion
        reads both through ``self._py_pool``.

        Protocol fee / LP fee / state_view_address are builder-supplied
        values the seam defaults; the builder overrides them after
        ``_from_py_pool`` (matches V3's deployer/init_hash override).

        Returns:
            A ``cls`` instance wrapping ``py_pool``.

        Raises:
            DegenbotValueError: If the handle is not a V4-family pool.

        """
        self = cls.__new__(cls)
        self._py_pool = py_pool

        # Variant-family guard.
        if py_pool.pool_family != "v4":
            msg = (
                "LiquidityPool handle is not a V4-family pool "
                f"(got pool_family {py_pool.pool_family!r}); "
                "UniswapV4Pool._from_py_pool requires a handle "
                "registered via register_v4_pool"
            )
            raise DegenbotValueError(message=msg)

        # Identity — all read off the handle (no shadow kwargs).
        self._pool_id = HexBytes(bytes.fromhex(py_pool.pool_id_hex.removeprefix("0x")))
        self._pool_manager_address = get_checksum_address(py_pool.pool_manager_address)
        raw_hook = py_pool.hook_address
        self.hook_address = get_checksum_address(raw_hook) if raw_hook else ZERO_ADDRESS
        self._state_view_address = ZERO_ADDRESS
        self.active_hooks = frozenset(
            hook for hook in Hooks if int(self.hook_address, 16) & hook.value != 0
        )

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

        self._pool_key = UniswapV4PoolKey(
            currency0=self._token0.address,
            currency1=self._token1.address,
            fee=py_pool.fee,
            tick_spacing=py_pool.tick_spacing,
            hooks=self.hook_address,
        )

        # Verify pool ID — the handle's pool_id is authoritative (Rust-stored).
        assert self.pool_id == (
            calculated_id := keccak256(
                encode(
                    types=["address", "address", "uint24", "int24", "address"],
                    args=[
                        self.pool_key.currency0,
                        self.pool_key.currency1,
                        self.pool_key.fee,
                        self.pool_key.tick_spacing,
                        self.pool_key.hooks,
                    ],
                ),
            )
        ), (
            f"Supplied pool ID {self.pool_id.to_0x_hex()} does not match calculated ID {calculated_id.to_0x_hex()}, {self.pool_key=}"  # ruff:ignore[line-too-long]
        )

        self.name = f"{self._token0}-{self._token1} ({self.__class__.__name__}, id={self.pool_id.to_0x_hex()})"  # ruff:ignore[line-too-long]

        # Protocol fee / LP fee / initial state block — builder-supplied values
        # the seam defaults; the builder overrides after _from_py_pool.
        self.protocol_fee = ProtocolFee(zero_for_one=0, one_for_zero=0)
        self.lp_fee = self.pool_key.fee
        self._initial_state_block = self._py_pool.update_block

        self._sparse_liquidity_map = len(self._py_pool.tick_data_snapshot()) == 0

        self._bitmap_override = {}
        self._tick_data_fetcher = None
        self._subscribers = WeakSet()
        return self

    def __eq__(self, other: object) -> bool:
        """Check equality with another object.

        Returns:
            True if the other object is the same pool, False otherwise.

        """
        if isinstance(other, type(self)):
            return self._pool_id == other.pool_id
        return NotImplemented

    def __hash__(self) -> int:
        """Hash.

        Returns:
            The hash of the pool ID.

        """
        return hash(self._pool_id)

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the pool.

        """
        return f"{self.__class__.__name__}(id={self.pool_id.to_0x_hex()}, token0={self._token0}, token1={self._token1}, fee={self.fee}, tick spacing={self.tick_spacing})"  # ruff:ignore[line-too-long]

    def __str__(self) -> str:
        """Return the canonical string representation.

        Returns:
            The pool name string.

        """
        return self.name

    @staticmethod
    def _calculate_swap_fee(
        protocol_fee: int,
        lp_fee: int,
    ) -> Pip:
        """Calculate combined swap fee from protocol + LP fee.

        Returns:
            The combined fee in pips.

        """
        protocol_fee &= 0xFFF
        lp_fee &= 0xFFFFFF
        numerator = protocol_fee * lp_fee
        return (protocol_fee + lp_fee) - (numerator // PIPS_DENOMINATOR)

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> int:
        """Calculate tokens in from tokens out.

        Returns:
            The required input token amount.

        Raises:
            DegenbotValueError: If token_out is not held by this pool.
            HookedPoolResult: If the pool has active hooks that affect the swap.
            IncompleteSwap: If the swap cannot fulfill the full output amount.
            LiquidityPoolError: If the simulated execution reverts.

        """
        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_out not found!")

        zero_for_one = token_out == self._token1

        # ADR-005 slice 4: mainline exact-output swap (no override) delegates to
        # the Rust exact-output fetch+retry seam. The V4 exact-output sign
        # convention (amountSpecified > 0) is handled in the Rust core; a custom
        # limit threads through (the seam honours it). The override path routes
        # to the Rust exact-output override seam
        # (``simulate_exact_output_swap_with_override`` — see below).
        if override_state is None:
            outcome = self._py_pool.simulate_exact_output_swap_with_fetch(
                zero_for_one=zero_for_one,
                amount_out=token_out_quantity,
                block=self.update_block,
            )
            if outcome is None:
                raise LiquidityPoolError(
                    message=(
                        f"Simulated execution could not compute. "
                        f"pool={self.address} zfo={zero_for_one} "
                        f"amount_out={token_out_quantity}"
                    ),
                )
            rust_amount0, rust_amount1 = int(outcome[0]), int(outcome[1])
            # The output side is the requested token_out; the input side is the
            # required token_in (opposing amount). zfo: token1 out / token0 in;
            # ofz: token0 out / token1 in.
            rust_amount_out = rust_amount1 if zero_for_one else rust_amount0
            if rust_amount_out < token_out_quantity:
                rust_amount_in = rust_amount0 if zero_for_one else rust_amount1
                raise IncompleteSwap(
                    amount_in=rust_amount_in,
                    amount_out=rust_amount_out,
                )
            return rust_amount0 if zero_for_one else rust_amount1

        # ADR-005 slice 4: exact-output swap over an override (arbitrage
        # hypothetical) delegates to the Rust fetch-enhanced exact-output
        # override seam. HookedPoolResult / IncompleteSwap stay in Python.
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
            sqrt_price_limit_x96=None,
        )
        if outcome is None:
            raise LiquidityPoolError(
                message=(
                    f"Simulated execution could not compute. "
                    f"pool={self.address} zfo={zero_for_one} "
                    f"amount_out={token_out_quantity} override"
                ),
            )
        rust_amount0, rust_amount1 = int(outcome[0]), int(outcome[1])
        # Exact-output: output side is the requested token_out; input side is
        # the required token_in. ofz (zfo=False): token0 out / token1 in.
        # zfo: token1 out / token0 in.
        amount_out = rust_amount1 if zero_for_one else rust_amount0
        amount_in = rust_amount0 if zero_for_one else rust_amount1

        assert amount_out <= token_out_quantity

        if conflicting_hooks := (
            {
                Hooks.AFTER_SWAP,
                Hooks.AFTER_SWAP_RETURNS_DELTA,
                Hooks.BEFORE_SWAP,
                Hooks.BEFORE_SWAP_RETURNS_DELTA,
            }
            & self.active_hooks
        ):
            raise HookedPoolResult(
                amount_in=amount_in,
                amount_out=amount_out,
                hooks=conflicting_hooks,
            )

        if amount_out < token_out_quantity:
            raise IncompleteSwap(
                amount_in=amount_in,
                amount_out=amount_out,
            )

        return amount_in

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> int:
        """Calculate tokens out from tokens in.

        Returns:
            The expected output token amount.

        Raises:
            DegenbotValueError: If token_in is not held by this pool.
            HookedPoolResult: If the pool has active hooks that affect the swap.
            IncompleteSwap: If the swap cannot fulfill the full input amount.
            LiquidityPoolError: If the simulated execution reverts.

        """
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_in not found!")

        zero_for_one = token_in == self._token0

        # ADR-005 slice 3b: mainline exact-input swap (no override, no custom
        # price limit) delegates to the Rust fetch+retry seam. The sparse-path
        # crossing-swap divergence (ELSE-branch miss check in `v4_simulate_swap`)
        # is fixed — `test_cached_calculations` was RED on seed 2, now GREEN
        # across seeds. The override path routes to the Rust override seam
        # (``simulate_swap_with_override`` — see below); a custom limit threads
        # through both paths (the seam honours it).
        if override_state is None:
            outcome = self._py_pool.simulate_swap_with_fetch(
                zero_for_one=zero_for_one,
                amount_in=token_in_quantity,
                block=self.update_block,
            )
            if outcome is None:
                raise LiquidityPoolError(
                    message=(
                        f"Simulated execution could not compute. "
                        f"pool={self.address} zfo={zero_for_one} "
                        f"amount_in={token_in_quantity}"
                    ),
                )
            rust_amount0, rust_amount1 = int(outcome[0]), int(outcome[1])
            return rust_amount1 if zero_for_one else rust_amount0

        # ADR-005 slice 4: exact-input swap over an override (arbitrage
        # hypothetical) delegates to the Rust fetch-enhanced override seam.
        # HookedPoolResult / IncompleteSwap stay in Python.
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
            sqrt_price_limit_x96=None,
        )
        if outcome is None:
            raise LiquidityPoolError(
                message=(
                    f"Simulated execution could not compute. "
                    f"pool={self.address} zfo={zero_for_one} "
                    f"amount_in={token_in_quantity} override"
                ),
            )
        rust_amount0, rust_amount1 = int(outcome[0]), int(outcome[1])
        # ofz (zfo=False): token1 in / token0 out. zfo: token0 in / token1 out.
        amount_in = rust_amount0 if zero_for_one else rust_amount1
        amount_out = rust_amount1 if zero_for_one else rust_amount0

        assert amount_in <= token_in_quantity

        if conflicting_hooks := (
            {
                Hooks.AFTER_SWAP,
                Hooks.AFTER_SWAP_RETURNS_DELTA,
                Hooks.BEFORE_SWAP,
                Hooks.BEFORE_SWAP_RETURNS_DELTA,
            }
            & self.active_hooks
        ):
            raise HookedPoolResult(
                amount_in=amount_in,
                amount_out=amount_out,
                hooks=conflicting_hooks,
            )

        if amount_in < token_in_quantity:
            raise IncompleteSwap(
                amount_in=amount_in,
                amount_out=amount_out,
            )

        return amount_out

    @property
    def address(self) -> ChecksummedAddress:
        """Address.

        Returns:
            The pool manager address.

        """
        return self._pool_manager_address

    @property
    def liquidity(self) -> int:
        """Liquidity.

        Returns:
            The current active liquidity (from Rust via the handle).

        """
        return self._py_pool.liquidity

    @property
    def pool_id(self) -> HexBytes:
        """Pool id.

        Returns:
            The pool ID bytes.

        """
        return self._pool_id

    @property
    def pool_key(self) -> UniswapV4PoolKey:
        """Pool key.

        Returns:
            The V4 pool key struct.

        """
        return self._pool_key

    @property
    def sqrt_price_x96(self) -> int:
        """Sqrt price x96.

        Returns:
            The current sqrt price as a Q64.96 value (from Rust).

        """
        return self._py_pool.sqrt_price_x96

    @property
    def state(self) -> UniswapV4PoolState:
        """State.

        Returns:
            The current pool state, built from one atomic Rust scalar snapshot
            (``_py_pool.snapshot_v3()`` — V3/V4-generic — ) + the tick-map
            snapshots.

        Raises:
            DegenbotValueError: If the pool is not registered in Rust.

        """
        snap = self._py_pool.snapshot_v3()
        if snap is None:
            msg = "No V4 pool state available (pool not registered in Rust)"
            raise DegenbotValueError(message=msg)
        sqrt_price_x96, liquidity, tick, block = snap
        return self.PoolState.__value__(
            id=self.pool_id,
            address=self._pool_manager_address,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            tick_bitmap=self.tick_bitmap,
            tick_data=self.tick_data,
            block=block,
        )

    @property
    def tick(self) -> int:
        """Tick.

        Returns:
            The current tick (from Rust via the handle).

        """
        return self._py_pool.tick

    @property
    def tick_bitmap(self) -> InitializedTickMap:
        """Tick bitmap.

        Returns a deep-copy snapshot of the Rust-side tick bitmap (built from
        ``tick_data`` keys — Rust derives the bitmap, no separate store) MERGED
        with the companion's verbatim ``_bitmap_override``. Mirrors the V3
        companion.
        """
        raw = self._py_pool.tick_bitmap_snapshot()
        result: dict[int, BitmapAtWord] = {
            int(word): (
                BitmapAtWord(bitmap=int(row[0]), block=int(row[1]))
                if not isinstance(row, BitmapAtWord)
                else row
            )
            for word, row in raw.items()
        }
        for word, bitmap_at_word in self._bitmap_override.items():
            result[word] = bitmap_at_word  # ruff:ignore[manual-dict-comprehension]
        return result

    @property
    def tick_data(self) -> LiquidityMap:
        """Tick data.

        Returns a deep-copy snapshot of the Rust-side tick data (mirrors V3).
        """
        raw = self._py_pool.tick_data_snapshot()
        return {
            int(tick): (
                LiquidityAtTick(
                    liquidity_net=int(row[1]),
                    liquidity_gross=int(row[0]),
                    block=int(row[2]),
                )
                if not isinstance(row, LiquidityAtTick)
                else row
            )
            for tick, row in raw.items()
        }

    @property
    def tick_spacing(self) -> int:
        """Tick spacing.

        Returns:
            The tick spacing for the pool (Python-side identity).

        """
        return self.pool_key.tick_spacing

    @property
    def fee(self) -> int:
        """Fee.

        Returns:
            The fee in pips (Python-side identity).

        """
        return self.pool_key.fee

    @property
    def update_block(self) -> BlockNumber:
        """Update block.

        Returns:
            The block number of the most recent state update (from Rust).

        """
        return self._py_pool.update_block

    @property
    def initial_state_block(self) -> int:
        """Block number at which the pool's initial state was captured.

        Returns:
            The block number from construction (DB snapshot or RPC fetch).

        """
        return self._initial_state_block

    def swap_is_viable(  # ruff:ignore[no-self-use]
        self,
        state: UniswapV4PoolState,
        vector: UniswapPoolSwapVector,  # ruff:ignore[unused-method-argument]
    ) -> bool:
        """Swap is viable.

        Returns:
            True if a swap can proceed with the given state, False otherwise.

        """
        if state.liquidity == 0:
            return False
        return state.sqrt_price_x96 > 1

    def update_tick_data(
        self,
        tick_bitmap: dict[int, Any],
        tick_data: dict[int, Any],
        block: int,
    ) -> None:
        """Apply updated tick bitmap and data from the tick data fetcher.

        Delegates to ``LiquidityPool.update_tick_data`` (replaces the
        Rust-side ``tick_data`` HashMap; scalars unchanged; ``update_block``
        advances when newer). Records every word in ``tick_bitmap`` into
        ``_bitmap_override``. Mirrors the V3 companion.
        """
        normalized: dict[int, tuple[int, int, int]] = {}
        for tick, info in tick_data.items():
            if isinstance(info, LiquidityAtTick):
                normalized[int(tick)] = (
                    int(info.liquidity_gross),
                    int(info.liquidity_net),
                    int(info.block),
                )
            elif isinstance(info, dict):
                normalized[int(tick)] = (
                    int(info["liquidity_gross"]),
                    int(info["liquidity_net"]),
                    int(info.get("block", 0)),
                )
            else:
                normalized[int(tick)] = (
                    int(info[0]),
                    int(info[1]),
                    int(info[2]) if len(info) > 2 else block,  # ruff:ignore[magic-value-comparison]
                )
        self._py_pool.update_tick_data(tick_bitmap, normalized, block)
        for word, bitmap_at_word in tick_bitmap.items():
            if isinstance(bitmap_at_word, BitmapAtWord):
                self._bitmap_override[int(word)] = bitmap_at_word
            elif isinstance(bitmap_at_word, dict):
                self._bitmap_override[int(word)] = BitmapAtWord(**bitmap_at_word)
            else:
                self._bitmap_override[int(word)] = bitmap_at_word
        self._notify_subscribers(message=UniswapV4PoolStateUpdated(self.state))
        # NOTE: ``_sparse_liquidity_map`` NOT flipped — fetcher's incremental
        # backfill must keep the pool sparse. See V3 companion for rationale.

    def _apply_fetched_tick_word(self, word: int, block: BlockNumber) -> bool:
        """Fetch a tick-bitmap word via ``_tick_data_fetcher`` and merge it.

        ADR-005 slice 3b: the fetcher RETURNS the word's ticks (return-data
        contract — the Rust ``simulate_swap_with_fetch`` seam uses the same
        callback). Merge them into the pool state via ``update_tick_data``;
        the bitmap is derived from the tick_data keys (V3/V4 invariant), so no
        verbatim bitmap value is needed.

        Returns:
            ``True`` if the fetch succeeded, ``False`` if no fetcher or the
            fetch returned ``None``.

        """
        if self._tick_data_fetcher is None:
            return False
        fetched = self._tick_data_fetcher(word, block)
        if fetched is None:
            return False
        merged = {**self.tick_data, **fetched}
        self.update_tick_data(
            tick_bitmap={word: BitmapAtWord(bitmap=0, block=block)},
            tick_data=merged,
            block=block,
        )
        return True

    def external_update(
        self,
        update: UniswapV4PoolExternalUpdate,
    ) -> bool:
        """Process a `UniswapV4PoolExternalUpdate` (Swap event).

        Delegates the scalar write to ``LiquidityPool.apply_swap``. Mirrors
        the V3 companion.

        Returns:
            True if any updated state value was recorded, False otherwise.

        Raises:
            ExternalUpdateError: If the update is for an invalid block.

        """
        if (
            update.block_number <= self._initial_state_block
            or update.block_number < self.update_block
        ):
            raise ExternalUpdateError(message=f"Rejected update for block {update.block_number}")

        if (
            update.liquidity == self.liquidity
            and update.sqrt_price_x96 == self.sqrt_price_x96
            and update.tick == self.tick
        ):
            return False

        self._py_pool.apply_swap(
            sqrt_price_x96=update.sqrt_price_x96,
            liquidity=update.liquidity,
            tick=update.tick,
            block_number=update.block_number,
        )
        self._notify_subscribers(message=UniswapV4PoolStateUpdated(self.state))
        return True

    def update_liquidity_map(
        self,
        update: UniswapV4PoolLiquidityMappingUpdate,
    ) -> None:
        """Apply an update to the liquidity map (ModifyLiquidity event).

        Delegates the tick mutation to ``LiquidityPool.apply_liquidity_update``
        (Rust does the tick bitmap + tick_data mutation under one write guard).
        The active ``liquidity`` scalar adjustment (when ``current_tick`` is in
        range) is landed via a separate ``apply_swap``. Mirrors the V3 companion.
        """
        if update.liquidity == 0:
            return

        state_block = update.block_number

        if self._sparse_liquidity_map and self._tick_data_fetcher is not None:
            for tick in (update.tick_lower, update.tick_upper):
                word, _ = cl_get_tick_word_and_bit_position(tick, self.tick_spacing)
                if word not in self.tick_bitmap:
                    self._apply_fetched_tick_word(word, state_block - 1)

        applied = self._py_pool.apply_liquidity_update(
            tick_lower=update.tick_lower,
            tick_upper=update.tick_upper,
            liquidity_delta=update.liquidity,
            block_number=state_block,
        )

        if (
            applied
            and update.tick_lower <= self.tick < update.tick_upper
            and state_block > self._initial_state_block
        ):
            new_active = self.liquidity + update.liquidity
            assert new_active >= 0, (
                f"In-range liquidity adjustment violated invariant: pool {self.address} "
                f"{self.tick=} {self.liquidity=} {self.update_block=} {update=}"
            )
            self._py_pool.apply_swap(
                sqrt_price_x96=self.sqrt_price_x96,
                liquidity=new_active,
                tick=self.tick,
                block_number=state_block,
            )

        self._notify_subscribers(message=UniswapV4PoolStateUpdated(self.state))

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """Discard cached states earlier than the given block.

        Raises:
            NoPoolStateAvailable: If the target is past the newest delta.

        """
        try:
            self._py_pool.discard_v3_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e

    def restore_state_before_block(self, block: BlockNumber) -> None:
        """Restore the last pool state recorded prior to a target block.

        Delegates to ``LiquidityPool.restore_v3_before_block`` (V3/V4-generic).
        Mirrors the V3 companion.

        Raises:
            NoPoolStateAvailable: If no state exists prior to the target block.

        """
        try:
            restored = self._py_pool.restore_v3_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e
        if restored is not None:
            self._notify_subscribers(message=UniswapV4PoolStateUpdated(self.state))
