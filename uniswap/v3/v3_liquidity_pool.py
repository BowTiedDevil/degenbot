import dataclasses
import warnings
from decimal import Decimal
from threading import Lock
from typing import TYPE_CHECKING, Dict, List, Optional, Tuple, Union

from brownie import Contract, chain, multicall, network  # type:ignore
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.constants import MAX_INT16, MIN_INT16
from degenbot.exceptions import (
    BitmapWordUnavailableError,
    BlockUnavailableError,
    EVMRevertError,
    ExternalUpdateError,
    LiquidityPoolError,
    ZeroSwapError,
)
from degenbot.logging import logger
from degenbot.manager import AllPools, Erc20TokenHelperManager
from degenbot.token import Erc20Token
from degenbot.types import PoolHelper
from degenbot.uniswap.abi import UNISWAP_V3_POOL_ABI
from degenbot.uniswap.v3.functions import generate_v3_pool_address
from degenbot.uniswap.v3.libraries import (
    LiquidityMath,
    SwapMath,
    TickBitmap,
    TickMath,
)
from degenbot.uniswap.v3.libraries.functions import to_int256
from degenbot.uniswap.v3.tick_lens import TickLens


@dataclasses.dataclass(slots=True)
class UniswapV3BitmapAtWord:
    bitmap: int = 0
    block: Optional[int] = dataclasses.field(compare=False, default=None)

    def to_dict(self):
        return dataclasses.asdict(self)


@dataclasses.dataclass(slots=True)
class UniswapV3LiquidityAtTick:
    liquidityNet: int = 0
    liquidityGross: int = 0
    block: Optional[int] = dataclasses.field(compare=False, default=None)

    def to_dict(self):
        return dataclasses.asdict(self)


@dataclasses.dataclass(slots=True)
class UniswapV3PoolExternalUpdate:
    block_number: int = dataclasses.field(compare=False)
    liquidity: Optional[int] = None
    sqrt_price_x96: Optional[int] = None
    tick: Optional[int] = None
    liquidity_change: Optional[
        Tuple[
            int,  # Liquidity
            int,  # TickLower
            int,  # TickUpper
        ]
    ] = None


@dataclasses.dataclass(slots=True)
class UniswapV3PoolState:
    pool: "V3LiquidityPool"
    liquidity: int
    sqrt_price_x96: int
    tick: int
    tick_bitmap: Optional[Dict] = dataclasses.field(
        compare=False, default=None
    )
    tick_data: Optional[Dict] = dataclasses.field(compare=False, default=None)


@dataclasses.dataclass(slots=True)
class UniswapV3PoolSimulationResult:
    amount0_delta: int
    amount1_delta: int
    current_state: UniswapV3PoolState = dataclasses.field(compare=False)
    future_state: UniswapV3PoolState = dataclasses.field(compare=False)


class V3LiquidityPool(PoolHelper):
    # Holds a reference to a TickLens contract object. This is a singleton
    # contract so there is no need to create separate references for each pool.
    # Dict is keyed by a tuple of chain ID and factory address
    _lens_contracts: Dict[Tuple[int, ChecksumAddress], TickLens] = dict()

    uniswap_version = 3

    _TICKSPACING_BY_FEE = {
        100: 1,
        500: 10,
        3000: 60,
        10000: 200,
    }

    def __init__(
        self,
        address: str,
        fee: Optional[int] = None,
        lens: Optional[Contract] = None,
        tokens: Optional[List[Erc20Token]] = None,
        name: str = "",
        update_method: Optional[str] = None,
        abi: Optional[list] = None,
        factory_address: Optional[str] = None,
        factory_init_hash: Optional[str] = None,
        extra_words: int = 10,
        silent: bool = False,
        tick_data: Optional[dict] = None,
        tick_bitmap: Optional[dict] = None,
        state_block: Optional[int] = None,
    ):
        self.address = Web3.toChecksumAddress(address)

        # held by methods that manipulate liquidity data
        self._liquidity_lock = Lock()

        # held by methods that manipulate state data held by slot0
        self._slot0_lock = Lock()

        self.update_block = state_block if state_block else chain.height
        self.liquidity_update_block = 0

        if abi is not None:
            self.abi = abi
        else:
            self.abi = UNISWAP_V3_POOL_ABI

        self._brownie_contract = Contract.from_abi(
            name="Uniswap V3 Pool",
            address=address,
            abi=self.abi,
            persist=False,
        )

        if factory_address:
            self.factory = Web3.toChecksumAddress(factory_address)
        else:
            self.factory = Web3.toChecksumAddress(
                self._brownie_contract.factory()
            )

        if lens:
            self.lens = lens
        else:
            # Use the singleton TickLens helper if available
            try:
                self.lens = self._lens_contracts[(chain.id, self.factory)]
            except KeyError:
                self.lens = TickLens(factory_address=self.factory)
                self._lens_contracts[(chain.id, self.factory)] = self.lens

        token0_address: ChecksumAddress = Web3.toChecksumAddress(
            self._brownie_contract.token0()
        )
        token1_address: ChecksumAddress = Web3.toChecksumAddress(
            self._brownie_contract.token1()
        )

        if tokens is not None:
            if len(tokens) != 2:
                raise ValueError(
                    f"Expected exactly two tokens, found {len(tokens)}"
                )

            self.token0 = min(tokens)
            self.token1 = max(tokens)

            if not (
                self.token0 == token0_address and self.token1 == token1_address
            ):
                raise ValueError(
                    "Token addresses do not match tokens recorded at contract"
                )
        else:
            _token_manager = Erc20TokenHelperManager(chain.id)
            self.token0 = _token_manager.get_erc20token(
                address=token0_address,
                min_abi=True,
                silent=silent,
                unload_brownie_contract_after_init=True,
            )
            self.token1 = _token_manager.get_erc20token(
                address=token1_address,
                min_abi=True,
                silent=silent,
                unload_brownie_contract_after_init=True,
            )

        if fee is None:
            fee = self._brownie_contract.fee()
        self.fee: int = fee
        self.tick_spacing = self._TICKSPACING_BY_FEE[self.fee]  # immutable

        if factory_address is not None and factory_init_hash is not None:
            computed_pool_address = generate_v3_pool_address(
                token_addresses=[self.token0.address, self.token1.address],
                fee=self.fee,
                factory_address=factory_address,
                init_hash=factory_init_hash,
            )
            if computed_pool_address != self.address:
                raise ValueError(
                    f"Pool address {self.address} does not match deterministic address {computed_pool_address} from factory"
                )

        if name:
            self.name = name
        else:
            self.name = (
                f"{self.token0}-{self.token1} (V3, {self.fee/10000:.2f}%)"
            )

        self.liquidity = self._brownie_contract.liquidity(
            block_identifier=self.update_block
        )

        slot0 = self._brownie_contract.slot0(
            block_identifier=self.update_block
        )
        self.sqrt_price_x96 = slot0[0]
        self.tick = slot0[1]

        if update_method is not None:
            warnings.warn(
                "The `update_method` argument to `V3LiquidityPool()` is unused and otherwise ignored. Remove it to stop seeing this message."
            )
            self._update_method = update_method
        self.extra_words = extra_words

        # default to an empty, sparse bitmap with no tick data
        self.tick_data: Dict[int, UniswapV3LiquidityAtTick] = {}
        self.tick_bitmap: Dict[int, UniswapV3BitmapAtWord] = {}
        self.sparse_bitmap = True

        if (tick_bitmap is not None) != (tick_data is not None):
            raise ValueError(
                f"Must provide both tick_bitmap and tick_data! Got {tick_bitmap=}, {tick_data=}"
            )

        if tick_bitmap is not None and tick_data is not None:
            # if a snapshot was provided, assume it is complete
            self.sparse_bitmap = False

        if tick_bitmap is not None:
            # transform dict to UniswapV3BitmapAtWord
            self.tick_bitmap = {
                int(word): (
                    UniswapV3BitmapAtWord(**bitmap_at_word)
                    if not isinstance(
                        bitmap_at_word,
                        UniswapV3BitmapAtWord,
                    )
                    else bitmap_at_word
                )
                for word, bitmap_at_word in tick_bitmap.items()
            }

        if tick_data is not None:
            # transform dict to LiquidityAtTick
            self.tick_data = {
                int(tick): (
                    UniswapV3LiquidityAtTick(**liquidity_at_tick)
                    if not isinstance(
                        liquidity_at_tick,
                        UniswapV3LiquidityAtTick,
                    )
                    else liquidity_at_tick
                )
                for tick, liquidity_at_tick in tick_data.items()
            }

        if tick_bitmap is None and tick_data is None:
            logger.debug(
                f"{self} @ {self.address} updating -> {tick_bitmap=}, {tick_data=}"
            )
            word_position, _ = self._get_tick_bitmap_position(self.tick)

            self._update_tick_data_at_word(
                word_position,
                single_word=True,
                block_number=self.update_block,
            )

        self.state = UniswapV3PoolState(
            pool=self,
            liquidity=self.liquidity,
            sqrt_price_x96=self.sqrt_price_x96,
            tick=self.tick,
        )

        AllPools(chain.id)[self.address] = self

        if not silent:
            logger.info(self.name)
            logger.info(f"• Token 0: {self.token0}")
            logger.info(f"• Token 1: {self.token1}")
            logger.info(f"• Fee: {self.fee}")
            logger.info(f"• Liquidity: {self.liquidity}")
            logger.info(f"• SqrtPrice: {self.sqrt_price_x96}")
            logger.info(f"• Tick: {self.tick}")

    def __eq__(self, other) -> bool:
        if issubclass(type(other), PoolHelper):
            return self.address == other.address
        elif isinstance(other, str):
            return self.address.lower() == other.lower()
        else:
            raise NotImplementedError

    # Some objects cannot be pickled, so set those references to None and return the state
    def __getstate__(self):
        keys_to_remove = [
            "_brownie_contract",
            "_liquidity_lock",
            "_slot0_lock",
            "lens",
        ]
        state = self.__dict__.copy()
        for key in keys_to_remove:
            if key in state:
                del state[key]
        return state

    def __hash__(self):
        return hash(self.address)

    def __repr__(self):
        return f"V3LiquidityPool(address={self.address}, token0={self.token0}, token1={self.token1}, fee={self.fee})"

    def __setstate__(self, state):
        self.__dict__ = state

    def __str__(self):
        """
        Return the pool name when the object is included in a print statement, or cast as a string
        """
        return self.name

    def _get_tick_bitmap_position(self, tick) -> Tuple[int, int]:
        """
        Retrieves the wordPosition and bitPosition for the input tick

        This function corrects internally for tick spacing! e.g. tick=600 is the
        11th initialized tick for an LP with tickSpacing of 60, starting at 0.
        Each "word" in the tickBitmap holds 256 initialized positions, so the 11th
        position of the 1st word will represent tick=600.

        Calling `get_tick_bitmap_position(600)` returns (0,10), where:
            0 = wordPosition (zero-indexed)
            10 = bitPosition (zero-indexed)
        """
        return TickBitmap.position(int(Decimal(tick) // self.tick_spacing))

    def _update_pool_state(self) -> None:
        self.state = UniswapV3PoolState(
            pool=self,
            liquidity=self.liquidity,
            sqrt_price_x96=self.sqrt_price_x96,
            tick=self.tick,
        )

    def _update_tick_data_at_word(
        self,
        word_position: int,
        single_word: bool = False,
        block_number: Optional[int] = None,
    ) -> None:
        """
        Update the initialized tick values at a specific word (a 32 byte number
        representing 256 ticks at the tickSpacing interval). Store
        the liquidity values in the `self.tick_data` dictionary using the tick
        as the key, and update the `self.tick_bitmap` dictionary.

        This function attempts to use Brownie's built-in multicall for any network
        with the 'multicall2' key set. If available, it will request extra words
        in the direction of the requested position to fill in gaps in `self.tick_data`

        If multicall is set but `self.tick_data` is empty, it will fall back to fetching
        a single word only. This is a time-saving technique since this should only occur
        inside the constructor when the pool helper is being created.

        Uses a lock to guard state-modifying methods that might cause race conditions
        when used with threads.
        """

        # Return immediately if requested word is already known.
        # This can occur in threaded bots. The lock prevents race conditions,
        # but this method might be running simultaneously across different threads.
        # Used to throw an exception, now just returns early.

        if word_position in self.tick_bitmap:
            logger.debug(f"returning early, {word_position=} found")
            logger.debug(self.tick_bitmap[word_position])
            return

        with self._liquidity_lock:
            if block_number is None:
                block_number = chain.height

            # fetch multiple words if multicall is available for the connected network
            # and single_word mode is not active
            if (
                network.main.CONFIG.active_network.get("multicall2")
                and not single_word
            ):
                # limit word values to int16 range
                words = set(
                    range(
                        max(MIN_INT16, word_position - self.extra_words // 2),
                        min(MAX_INT16, word_position + self.extra_words // 2),
                    )
                ) - set(self.tick_bitmap)

                logger.debug(f"fetching words: {words}")

                # fetch the tick bitmaps for the range
                try:
                    with multicall(block_identifier=block_number):
                        multicall_tick_bitmaps = {
                            word: UniswapV3BitmapAtWord(
                                bitmap=self._brownie_contract.tickBitmap(word),
                                block=block_number,
                            )
                            for word in words
                        }
                    with multicall(block_identifier=block_number):
                        multicall_tick_data = {
                            tick: UniswapV3LiquidityAtTick(
                                liquidityNet=liquidity_net,
                                liquidityGross=liquidity_gross,
                                block=block_number,
                            )
                            for word_position, bitmap in multicall_tick_bitmaps.items()
                            for tick, liquidity_net, liquidity_gross in self.lens._brownie_contract.getPopulatedTicksInWord(
                                self.address,
                                word_position,
                            )
                            if bitmap
                        }
                except Exception as e:
                    print(
                        f"(V3LiquidityPool (_update_tick_data_at_word) (multicall): {e}"
                    )
                    print(type(e))
                    raise
                else:
                    self.tick_bitmap.update(multicall_tick_bitmaps)
                    self.tick_data.update(multicall_tick_data)
                    self.liquidity_update_block = block_number

            # fetch words one by one (single_tick = True)
            else:
                try:
                    if single_tick_bitmap := self._brownie_contract.tickBitmap(
                        word_position,
                        block_identifier=block_number,
                    ):
                        single_tick_data = self.lens._brownie_contract.getPopulatedTicksInWord(
                            self.address,
                            word_position,
                            block_identifier=block_number,
                        )
                except Exception as e:
                    print(
                        f"(V3LiquidityPool) (_update_tick_data_at_word) (single tick): {e}"
                    )
                    print(type(e))
                    raise
                else:
                    self.tick_bitmap[word_position] = UniswapV3BitmapAtWord(
                        bitmap=single_tick_bitmap,
                        block=block_number,
                    )
                    if single_tick_bitmap:
                        for (
                            tick,
                            liquidity_net,
                            liquidity_gross,
                        ) in single_tick_data:
                            self.tick_data[tick] = UniswapV3LiquidityAtTick(
                                liquidityNet=liquidity_net,
                                liquidityGross=liquidity_gross,
                                block=block_number,
                            )
                    self.liquidity_update_block = block_number

    def _uniswap_v3_pool_swap(
        self,
        zeroForOne: bool,
        amount_specified: int,
        sqrt_price_limit_x96: int,
        override_start_liquidity: Optional[int] = None,
        override_start_sqrt_price_x96: Optional[int] = None,
        override_start_tick: Optional[int] = None,
        # TODO: support tick data overrides
        override_tick_data: Optional[dict] = None,
        # TODO: support tick bitmap overrides
        override_tick_bitmap: Optional[dict] = None,
    ) -> Tuple[int, int, int, int, int]:
        """
        This function is ported and adapted from the UniswapV3Pool.sol contract
        at https://github.com/Uniswap/v3-core/blob/main/contracts/UniswapV3Pool.sol

        It is called by the `calculate_tokens_in_from_tokens_out` and `calculate_tokens_out_from_tokens_in` methods to calculate
        swap amounts, ticks crossed, liquidity changes at various ticks, etc.
        """

        @dataclasses.dataclass(slots=True)
        class SwapCache:
            liquidityStart: int
            tickCumulative: int

        @dataclasses.dataclass(slots=True)
        class SwapState:
            amountSpecifiedRemaining: int
            amountCalculated: int
            sqrtPriceX96: int
            tick: int
            liquidity: int

        @dataclasses.dataclass(slots=True)
        class StepComputations:
            sqrtPriceStartX96: int = 0
            tickNext: int = 0
            initialized: bool = False
            sqrtPriceNextX96: int = 0
            amountIn: int = 0
            amountOut: int = 0
            feeAmount: int = 0

        if amount_specified == 0:
            raise EVMRevertError("AS")

        if override_start_liquidity is not None:
            liquidity = override_start_liquidity
        else:
            liquidity = self.liquidity

        if override_start_sqrt_price_x96 is not None:
            sqrt_price_x96 = override_start_sqrt_price_x96
        else:
            sqrt_price_x96 = self.sqrt_price_x96

        if override_start_tick is not None:
            tick = override_start_tick
        else:
            tick = self.tick

        if not (
            sqrt_price_limit_x96 < sqrt_price_x96
            and sqrt_price_limit_x96 > TickMath.MIN_SQRT_RATIO
            if zeroForOne
            else sqrt_price_limit_x96 > sqrt_price_x96
            and sqrt_price_limit_x96 < TickMath.MAX_SQRT_RATIO
        ):
            raise EVMRevertError(f"SPL")

        cache = SwapCache(
            liquidityStart=liquidity,
            tickCumulative=0,
            # ignored attributes:
            #   - blockTimestamp
            #   - feeProtocol
            #   - secondsPerLiquidityCumulativeX128
            #   - computedLatestObservation
        )

        exactInput: bool = amount_specified > 0

        state = SwapState(
            amountSpecifiedRemaining=amount_specified,
            amountCalculated=0,
            sqrtPriceX96=sqrt_price_x96,
            tick=tick,
            liquidity=cache.liquidityStart,
            # ignored attributes:
            #   - feeGrowthGlobalX128
            #   - protocolFee
        )

        while (
            state.amountSpecifiedRemaining != 0
            and state.sqrtPriceX96 != sqrt_price_limit_x96
        ):
            step = StepComputations()

            step.sqrtPriceStartX96 = state.sqrtPriceX96

            while True:
                try:
                    (
                        step.tickNext,
                        step.initialized,
                    ) = TickBitmap.nextInitializedTickWithinOneWord(
                        self.tick_bitmap,
                        state.tick,
                        self.tick_spacing,
                        zeroForOne,
                    )
                except BitmapWordUnavailableError as e:
                    missing_word = e.args[-1]
                    if self.sparse_bitmap:
                        logger.debug(
                            f"(swap) {self.name} fetching word {missing_word}"
                        )
                        self._update_tick_data_at_word(missing_word)
                    else:
                        # bitmap is complete, so mark the word as empty
                        self.tick_bitmap[
                            missing_word
                        ] = UniswapV3BitmapAtWord()
                else:
                    # nextInitializedTickWithinOneWord will search up to 256 ticks away, which may
                    # return a tick in an adjacent word if there are no initialized ticks in the current word.
                    # This word may not be known to the helper, so check and fetch the containing word for this tick

                    # BUGFIX: previously called position directly, which implies tickSpacing=1,
                    # so the call returned an inaccurate word and short-circuited the optimization
                    tick_next_word, _ = self._get_tick_bitmap_position(
                        step.tickNext
                    )

                    if (
                        self.sparse_bitmap
                        and tick_next_word not in self.tick_bitmap
                    ):
                        logger.debug(
                            f"tickNext={step.tickNext} out of range! Fetching word={tick_next_word}"
                            f"\n{self.name}"
                        )
                        self._update_tick_data_at_word(
                            tick_next_word,
                            single_word=True,
                        )
                    break

            # ensure that we do not overshoot the min/max tick, as the tick bitmap is not aware of these bounds
            if step.tickNext < TickMath.MIN_TICK:
                step.tickNext = TickMath.MIN_TICK
            elif step.tickNext > TickMath.MAX_TICK:
                step.tickNext = TickMath.MAX_TICK

            # get the price for the next tick
            step.sqrtPriceNextX96 = TickMath.getSqrtRatioAtTick(step.tickNext)

            # compute values to swap to the target tick, price limit, or point where input/output amount is exhausted
            (
                state.sqrtPriceX96,
                step.amountIn,
                step.amountOut,
                step.feeAmount,
            ) = SwapMath.computeSwapStep(
                state.sqrtPriceX96,
                sqrt_price_limit_x96
                if (
                    step.sqrtPriceNextX96 < sqrt_price_limit_x96
                    if zeroForOne
                    else step.sqrtPriceNextX96 > sqrt_price_limit_x96
                )
                else step.sqrtPriceNextX96,
                state.liquidity,
                state.amountSpecifiedRemaining,
                self.fee,
            )

            if exactInput:
                state.amountSpecifiedRemaining -= to_int256(
                    step.amountIn + step.feeAmount
                )
                state.amountCalculated = to_int256(
                    state.amountCalculated - step.amountOut
                )
            else:
                state.amountSpecifiedRemaining += to_int256(step.amountOut)
                state.amountCalculated = to_int256(
                    state.amountCalculated + step.amountIn + step.feeAmount
                )

            # shift tick if we reached the next price
            if state.sqrtPriceX96 == step.sqrtPriceNextX96:
                # if the tick is initialized, run the tick transition
                if step.initialized:
                    tick_next = step.tickNext
                    liquidityNet = self.tick_data[tick_next].liquidityNet

                    if zeroForOne:
                        liquidityNet = -liquidityNet

                    state.liquidity = LiquidityMath.addDelta(
                        state.liquidity, liquidityNet
                    )

                state.tick = step.tickNext - 1 if zeroForOne else step.tickNext

            elif state.sqrtPriceX96 != step.sqrtPriceStartX96:
                # recompute unless we're on a lower tick boundary (i.e. already transitioned ticks), and haven't moved
                state.tick = TickMath.getTickAtSqrtRatio(state.sqrtPriceX96)

        amount0, amount1 = (
            (
                amount_specified - state.amountSpecifiedRemaining,
                state.amountCalculated,
            )
            if zeroForOne == exactInput
            else (
                state.amountCalculated,
                amount_specified - state.amountSpecifiedRemaining,
            )
        )

        return (
            amount0,
            amount1,
            state.sqrtPriceX96,
            state.liquidity,
            state.tick,
        )

    def auto_update(
        self,
        silent: bool = True,
        block_number: Optional[int] = None,
    ) -> Tuple[bool, UniswapV3PoolState]:
        """
        Retrieves the current slot0 and liquidity values from the LP, stores any that have changed,
        and returns a tuple with a status boolean indicating whether any update was found,
        and a dictionary holding current state values:
            - liquidity
            - sqrt_price_x96
            - tick

        Uses a lock to guard state-modifying methods that might cause race conditions
        when used with threads.
        """

        with self._slot0_lock:
            updated = False

            # use the block_number if provided, otherwise pull from Brownie
            if block_number is None:
                block_number = chain.height

            if block_number < self.update_block:
                raise ExternalUpdateError(
                    f"Current state recorded at block {self.update_block}, received update for stale block {block_number}"
                )

            _sqrt_price_x96, _tick, *_ = self._brownie_contract.slot0(
                block_identifier=block_number,
            )
            _liquidity = self._brownie_contract.liquidity(
                block_identifier=block_number
            )

            if self.sqrt_price_x96 != _sqrt_price_x96:
                updated = True
                self.sqrt_price_x96 = _sqrt_price_x96

            if self.tick != _tick:
                updated = True
                self.tick = _tick

            if self.liquidity != _liquidity:
                updated = True
                self.liquidity = _liquidity

            if updated:
                self._update_pool_state()

            if not silent:
                logger.info(f"Liquidity: {self.liquidity}")
                logger.info(f"SqrtPriceX96: {self.sqrt_price_x96}")
                logger.info(f"Tick: {self.tick}")

            # WORKAROUND: update the block even if there are no state changes
            # pools were being repeatedly caught by "stale pool" checks
            self.update_block = block_number

        return updated, self.state

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: Optional[UniswapV3PoolState] = None,
        with_remainder: bool = False,
    ) -> Union[int, Tuple[int, int]]:
        """
        This function implements the common degenbot interface `calculate_tokens_out_from_tokens_in`
        to calculate the number of tokens withdrawn (out) for a given number of tokens deposited (in).

        It is similar to calling quoteExactInputSingle using the quoter contract with arguments:
        `quoteExactInputSingle(
            tokenIn=token_in,
            tokenOut=[automatically determined by helper],
            fee=[automatically determined by helper],
            amountIn=token_in_quantity,
            sqrt_price_limitX96 = 0
        )` which returns the value `amountOut`

        Note that this wrapper function always assumes that the sqrt_price_limitx96 argument is unset,
        thus the swap calculation will continue until the target amount is satisfied, regardless of
        price impact

        Some swaps will not consume the entire input amount, so call this function using `with_remainder=True`
        to include that leftover value with the return and offset the input

        Accepts a dictionary of state values (`override_state`) to allow calculations beginning from an
        arbitrary starting point. This dictionary must have one or more of the following keys:
            - 'liquidity'
            - 'sqrt_price_x96'
            - 'tick'
            - 'tick_data'  (not yet implemented)
            - 'tick_bitmap'  (not yet implemented)
        """

        if token_in not in (self.token0, self.token1):
            raise ValueError("token_in not found!")

        if override_state:
            logger.debug(f"V3 calc with overridden state: {override_state}")

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_in == self.token0 else False

        try:
            amount0_delta, amount1_delta, *_ = self._uniswap_v3_pool_swap(
                zeroForOne=zeroForOne,
                amount_specified=token_in_quantity,
                sqrt_price_limit_x96=(
                    TickMath.MIN_SQRT_RATIO + 1
                    if zeroForOne
                    else TickMath.MAX_SQRT_RATIO - 1
                ),
                override_start_liquidity=override_state.liquidity
                if override_state
                else None,
                override_start_sqrt_price_x96=override_state.sqrt_price_x96
                if override_state
                else None,
                override_start_tick=override_state.tick
                if override_state
                else None,
                override_tick_bitmap=override_state.tick_bitmap
                if override_state
                else None,
                override_tick_data=override_state.tick_data
                if override_state
                else None,
            )
        except EVMRevertError as e:
            raise LiquidityPoolError(
                f"Simulated execution reverted: {e}"
            ) from e
        else:
            # if zeroForOne:
            #     if token_in_quantity != amount0_delta:
            #         print(f"input not completely consumed!")
            #         print(f"{token_in_quantity=}")
            #         print(f"{amount0_delta=}")
            # else:
            #     if token_in_quantity != amount1_delta:
            #         print(f"input not completely consumed!")
            #         print(f"{token_in_quantity=}")
            #         print(f"{amount1_delta=}")

            if with_remainder:
                return (
                    (-amount1_delta, token_in_quantity - amount0_delta)
                    if zeroForOne
                    else (-amount0_delta, token_in_quantity - amount1_delta)
                )
            else:
                return -amount1_delta if zeroForOne else -amount0_delta

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: Optional[UniswapV3PoolState] = None,
    ) -> int:
        """
        This function implements the common degenbot interface `calculate_tokens_in_from_tokens_out`
        to calculate the number of tokens deposited (in) for a given number of tokens withdrawn (out).

        It is similar to calling quoteExactOutputSingle using the quoter contract with arguments:
        `quoteExactOutputSingle(
            tokenIn=[automatically determined by helper],
            tokenOut=token_out,
            fee=[automatically determined by helper],
            amountOut=token_out_quantity,
            sqrt_price_limitX96 = 0
        )` which returns the value `amountIn`

        Note that this wrapper function always assumes that the sqrt_price_limitx96 argument is unset, thus the
        swap calculation will continue until the target amount is satisfied, regardless of price impact

        Accepts a dictionary of state values (`override_state`) to allow calculations beginning from an
        arbitrary starting point. This dictionary must have one or more of the following keys:
            - 'liquidity'
            - 'sqrt_price_x96'
            - 'tick'
            - 'tick_data'  (not yet implemented)
            - 'tick_bitmap'  (not yet implemented)
        """

        if token_out not in (self.token0, self.token1):
            raise ValueError("token_in not found!")

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_out == self.token1 else False

        try:
            amount0_delta, amount1_delta, *_ = self._uniswap_v3_pool_swap(
                zeroForOne=zeroForOne,
                amount_specified=-token_out_quantity,
                sqrt_price_limit_x96=(
                    TickMath.MIN_SQRT_RATIO + 1
                    if zeroForOne
                    else TickMath.MAX_SQRT_RATIO - 1
                ),
                override_start_liquidity=override_state.liquidity
                if override_state
                else None,
                override_start_sqrt_price_x96=override_state.sqrt_price_x96
                if override_state
                else None,
                override_start_tick=override_state.tick
                if override_state
                else None,
                override_tick_bitmap=override_state.tick_bitmap
                if override_state
                else None,
                override_tick_data=override_state.tick_data
                if override_state
                else None,
            )
        except EVMRevertError as e:
            raise LiquidityPoolError(
                f"Simulated execution reverted: {e}"
            ) from e
        else:
            amountIn, amountOutReceived = (
                (amount0_delta, -amount1_delta)
                if zeroForOne
                else (amount1_delta, -amount0_delta)
            )

            return amountIn

    def external_update(
        self,
        update: Optional[UniswapV3PoolExternalUpdate] = None,
        updates: Optional[dict] = None,
        block_number: Optional[int] = None,
        silent: bool = True,
        fetch_missing: Optional[bool] = None,
        force: bool = False,  # added primarily to support liquidity bootstrapping without excessive refactoring
    ) -> bool:
        """
        Process a `UniswapV3PoolExternalUpdate` with one or more of the following update types:
            - `tick`: int
            - `liquidity`: int
            - `sqrt_price_x96`: int
            - `liquidity_change`: tuple of (liquidity_delta, lower_tick, upper_tick). The delta can be positive or negative to indicate added or removed liquidity.

        `block_number` is validated against the most recently recorded block prior to recording any changes. If `force=True`, the block check is skipped.

        If any update is processed, `self.state` and `self.update_block` are updated.

        Returns a bool indicating whether any updated state value was recorded.

        @dev This method uses a lock to guard state-modifying methods that might cause race conditions when used with threads.
        """

        def is_valid_update_block(block_number) -> bool:
            """
            Check if `block_number` is valid (matches or exceeds the last update block)
            """
            return block_number >= self.update_block

        def is_valid_liquidity_update_block(block_number) -> bool:
            """
            Check if `block_number` is valid (matches or exceeds the last liquidity update block)
            """
            return block_number >= self.liquidity_update_block

        if fetch_missing is not None:
            raise DeprecationWarning(
                "The fetch_missing argument has been deprecated, to address this exception remove it from any calls to external_update"
            )

        # warnings.warn(
        #     "\n"
        #     + "The `updates` dict argument is deprecated and will be "
        #     + "removed in the future. It has been converted in-place to a "
        #     + "`UniswapV3PoolExternalUpdate` object. Pass this using the "
        #     + "`update=` argument to remove this warning."
        #     + "\n"
        #     + "For the values you've provided, pass this data using "
        #     + "the format: \n"
        #     + "update=UniswapV3PoolExternalUpdate(\n"
        #     + (
        #         f"    liquidity={val}\n"
        #         if (val := updates.get("liquidity")) is not None
        #         else ""
        #     )
        #     + (
        #         f"    sqrt_price_x96={val}\n"
        #         if (val := updates.get("sqrt_price_x96")) is not None
        #         else ""
        #     )
        #     + (
        #         f"    tick={val}\n"
        #         if (val := updates.get("tick")) is not None
        #         else ""
        #     )
        #     + ")",
        # )

        if TYPE_CHECKING:
            assert isinstance(update, UniswapV3PoolExternalUpdate)

        # if block_number was not provided, pull from Brownie
        if block_number is None:
            if update is not None:
                block_number = update.block_number
            else:
                block_number = chain.height
                warnings.warn(
                    f"(V3LiquidityPool.external_update) block_number was not provided, using {block_number} from chain"
                )

        if updates and not update:
            update = UniswapV3PoolExternalUpdate(
                block_number=block_number,
                liquidity=updates.get("liquidity"),
                sqrt_price_x96=updates.get("sqrt_price_x96"),
                tick=updates.get("tick"),
                liquidity_change=updates.get("liquidity_change"),
            )

        if update.liquidity or update.sqrt_price_x96 or update.tick:
            if not force and not is_valid_update_block(block_number):
                raise ExternalUpdateError(
                    f"Rejected update for block {block_number} in the past, current update block is {self.update_block}"
                )

        if update.liquidity_change:
            if not force and not is_valid_liquidity_update_block(block_number):
                raise ExternalUpdateError(
                    f"Rejected liquidity update for past block {block_number}, current liquidity update block is {self.liquidity_update_block}"
                )

        with self._slot0_lock:
            updated_state = False

            for update_type in ["tick", "liquidity", "sqrt_price_x96"]:
                if (
                    update_value := getattr(update, update_type, None)
                ) and update_value != getattr(self, update_type):
                    setattr(self, update_type, update_value)
                    updated_state = True

            if update.liquidity_change:
                (
                    liquidity_delta,
                    lower_tick,
                    upper_tick,
                ) = update.liquidity_change

                # adjust in-range liquidity if current tick is within the position's range
                if (
                    lower_tick <= self.tick < upper_tick
                    # bugfix: check the liquidity update timestamp  - fixes issue where
                    # liquidity events were applied after a slot0 update, which put
                    # `self.liquidity` into an inconsistent state
                    and is_valid_update_block(block_number)
                ):
                    self.liquidity += liquidity_delta
                    logger.debug(
                        f"Adjusting in-range liquidity {block_number=}, {self.update_block=}, {self.tick=}, {self.address=}, {self.liquidity=}"
                    )
                    assert (
                        self.liquidity >= 0
                    ), f"{self.address=} {self.liquidity=} {update=} {block_number=} {self.tick=}"

                for i, tick in enumerate([lower_tick, upper_tick]):
                    tick_word, _ = self._get_tick_bitmap_position(tick)

                    if tick_word not in self.tick_bitmap:
                        # the tick bitmap must be available for the word prior to flipping
                        # the initialized status of any tick

                        if self.sparse_bitmap:
                            logger.debug(
                                f"(external_update) {tick_word=} not found in tick_bitmap {self.tick_bitmap.keys()=}"
                            )
                            try:
                                # fetch the single word
                                self._update_tick_data_at_word(
                                    word_position=tick_word,
                                    single_word=True,
                                    # Fetch the word using the previous block as a known "good" state snapshot
                                    block_number=block_number - 1,
                                )
                            except ValueError as e:
                                raise BlockUnavailableError(
                                    f"Could not query chain at block {block_number - 1}"
                                )
                        else:
                            # The bitmap is complete (sparse=False), so mark this word as empty
                            self.tick_bitmap[
                                tick_word
                            ] = UniswapV3BitmapAtWord()

                    # Get the liquidity info for this tick
                    try:
                        tick_liquidity_net, tick_liquidity_gross = (
                            self.tick_data[tick].liquidityNet,
                            self.tick_data[tick].liquidityGross,
                        )
                    except (KeyError, AttributeError) as e:
                        # if it doesn't exist, initialize the tick and set the current values to zero
                        tick_liquidity_net = 0
                        tick_liquidity_gross = 0
                        TickBitmap.flipTick(
                            self.tick_bitmap,
                            tick,
                            self.tick_spacing,
                            update_block=block_number,
                        )

                    # MINT: add liquidity at lower tick (i==0), subtract at upper tick (i==1)
                    # BURN: subtract liquidity at lower tick (i==0), add at upper tick (i==1)
                    # Same equation, but for BURN events the liquidity_delta value is negative
                    new_liquidity_net = (
                        tick_liquidity_net + liquidity_delta
                        if i == 0
                        else tick_liquidity_net - liquidity_delta
                    )
                    new_liquidity_gross = (
                        tick_liquidity_gross + liquidity_delta
                    )

                    # Delete entirely if there is no liquidity referencing this tick, then flip it in the bitmap
                    if new_liquidity_gross == 0:
                        del self.tick_data[tick]
                        TickBitmap.flipTick(
                            self.tick_bitmap,
                            tick,
                            self.tick_spacing,
                            update_block=block_number,
                        )
                    # otherwise record the new values
                    else:
                        self.tick_data[tick] = UniswapV3LiquidityAtTick(
                            liquidityNet=new_liquidity_net,
                            liquidityGross=new_liquidity_gross,
                            block=block_number,
                        )

                self.liquidity_update_block = block_number

            if not silent:
                logger.debug(f"Liquidity: {self.liquidity}")
                logger.debug(f"SqrtPriceX96: {self.sqrt_price_x96}")
                logger.debug(f"Tick: {self.tick}")
                logger.debug(
                    f"liquidity event: {liquidity_delta} in tick range [{lower_tick},{upper_tick}], pool: {self.name}"
                    "\n"
                    f"old liquidity: {tick_liquidity_net} net, {tick_liquidity_gross} gross"
                    "\n"
                    f"new liquidity: {new_liquidity_net} net, {new_liquidity_gross} gross"
                )
                logger.debug(
                    f"update block: {block_number} (last={self.update_block})"
                )

            if updated_state:
                self._update_pool_state()
                # if the update was forced, do not refresh the update block
                if not force:
                    self.update_block = block_number

            return updated_state

    def simulate_swap(
        self,
        token_in: Optional[Erc20Token] = None,
        token_in_quantity: Optional[int] = None,
        token_out: Optional[Erc20Token] = None,
        token_out_quantity: Optional[int] = None,
        sqrt_price_limit: Optional[int] = None,
        override_state: Optional[UniswapV3PoolState] = None,
    ) -> UniswapV3PoolSimulationResult:
        """
        [TBD]
        """

        if token_in is None and token_out is None:
            raise ValueError("token_in or token_out not provided")

        if token_in_quantity and token_out_quantity:
            raise ValueError(
                "Provide token_in_quantity or token_out_quantity, not both"
            )

        if token_in is not None:
            if token_in_quantity is None:
                raise ValueError("token_in_quantity not provided")
            if token_in not in (self.token0, self.token1):
                raise ValueError("token_in not provided")

        if token_out is not None:
            if token_out_quantity is None:
                raise ValueError("token_out_quantity not provided")
            if token_out not in (self.token0, self.token1):
                raise ValueError("token_out not provided")

        if 0 in (token_in_quantity, token_out_quantity):
            raise ZeroSwapError("Zero input/output swap requested")

        # determine whether the swap is token0 -> token1
        if token_in is not None:
            zeroForOne = True if token_in == self.token0 else False
        elif token_out is not None:
            zeroForOne = True if token_out == self.token1 else False

        _sqrt_price_limit = (
            sqrt_price_limit
            if sqrt_price_limit is not None
            else (
                TickMath.MIN_SQRT_RATIO + 1
                if zeroForOne
                else TickMath.MAX_SQRT_RATIO - 1
            )
        )

        if token_in_quantity is not None:
            _amount_specified = token_in_quantity
        elif token_out_quantity is not None:
            _amount_specified = -token_out_quantity

        try:
            (
                amount0_delta,
                amount1_delta,
                end_sqrtprice,
                end_liquidity,
                end_tick,
            ) = self._uniswap_v3_pool_swap(
                zeroForOne=zeroForOne,
                amount_specified=_amount_specified,
                sqrt_price_limit_x96=_sqrt_price_limit,
                override_start_liquidity=override_state.liquidity
                if override_state
                else None,
                override_start_sqrt_price_x96=override_state.sqrt_price_x96
                if override_state
                else None,
                override_start_tick=override_state.tick
                if override_state
                else None,
                override_tick_bitmap=override_state.tick_bitmap
                if override_state
                else None,
                override_tick_data=override_state.tick_data
                if override_state
                else None,
            )
        except EVMRevertError as e:
            raise LiquidityPoolError(
                f"Simulated execution reverted: {e}"
            ) from e
        else:
            return UniswapV3PoolSimulationResult(
                amount0_delta=amount0_delta,
                amount1_delta=amount1_delta,
                current_state=self.state,
                future_state=UniswapV3PoolState(
                    pool=self,
                    liquidity=end_liquidity,
                    sqrt_price_x96=end_sqrtprice,
                    tick=end_tick,
                ),
            )
