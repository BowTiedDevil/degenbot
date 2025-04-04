import dataclasses
from collections.abc import Sequence
from enum import Enum
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Any, Final, NewType, cast
from weakref import WeakSet

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import BlockNumber, ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import ContractLogicError
from web3.types import BlockIdentifier

from degenbot.cache import get_checksum_address
from degenbot.config import connection_manager
from degenbot.constants import MAX_INT256, MIN_INT256, ZERO_ADDRESS
from degenbot.erc20_token import Erc20Token
from degenbot.exceptions import (
    DegenbotValueError,
    EVMRevertError,
    ExternalUpdateError,
    IncompleteSwap,
    LateUpdateError,
    LiquidityMapWordMissing,
    LiquidityPoolError,
    PossibleInaccurateResult,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.managers.erc20_token_manager import Erc20TokenManager
from degenbot.registry.all_pools import pool_registry
from degenbot.types import (
    AbstractArbitrage,
    AbstractLiquidityPool,
    BoundedCache,
    Message,
    Publisher,
    PublisherMixin,
    Subscriber,
)
from degenbot.uniswap.types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4PoolExternalUpdate,
    UniswapV4PoolKey,
    UniswapV4PoolLiquidityMappingUpdate,
    UniswapV4PoolState,
    UniswapV4PoolStateUpdated,
)
from degenbot.uniswap.v3_functions import (
    exchange_rate_from_sqrt_price_x96,
    get_tick_word_and_bit_position,
)
from degenbot.uniswap.v4_libraries.swap_math import (
    MAX_SWAP_FEE,
    compute_swap_step,
    get_sqrt_price_target,
)
from degenbot.uniswap.v4_libraries.tick_bitmap import (
    flip_tick,
    gen_ticks,
    next_initialized_tick_within_one_word,
)
from degenbot.uniswap.v4_libraries.tick_math import (
    MAX_SQRT_PRICE,
    MAX_TICK,
    MIN_SQRT_PRICE,
    MIN_TICK,
    get_sqrt_price_at_tick,
    get_tick_at_sqrt_price,
)


@dataclasses.dataclass(slots=True)
class SwapResult:
    sqrt_price_x96: int
    tick: int
    liquidity: int


@dataclasses.dataclass(slots=True, frozen=True)
class SwapDelta:
    currency0: int
    currency1: int

    @property
    def amount_in(self) -> int:
        "The deposited token amount."
        return -min(self.currency0, self.currency1)

    @property
    def amount_out(self) -> int:
        "The withdrawn token amount."
        return max(self.currency0, self.currency1)


@dataclasses.dataclass(slots=True, frozen=True)
class ProtocolFee:
    zero_for_one: int
    one_for_zero: int


@dataclasses.dataclass(slots=True, frozen=True)
class Slot0:
    sqrt_price_x96: int
    tick: int
    protocol_fee: ProtocolFee
    lp_fee: int


FeeToProtocol = NewType("FeeToProtocol", int)
SwapFee = NewType("SwapFee", int)
Liquidity = NewType("Liquidity", int)


PIPS_DENOMINATOR = 1_000_000
NATIVE_CURRENCY_ADDRESS = ZERO_ADDRESS


class Hooks(Enum):
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


class UniswapV4Pool(PublisherMixin, AbstractLiquidityPool):
    _state_cache: BoundedCache[BlockNumber, UniswapV4PoolState]

    SLOT0_STRUCT_TYPES = (
        "uint160",  # sqrtPriceX96
        "int24",  # tick
        "uint24",  # protocolFee
        "uint24",  # lpFee
    )
    TICK_LIQUIDITY_STRUCT_TYPES = (
        "uint128",  # liquidityGross
        "int128",  # liquidityNet
    )

    FEE_DENOMINATOR = 1_000_000

    @dataclasses.dataclass(slots=True)
    class SwapCache:
        liquidity_start: int
        tick_cumulative: int

    @dataclasses.dataclass(slots=True)
    class SwapState:
        amount_specified_remaining: int
        amount_calculated: int
        sqrt_price_x96: int
        tick: int
        liquidity: int

    @dataclasses.dataclass(slots=True)
    class StepComputations:
        sqrt_price_start_x96: int = 0
        tick_next: int = 0
        initialized: bool = False
        sqrt_price_next_x96: int = 0
        amount_in: int = 0
        amount_out: int = 0
        fee_amount: int = 0
        fee_growth_global_x128: int | None = None  # unused

    def __init__(
        self,
        *,
        pool_id: str,
        pool_manager_address: str,
        state_view_address: str,
        tokens: Sequence[str],
        fee: int,
        tick_spacing: int,
        hook_address: str | None = None,
        chain_id: int | None = None,
        tick_data: dict[int, dict[str, Any] | UniswapV4LiquidityAtTick] | None = None,
        tick_bitmap: dict[int, dict[str, Any] | UniswapV4BitmapAtWord] | None = None,
        state_block: BlockNumber | int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ):
        self._chain_id: Final[int] = (
            chain_id if chain_id is not None else connection_manager.default_chain_id
        )
        w3 = connection_manager.get_web3(self.chain_id)
        state_block = (
            cast("BlockNumber", state_block) if state_block is not None else w3.eth.block_number
        )
        self._initial_state_block = state_block
        self._state_view_address = get_checksum_address(state_view_address)

        assert len(tokens) == 2  # noqa: PLR2004
        tokens = [token.lower() for token in tokens]
        token_manager = Erc20TokenManager(chain_id=self.chain_id)
        self.token0: Final[Erc20Token] = token_manager.get_erc20token(
            address=min(tokens),
            silent=silent,
        )
        self.token1: Final[Erc20Token] = token_manager.get_erc20token(
            address=max(tokens),
            silent=silent,
        )

        self.hook_address = (
            get_checksum_address(hook_address) if hook_address is not None else ZERO_ADDRESS
        )

        self.active_hooks: set[Hooks] = {
            hook for hook in Hooks if int(self.hook_address, 16) & hook.value != 0
        }

        # Construct the PoolKey
        self._pool_key: Final[UniswapV4PoolKey] = UniswapV4PoolKey(
            currency0=self.token0.address,
            currency1=self.token1.address,
            fee=fee,
            tick_spacing=tick_spacing,
            hooks=self.hook_address,
        )

        self._pool_manager_address = get_checksum_address(pool_manager_address)
        self._pool_id: Final[HexBytes] = HexBytes(pool_id)
        self.name = (
            f"{self.token0}-{self.token1} ({type(self).__name__}, id={self.pool_id.to_0x_hex()})"
        )

        try:
            _slot0, _liquidity = self._get_state_values(w3=w3, state_block=state_block)
            _sqrt_price_x96 = _slot0.sqrt_price_x96
            _tick = _slot0.tick
            self.lp_fee = _slot0.lp_fee
            self.protocol_fee = _slot0.protocol_fee
        except (ContractLogicError, DecodingError) as exc:
            # Contracts differ slightly across Uniswap V3 forks, so decoding may fail. Catch this
            # here and raise as a pool-specific exception
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        assert self.pool_id == (
            calculated_id := Web3.keccak(
                eth_abi.abi.encode(
                    types=["address", "address", "uint24", "int24", "address"],
                    args=[
                        self.pool_key.currency0,
                        self.pool_key.currency1,
                        self.pool_key.fee,
                        self.pool_key.tick_spacing,
                        self.pool_key.hooks,
                    ],
                )
            )
        ), (
            f"Supplied pool ID {self.pool_id.to_0x_hex()} does not match calculated ID {calculated_id.to_0x_hex()}, {self.pool_key=}"  # noqa
        )

        # If liquidity info was not provided, treat the mapping as sparse
        self.sparse_liquidity_map = tick_bitmap is None or tick_data is None

        _tick_bitmap = {}
        _tick_data = {}

        if tick_bitmap is not None:
            # transform dict to UniswapV4BitmapAtWord
            _tick_bitmap.update(
                {
                    int(word): (
                        UniswapV4BitmapAtWord(**bitmap_at_word)
                        if not isinstance(
                            bitmap_at_word,
                            UniswapV4BitmapAtWord,
                        )
                        else bitmap_at_word
                    )
                    for word, bitmap_at_word in tick_bitmap.items()
                }
            )

        if tick_data is not None:
            _tick_data.update(
                {
                    int(tick): (
                        # transform dict to UniswapV4LiquidityAtTick
                        UniswapV4LiquidityAtTick(**liquidity_at_tick)
                        if not isinstance(
                            liquidity_at_tick,
                            UniswapV4LiquidityAtTick,
                        )
                        else liquidity_at_tick
                    )
                    for tick, liquidity_at_tick in tick_data.items()
                }
            )

        if tick_bitmap is None and tick_data is None:
            word, _ = get_tick_word_and_bit_position(tick=_tick, tick_spacing=self.tick_spacing)
            self._fetch_and_populate_initialized_ticks(
                word_position=word,
                tick_bitmap=_tick_bitmap,
                tick_data=_tick_data,
                block_number=state_block,
            )

        self._state = UniswapV4PoolState(
            id=self.pool_id,
            address=self._pool_manager_address,
            liquidity=_liquidity,
            sqrt_price_x96=_sqrt_price_x96,
            tick=_tick,
            tick_bitmap=_tick_bitmap,
            tick_data=_tick_data,
            block=state_block,
        )
        self._state_cache = BoundedCache(max_items=state_cache_depth)
        self._state_cache[self.update_block] = self.state
        self._state_lock = Lock()

        pool_registry.add(
            pool=self,
            chain_id=self.chain_id,
            pool_address=self.address,
            pool_id=self.pool_id,
        )

        self._subscribers: WeakSet[Subscriber] = WeakSet()

        if not silent:  # pragma: no branch
            logger.info(self.name)
            logger.info(f"• ID: {self.pool_id.to_0x_hex()}")
            logger.info(f"• Token 0: {self.token0}")
            logger.info(f"• Token 1: {self.token1}")
            logger.info(f"• Liquidity: {self.liquidity}")
            logger.info(f"• SqrtPrice: {self.sqrt_price_x96}")
            logger.info(f"• Tick: {self.tick}")

    def __eq__(self, other: object) -> bool:
        if isinstance(other, type(self)):
            return self.address == other.address and self.pool_id == other.pool_id
        return super().__eq__(other)

    def __hash__(self) -> int:
        return hash(HexBytes(self.address) + self.pool_id)

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that cannot be pickled and are unnecessary to perform
        # the calculation
        copied_attributes = ()

        dropped_attributes = (
            "_contract",
            "_state_cache",
            "_state_lock",
            "_subscribers",
            "deployer_address",
            "factory",
            "init_hash",
        )

        with self._state_lock:
            return {
                k: (v.copy() if k in copied_attributes else v)
                for k, v in self.__dict__.items()
                if k not in dropped_attributes
            }

    def __repr__(self) -> str:  # pragma: no cover
        return f"{type(self).__name__}(pool_id={self.pool_id.to_0x_hex()},  token0={self.token0}, token1={self.token1}, fee={self.fee}, tick spacing={self.tick_spacing})"  # noqa:E501

    def __str__(self) -> str:
        return self.name

    def _fetch_and_populate_initialized_ticks(
        self,
        word_position: int,
        tick_bitmap: dict[int, UniswapV4BitmapAtWord],
        tick_data: dict[int, UniswapV4LiquidityAtTick],
        block_number: int | None = None,
    ) -> None:
        """
        Update the supplied tick bitmap with initialized tick values within a specified word
        position. A word is divided into 256 ticks, spaced at a fixed interval.
        """

        w3 = connection_manager.get_web3(self.chain_id)

        if block_number is None:
            block_number = w3.eth.get_block_number()

        _tick_bitmap: int = 0
        _tick_data: list[tuple[int, int, int]] = []
        _tick_bitmap = self.get_tick_bitmap_at_word(
            w3=w3,
            word_position=word_position,
            block_identifier=block_number,
        )
        if _tick_bitmap != 0:
            _tick_data = self.get_populated_ticks_in_word(
                w3=w3,
                word_position=word_position,
                block_identifier=block_number,
            )

        tick_bitmap[word_position] = UniswapV4BitmapAtWord(
            bitmap=_tick_bitmap,
            block=block_number,
        )
        for tick, liquidity_gross, liquidity_net in _tick_data:
            tick_data[tick] = UniswapV4LiquidityAtTick(
                liquidity_net=liquidity_net,
                liquidity_gross=liquidity_gross,
                block=block_number,
            )

    def _get_state_values(
        self,
        w3: Web3,
        state_block: BlockNumber,
    ) -> tuple[Slot0, Liquidity]:
        with w3.batch_requests() as batch:
            batch.add(
                # This call uses a specific block so the mutable state values are consistent
                w3.eth.call(
                    transaction={
                        "to": self._state_view_address,
                        "data": encode_function_calldata(
                            function_prototype="getSlot0(bytes32)",
                            function_arguments=[self.pool_id],
                        ),
                    },
                    block_identifier=state_block,
                )
            )
            batch.add(
                # This call uses a specific block so the mutable state values are consistent
                w3.eth.call(
                    transaction={
                        "to": self._state_view_address,
                        "data": encode_function_calldata(
                            function_prototype="getLiquidity(bytes32)",
                            function_arguments=[self.pool_id],
                        ),
                    },
                    block_identifier=state_block,
                )
            )

            slot0, liquidity = batch.execute()

        protocol_fee: int
        price, tick, protocol_fee, lp_fee = eth_abi.abi.decode(
            types=self.SLOT0_STRUCT_TYPES, data=cast("HexBytes", slot0)
        )
        (liquidity,) = eth_abi.abi.decode(types=["uint256"], data=cast("HexBytes", liquidity))

        # Extract the two fees from the packed protocol fee
        # ref: https://github.com/Uniswap/v4-core/blob/main/src/types/Slot0.sol
        protocol_fee_as_bytes = (protocol_fee).to_bytes(length=6, byteorder="big")
        protocol_fee_one_to_zero = int.from_bytes(protocol_fee_as_bytes[:3], byteorder="big")
        protocol_fee_zero_to_one = int.from_bytes(protocol_fee_as_bytes[3:6], byteorder="big")

        return (
            Slot0(
                sqrt_price_x96=cast("int", price),
                tick=cast("int", tick),
                protocol_fee=ProtocolFee(
                    one_for_zero=protocol_fee_one_to_zero,
                    zero_for_one=protocol_fee_zero_to_one,
                ),
                lp_fee=cast("int", lp_fee),
            ),
            Liquidity(cast("int", liquidity)),
        )

    def _calculate_swap_fee(
        self,
        protocol_fee: int,
        lp_fee: int,
    ) -> SwapFee:
        protocol_fee &= 0xFFF
        lp_fee &= 0xFFFFFF
        numerator = protocol_fee * lp_fee
        return SwapFee(
            (protocol_fee + lp_fee) - (numerator // PIPS_DENOMINATOR),
        )

    def _calculate_swap(
        self,
        zero_for_one: bool,
        amount_specified: int,
        sqrt_price_x96_limit: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> tuple[SwapDelta, FeeToProtocol, SwapFee, SwapResult]:
        """
        This function is ported and adapted from the swap() function implemented by the Pool.sol
        library contract.

        ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/Pool.sol

        Returns a tuple with amounts and final pool state values for a successful swap:
        (amount0, amount1, sqrt_price_x96, liquidity, tick)

        A positive amount indicates the quantity available for withdrawal by the swapper,
        and a negative amount indicates the deposit required.

        This method will fetch missing liquidity data as needed, but it will be discarded to avoid
        race conditions.
        """

        if override_state is not None:
            liquidity_start = override_state.liquidity
            sqrt_price_x96_start = override_state.sqrt_price_x96
            tick_start = override_state.tick
            tick_bitmap_temp = override_state.tick_bitmap
            tick_data_temp = override_state.tick_data
        else:
            liquidity_start = self.liquidity
            sqrt_price_x96_start = self.sqrt_price_x96
            tick_start = self.tick
            tick_bitmap_temp = self.tick_bitmap
            tick_data_temp = self.tick_data

        protocol_fee = (
            self.protocol_fee.zero_for_one if zero_for_one else self.protocol_fee.one_for_zero
        )

        assert liquidity_start >= 0

        amount_specified_remaining = amount_specified
        amount_calculated = 0
        result: Final[SwapResult] = SwapResult(
            sqrt_price_x96=sqrt_price_x96_start,
            tick=tick_start,
            liquidity=liquidity_start,
        )

        lp_fee = self.lp_fee
        swap_fee = lp_fee if protocol_fee == 0 else self._calculate_swap_fee(protocol_fee, lp_fee)

        # a swap fee totaling MAX_SWAP_FEE (100%) makes exact output swaps impossible since the
        # input is entirely consumed by the fee
        if swap_fee >= MAX_SWAP_FEE and amount_specified > 0:  # exact output
            raise EVMRevertError(error="InvalidFeeForExactOut")

        # swapFee is the pool's fee in pips (LP fee + protocol fee)
        # when the amount swapped is 0, there is no protocolFee applied and the fee amount paid to
        # the protocol is set to 0
        if amount_specified == 0:
            return (
                SwapDelta(currency0=0, currency1=0),
                cast("FeeToProtocol", 0),
                cast("SwapFee", swap_fee),
                result,
            )

        if zero_for_one:
            if sqrt_price_x96_limit >= sqrt_price_x96_start:
                raise EVMRevertError(error="PriceLimitAlreadyExceeded")
            # Swaps can never occur at MIN_TICK, only at MIN_TICK + 1, except at initialization of
            # a pool. Under certain circumstances outlined below, the tick will preemptively reach
            # MIN_TICK without swapping there
            if sqrt_price_x96_limit <= MIN_SQRT_PRICE:
                raise EVMRevertError(error="PriceLimitOutOfBounds")
        else:
            if sqrt_price_x96_limit <= sqrt_price_x96_start:
                raise EVMRevertError(error="PriceLimitAlreadyExceeded")
            if sqrt_price_x96_limit >= MAX_SQRT_PRICE:
                raise EVMRevertError(error="PriceLimitOutOfBounds")

        step = self.StepComputations()

        if not self.sparse_liquidity_map:
            # The liquidity mapping is complete. Optimize loop by building a generator that yields
            # ticks and initialization status along the swap path
            ticks_along_swap_path = gen_ticks(
                tick_data_temp, tick_start, self.tick_spacing, zero_for_one
            )

        while not (
            amount_specified_remaining == 0 or result.sqrt_price_x96 == sqrt_price_x96_limit
        ):
            step.sqrt_price_start_x96 = result.sqrt_price_x96

            if not self.sparse_liquidity_map:
                step.tick_next, step.initialized = next(ticks_along_swap_path)
            else:
                try:
                    step.tick_next, step.initialized = next_initialized_tick_within_one_word(
                        tick_bitmap_temp,
                        tick_data_temp,
                        result.tick,
                        self.tick_spacing,
                        zero_for_one,
                    )
                except LiquidityMapWordMissing as exc:
                    missing_word = exc.word
                    self._fetch_and_populate_initialized_ticks(
                        word_position=missing_word,
                        tick_bitmap=tick_bitmap_temp,
                        tick_data=tick_data_temp,
                        block_number=self.update_block,
                    )
                    continue

            # Ensure that we do not overshoot the min/max tick, as the tick bitmap is not aware of
            # these bounds
            step.tick_next = (
                max(MIN_TICK, step.tick_next)  # descending ticks
                if zero_for_one
                else min(MAX_TICK, step.tick_next)  # ascending ticks
            )
            step.sqrt_price_next_x96 = get_sqrt_price_at_tick(step.tick_next)

            # compute values to swap to the target tick, price limit, or point where input/output
            # amount is exhausted
            result.sqrt_price_x96, step.amount_in, step.amount_out, step.fee_amount = (
                compute_swap_step(
                    sqrt_ratio_x96_current=result.sqrt_price_x96,
                    sqrt_ratio_x96_target=get_sqrt_price_target(
                        zero_for_one=zero_for_one,
                        sqrt_price_next_x96=step.sqrt_price_next_x96,
                        sqrt_price_limit_x96=sqrt_price_x96_limit,
                    ),
                    liquidity=result.liquidity,
                    amount_remaining=amount_specified_remaining,
                    fee_pips=swap_fee,
                )
            )

            if amount_specified > 0:  # exact output
                if not (MIN_INT256 <= step.amount_out <= MAX_INT256):
                    raise EVMRevertError(error="SafeCastOverflow")
                if not (MIN_INT256 <= step.amount_in + step.fee_amount <= MAX_INT256):
                    raise EVMRevertError(error="SafeCastOverflow")

                amount_specified_remaining -= step.amount_out
                amount_calculated -= step.amount_in + step.fee_amount
            else:  # exact input
                if not (MIN_INT256 <= step.amount_in + step.fee_amount <= MAX_INT256):
                    raise EVMRevertError(error="SafeCastOverflow")
                if not (MIN_INT256 <= step.amount_out <= MAX_INT256):
                    raise EVMRevertError(error="SafeCastOverflow")

                amount_specified_remaining += step.amount_in + step.fee_amount
                amount_calculated += step.amount_out

            if protocol_fee > 0:
                # step.amountIn does not include the swap fee, as it's already been taken from it,
                # so add it back to get the total amountIn and use that to calculate the amount of
                # fees owed to the protocol cannot overflow due to limits on the size of protocolFee
                # and params.amountSpecified.
                # This rounds down to favor LPs over the protocol
                delta = (
                    step.fee_amount  # lp fee is 0, so the entire fee is owed to the protocol
                    if (swap_fee == protocol_fee)
                    else (step.amount_in + step.fee_amount) * protocol_fee // PIPS_DENOMINATOR
                )
                # subtract it from the total fee and add it to the protocol fee
                step.fee_amount -= delta

            # Shift tick if we reached the next price, and preemptively decrement for zeroForOne
            # swaps to tickNext - 1. If the swap doesn't continue (if amountRemaining == 0 or
            # sqrtPriceLimit is met), slot0.tick will be 1 less than
            # getTickAtSqrtPrice(slot0.sqrtPrice). This doesn't affect swaps, but donation calls
            # should verify both price and tick to reward the correct LPs.
            if result.sqrt_price_x96 == step.sqrt_price_next_x96:
                # If the tick is initialized, adjust the liquidity range
                if step.initialized:
                    liquidity_net_at_next_tick = tick_data_temp[step.tick_next].liquidity_net
                    result.liquidity += (
                        -liquidity_net_at_next_tick if zero_for_one else liquidity_net_at_next_tick
                    )
                result.tick = step.tick_next - 1 if zero_for_one else step.tick_next
            elif result.sqrt_price_x96 != step.sqrt_price_start_x96:
                # Recompute unless we're on a lower tick boundary (i.e. already transitioned ticks),
                # and haven't moved
                result.tick = get_tick_at_sqrt_price(result.sqrt_price_x96)

            assert result.liquidity >= 0

        if zero_for_one != (amount_specified < 0):
            # currency1 is swapped in
            swap_delta = SwapDelta(
                currency0=amount_calculated,
                currency1=amount_specified - amount_specified_remaining,
            )
        else:
            swap_delta = SwapDelta(
                currency0=amount_specified - amount_specified_remaining,
                currency1=amount_calculated,
            )

        return (
            swap_delta,
            cast("FeeToProtocol", protocol_fee),
            cast("SwapFee", swap_fee),
            result,
        )

    def _notify_subscribers(self: Publisher, message: Message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> int:
        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_out not found!")

        zero_for_one = token_out == self.token1

        try:
            swap_delta, *_ = self._calculate_swap(
                zero_for_one=zero_for_one,
                amount_specified=token_out_quantity,
                sqrt_price_x96_limit=MIN_SQRT_PRICE + 1 if zero_for_one else MAX_SQRT_PRICE - 1,
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e

        assert swap_delta.amount_out <= token_out_quantity

        if conflicting_hooks := (
            self.active_hooks
            & {
                Hooks.AFTER_SWAP,
                Hooks.AFTER_SWAP_RETURNS_DELTA,
                Hooks.BEFORE_SWAP,
                Hooks.BEFORE_SWAP_RETURNS_DELTA,
            }
        ):
            raise PossibleInaccurateResult(
                amount_in=swap_delta.amount_in,
                amount_out=swap_delta.amount_out,
                hooks=conflicting_hooks,
            )

        if swap_delta.amount_out < token_out_quantity:
            raise IncompleteSwap(
                amount_in=swap_delta.amount_in,
                amount_out=swap_delta.amount_out,
            )

        return swap_delta.amount_in

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> int:
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_in not found!")

        zero_for_one = token_in == self.token0

        try:
            swap_delta, *_ = self._calculate_swap(
                zero_for_one=zero_for_one,
                amount_specified=-token_in_quantity,
                sqrt_price_x96_limit=MIN_SQRT_PRICE + 1 if zero_for_one else MAX_SQRT_PRICE - 1,
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e

        assert swap_delta.amount_in <= token_in_quantity

        if conflicting_hooks := (
            self.active_hooks
            & {
                Hooks.AFTER_SWAP,
                Hooks.AFTER_SWAP_RETURNS_DELTA,
                Hooks.BEFORE_SWAP,
                Hooks.BEFORE_SWAP_RETURNS_DELTA,
            }
        ):
            raise PossibleInaccurateResult(
                amount_in=swap_delta.amount_in,
                amount_out=swap_delta.amount_out,
                hooks=conflicting_hooks,
            )

        if swap_delta.amount_in < token_in_quantity:
            raise IncompleteSwap(
                amount_in=swap_delta.amount_in,
                amount_out=swap_delta.amount_out,
            )

        return swap_delta.amount_out

    def get_tick_bitmap_at_word(
        self, w3: Web3, word_position: int, block_identifier: BlockIdentifier
    ) -> int:
        (bitmap_at_word,) = raw_call(
            w3=w3,
            address=self._state_view_address,
            calldata=encode_function_calldata(
                function_prototype="getTickBitmap(bytes32,int16)",
                function_arguments=[self.pool_id, word_position],
            ),
            return_types=["uint256"],
            block_identifier=block_identifier,
        )
        return cast("int", bitmap_at_word)

    def get_populated_ticks_in_word(
        self,
        w3: Web3,
        word_position: int,
        block_identifier: BlockIdentifier,
    ) -> list[tuple[int, int, int]]:
        bitmap_at_word = self.get_tick_bitmap_at_word(
            w3=w3, word_position=word_position, block_identifier=block_identifier
        )

        active_ticks = [
            ((word_position << 8) + i) * self.tick_spacing
            for i in range(256)
            if bitmap_at_word & (1 << i) > 0
        ]

        with w3.batch_requests() as batch:
            for tick in active_ticks:
                batch.add(
                    w3.eth.call(
                        transaction={
                            "to": self._state_view_address,
                            "data": encode_function_calldata(
                                function_prototype="getTickLiquidity(bytes32,int24)",
                                function_arguments=[self.pool_id, tick],
                            ),
                        },
                        block_identifier=block_identifier,
                    )
                )
            results = batch.execute()

        populated_ticks = []
        for tick, result in zip(active_ticks, results, strict=True):
            liquidity_gross, liquidity_net = eth_abi.abi.decode(
                types=self.TICK_LIQUIDITY_STRUCT_TYPES,
                data=cast("HexBytes", result),
            )
            populated_ticks.append((tick, liquidity_gross, liquidity_net))

        return populated_ticks

    @property
    def address(self) -> ChecksumAddress:  # type: ignore[override]
        return self._pool_manager_address

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def liquidity(self) -> int:
        return self.state.liquidity

    @property
    def pool_id(self) -> HexBytes:
        return self._pool_id

    @property
    def pool_key(self) -> UniswapV4PoolKey:
        return self._pool_key

    @property
    def sqrt_price_x96(self) -> int:
        return self.state.sqrt_price_x96

    @property
    def state(self) -> UniswapV4PoolState:
        return self._state

    @property
    def tick(self) -> int:
        return self.state.tick

    @property
    def tick_bitmap(self) -> dict[int, UniswapV4BitmapAtWord]:
        return self.state.tick_bitmap.copy()

    @property
    def tick_data(self) -> dict[int, UniswapV4LiquidityAtTick]:
        return self.state.tick_data.copy()

    @property
    def tick_spacing(self) -> int:
        return self.pool_key.tick_spacing

    @property
    def fee(self) -> int:
        return self.pool_key.fee

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        return self.token0, self.token1

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

    def auto_update(
        self,
        block_number: int | None = None,
        silent: bool = True,
    ) -> None:
        """
        Retrieves and records the current slot0 and liquidity state from the pool at the provided
        block number, or the latest block if not provided.

        @dev this method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """

        with self._state_lock:
            if block_number is not None and block_number < self.update_block:
                raise LateUpdateError

            state_updated = False

            w3 = connection_manager.get_web3(self.chain_id)
            block_number = (
                cast("BlockNumber", block_number)
                if block_number is not None
                else w3.eth.get_block_number()
            )

            _slot0, _liquidity = self._get_state_values(w3=w3, state_block=block_number)
            _sqrt_price_x96 = _slot0.sqrt_price_x96
            _tick = _slot0.tick
            self.lp_fee = _slot0.lp_fee
            self.protocol_fee = _slot0.protocol_fee

            state = dataclasses.replace(
                self.state,
                liquidity=_liquidity,
                sqrt_price_x96=_sqrt_price_x96,
                tick=_tick,
                block=block_number,
            )

            state_updated = state != self.state
            self._state = state
            self._state_cache[block_number] = state

            if state_updated:
                self._notify_subscribers(
                    message=UniswapV4PoolStateUpdated(state),
                )

            if not silent:  # pragma: no cover
                logger.info(f"Liquidity: {self.liquidity}")
                logger.info(f"SqrtPriceX96: {self.sqrt_price_x96}")
                logger.info(f"Tick: {self.tick}")

    def external_update(
        self,
        update: UniswapV4PoolExternalUpdate,
    ) -> bool:
        """
        Process a `UniswapV3PoolExternalUpdate` with one or more of the following update types:
            - `block_number`: int
            - `tick`: int
            - `liquidity`: int
            - `sqrt_price_x96`: int

        `block_number` is validated against the most recently recorded block prior to recording any
        changes.

        Returns a bool indicating whether any updated state value was recorded.

        @dev This method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """

        if update.block_number < self.update_block:
            raise ExternalUpdateError(
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}"  # noqa:E501
            )

        with self._state_lock:
            state_block = cast("BlockNumber", update.block_number)

            state = dataclasses.replace(
                self.state,
                liquidity=update.liquidity,
                sqrt_price_x96=update.sqrt_price_x96,
                tick=update.tick,
                block=state_block,
            )

            updated_state = state != self.state

            self._state = state
            self._state_cache[state_block] = state

            if updated_state:
                self._notify_subscribers(
                    message=UniswapV4PoolStateUpdated(state),
                )

            return updated_state

    def update_liquidity_map(
        self,
        update: UniswapV4PoolLiquidityMappingUpdate,
    ) -> None:
        """
        Applies an update to the liquidity map.

        @dev This method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """

        if update.liquidity == 0:
            return

        with self._state_lock:
            state_block = cast("BlockNumber", update.block_number)

            # The tick bitmap and tick data dictionaries are copies, so they can be freely modified
            # without corrupting states for previous blocks
            _tick_bitmap = self.tick_bitmap
            _tick_data = self.tick_data

            _liquidity = self.liquidity

            assert _liquidity >= 0, (
                f"Starting liquidity violates invariant: pool {self.address} {self.tick=} {self.liquidity=}"  # noqa: E501
            )

            # Adjust in-range liquidity if the modified region includes the active tick.
            # NOTE: This compares the update block to `initial_state_block` so that onchain
            # liquidity updates from blocks prior to the creation of this pool helper can be applied
            # without triggering an inconsistent invariant check. Particularly, the values for
            # `self.tick` and `self.liquidity` may not align with the pool state when these
            # liquidity events occured.
            if (
                update.tick_lower <= self.tick < update.tick_upper
                and state_block > self._initial_state_block
            ):
                _liquidity += update.liquidity
                assert _liquidity >= 0, (
                    f"In-range liquidity adjustment violated invariant: pool {self.address} {self.tick=} {self.liquidity=} {self.update_block=} {update=}"  # noqa: E501
                )

            for tick in (update.tick_lower, update.tick_upper):
                tick_word, _ = get_tick_word_and_bit_position(tick, self.tick_spacing)

                if self.sparse_liquidity_map and tick_word not in _tick_bitmap:
                    # The liquidity map at the affected word must be complete prior to changing the
                    # status of any tick
                    self._fetch_and_populate_initialized_ticks(
                        word_position=tick_word,
                        tick_bitmap=_tick_bitmap,
                        tick_data=_tick_data,
                        block_number=(
                            # Populate the liquidity data from the previous block
                            state_block - 1
                        ),
                    )

                # Get the liquidity info for this tick. If the mapping is empty at this tick, it is
                # uninitialized and must be flipped in the bitmap and initialized as empty in the
                # mapping
                if tick not in _tick_data:
                    _tick_data[tick] = UniswapV4LiquidityAtTick(
                        liquidity_net=0,
                        liquidity_gross=0,
                        block=state_block,
                    )
                    flip_tick(
                        tick_bitmap=_tick_bitmap,
                        sparse=self.sparse_liquidity_map,
                        tick=tick,
                        tick_spacing=self.tick_spacing,
                        update_block=state_block,
                    )

                current_liquidity_net = _tick_data[tick].liquidity_net
                current_liquidity_gross = _tick_data[tick].liquidity_gross

                new_liquidity_gross = current_liquidity_gross + update.liquidity
                assert new_liquidity_gross >= 0, (
                    f"Negative gross liquidity ({new_liquidity_gross})!"
                )

                if new_liquidity_gross == 0:
                    # Delete tick from the map if there is no remaining liquidity referencing it,
                    # and flip it in the bitmap
                    del _tick_data[tick]
                    flip_tick(
                        tick_bitmap=_tick_bitmap,
                        sparse=self.sparse_liquidity_map,
                        tick=tick,
                        tick_spacing=self.tick_spacing,
                        update_block=state_block,
                    )
                    continue

                # Liquidity positions include the lower tick, but exclude the upper tick.
                if tick == update.tick_lower:
                    new_liquidity_net = current_liquidity_net + update.liquidity
                else:
                    new_liquidity_net = current_liquidity_net - update.liquidity

                _tick_data[tick] = UniswapV4LiquidityAtTick(
                    liquidity_net=new_liquidity_net,
                    liquidity_gross=new_liquidity_gross,
                    block=state_block,
                )

            state = dataclasses.replace(
                self.state,
                liquidity=_liquidity,
                tick_data=_tick_data,
                tick_bitmap=_tick_bitmap,
                block=max(self.update_block, state_block),
            )
            self._state = state
            self._state_cache[state_block] = state
            self._notify_subscribers(
                message=UniswapV4PoolStateUpdated(state),
            )

    def get_arbitrage_helpers(self) -> tuple[AbstractArbitrage, ...]:
        return tuple(
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, AbstractArbitrage)
        )

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """
        Get the absolute price for the given token, expressed in units of the other.
        """

        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """
        Get the absolute exchange rate for the given token, expressed in terms of a unit amount of
        its paired token.

        e.g. taking the USDC-WETH pool in https://blog.uniswap.org/uniswap-v3-math-primer — the
        WETH/USDC exchange rate is 649004842.70137. Rounding down, this signifies that the smallest
        swap (1 USDC) results in a 649004842 WETH output.

        A V4 pool encodes the token1/token0 exchange rate in `sqrt_price_x96`, so it can be directly
        obtained.
        """

        if token not in self.tokens:
            raise DegenbotValueError(message=f"Unknown token {token}")

        state = self.state if override_state is None else override_state

        return (
            exchange_rate_from_sqrt_price_x96(state.sqrt_price_x96)
            if token == self.token1
            else 1 / exchange_rate_from_sqrt_price_x96(state.sqrt_price_x96)
        )

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal price for the given token, expressed in units of the other, corrected for
        decimal place values.
        """

        return 1 / self.get_nominal_exchange_rate(token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal rate for the given token, expressed in units of the other, corrected for
        decimal place values.
        """

        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self.token1.decimals, 10**self.token0.decimals)
            if token == self.token0
            else Fraction(10**self.token0.decimals, 10**self.token1.decimals)
        )
