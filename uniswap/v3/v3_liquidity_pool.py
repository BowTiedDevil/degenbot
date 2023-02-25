# TODO: implement a way to track when amountSpecified for a swap
# exceeds the amount actually swapped

from abc import ABC, abstractmethod
from decimal import Decimal
from threading import Lock, RLock
from typing import List, Optional, Tuple
from warnings import catch_warnings, simplefilter

from brownie import Contract, chain, multicall, network
from brownie.convert import to_address

from degenbot.exceptions import (
    ArbitrageError,
    BitmapWordUnavailableError,
    EVMRevertError,
    ExternalUpdateError,
    LiquidityPoolError,
)
from degenbot.token import Erc20Token
from degenbot.manager import Erc20TokenHelperManager

from .abi import UNISWAP_V3_POOL_ABI
from .libraries import LiquidityMath, SwapMath, TickBitmap, TickMath
from .libraries.Helpers import *
from .tick_lens import TickLens


class BaseV3LiquidityPool(ABC):

    _token_manager = Erc20TokenHelperManager()

    @abstractmethod
    def _derived():
        """
        An abstract method designed to ensure that all consumers of this API
        use a derived class instead of this base class. Calling BaseV3LiquidityPool()
        will raise a NotImplementedError exception.

        Consumers should use V3LiquidityPool() instead, or create their own derived
        class and define a `_derived` method within that class.
        """
        raise NotImplementedError

    def __init__(
        self,
        address: str,
        lens: Optional[Contract] = None,
        tokens: Optional[List[Erc20Token]] = None,
        name: str = "",
        update_method: str = "polling",
        abi: Optional[list] = None,
        extra_words: int = 250,
        silent: bool = False,  # TODO: add status print in constructor
    ):

        self.lock = Lock()
        self.rlock = RLock()

        block_number = chain.height

        self.uniswap_version = 3

        if tokens is not None:
            if len(tokens) != 2:
                raise ValueError(
                    f"Expected exactly two tokens, found {len(tokens)}"
                )

        self.address = to_address(address)

        with catch_warnings():
            simplefilter("ignore")

            if abi:
                try:
                    self._brownie_contract = Contract.from_abi(
                        name="", address=address, abi=abi
                    )
                except:
                    raise
            else:
                try:
                    self._brownie_contract = Contract(address)
                except:
                    try:
                        self._brownie_contract = Contract.from_explorer(
                            address=address, silent=True
                        )
                    except:
                        try:
                            self._brownie_contract = Contract.from_abi(
                                name="",
                                address=address,
                                abi=UNISWAP_V3_POOL_ABI,
                            )
                        except:
                            raise

        if lens:
            self.lens = lens
        else:
            try:
                self.lens = TickLens()
            except:
                raise

        try:
            if tokens:
                self.token0 = min(tokens)
                self.token1 = max(tokens)
                if not (
                    self.token0.address == self._brownie_contract.token0()
                    and self.token1.address == self._brownie_contract.token1()
                ):
                    raise ValueError(
                        "Token addresses do not match tokens recorded at contract"
                    )
                # assert self.token0.address == self._brownie_contract.token0()
                # assert self.token1.address == self._brownie_contract.token1()
            else:
                self.token0 = self._token_manager.get_erc20token(
                    address=self._brownie_contract.token0(),
                    min_abi=True,
                    silent=silent,
                    unload_brownie_contract_after_init=True,
                )
                self.token1 = self._token_manager.get_erc20token(
                    address=self._brownie_contract.token1(),
                    min_abi=True,
                    silent=silent,
                    unload_brownie_contract_after_init=True,
                )
                # self.token0 = Erc20Token(self._brownie_contract.token0())
                # self.token1 = Erc20Token(self._brownie_contract.token1())

            self.fee = self._brownie_contract.fee()  # immutable
            self.liquidity = self._brownie_contract.liquidity(
                block_identifier=block_number
            )
            self.tick_spacing = (
                self._brownie_contract.tickSpacing()
            )  # immutable
            slot0 = self._brownie_contract.slot0(block_identifier=block_number)
            self.sqrt_price_x96 = slot0[0]
            self.tick = slot0[1]
            self.tick_data = {}
            self.tick_bitmap = {}

            _tick_word, _ = self._get_tick_bitmap_position(self.tick)
            self._get_tick_data_at_word(_tick_word, block_number=block_number)

        except:
            raise

        self._update_method = update_method
        self.extra_words = extra_words

        if name:
            self.name = name
        else:
            self.name = f"{self.token0.symbol}-{self.token1.symbol} (V3, {self.fee/10000:.2f}%)"

        self.state = {}
        self._update_pool_state()
        self.update_block = block_number

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

    def _get_tick_data_at_word(
        self,
        word_position: int,
        single_word: bool = False,
        block_number: Optional[int] = None,
    ) -> dict:
        """
        Gets the initialized tick values at a specific word (a 32 byte number
        representing 256 ticks at the tickSpacing interval), stores
        the liquidity values in the `self.tick_data` dictionary using the tick
        as the key, and updates the tick_bitmap dict.

        This function attempts to use Brownie's built-in multicall for any network
        with the 'multicall2' key set. If available, it will request extra words
        in the direction of the requested position to fill in gaps in `self.tick_data`

        If multicall is set but `self.tick_data` is empty, it will fall back to fetching
        a single word only. This is a time-saving technique since this should only occur
        inside the constructor when the pool helper is being created.

        Uses a lock to guard state-modifying methods that might cause race conditions
        when used with threads.
        """

        # bugfix: this method calls itself recursively, so the single-use Lock was causing
        # a deadlock. Changed to RLock which still prevents cross-thread interference but
        # allows the recursive call to execute correctly
        with self.rlock:

            if block_number is None:
                block_number = chain.height

            # if the bitmap is empty, assume that the object has just been instantiated
            # and force single_word mode to reduce startup time
            if not self.tick_bitmap:
                single_word = True

            # fetch multiple words if multicall is available for the connected network
            # and single_word mode is not active
            if (
                network.main.CONFIG.active_network.get("multicall2")
                and not single_word
            ):

                # requested word is already known. This should not occur!
                if word_position in self.tick_bitmap.keys():
                    print(
                        f"(V3LiquidityPool) {word_position=} inside known range"
                    )
                    print(f"known words: {self.tick_bitmap.keys()}")
                    print(f"{self.name}")
                    print(f"{single_word=}")
                    # exit early (debugging)
                    import sys

                    sys.exit()

                min_word = min(self.tick_bitmap.keys())
                max_word = max(self.tick_bitmap.keys())

                # requested word is inside the known range, so call this function in single-tick mode
                # and pass the return value through
                if min_word < word_position < max_word:
                    return self._get_tick_data_at_word(
                        word_position=word_position,
                        single_word=True,
                        block_number=block_number,
                    )

                # for both code sections below, `lower_word` and `upper_word` are used to feed the Python
                # built-in `range()` generator, which will include the lower word but exclude the upper word

                # requested word is above the known range
                if word_position > max_word:
                    lower_word = max_word + 1
                    upper_word = max(
                        word_position + 1, lower_word + self.extra_words
                    )

                # requested word is below the known range
                elif word_position < min_word:
                    lower_word = min(
                        word_position, min_word - self.extra_words
                    )
                    upper_word = min_word

                # fetch the bitmaps for the range
                try:
                    with multicall(block_identifier=block_number):
                        multicall_tick_bitmaps = {
                            _word: self._brownie_contract.tickBitmap(_word)
                            for _word in range(
                                lower_word,
                                upper_word,
                            )
                        }
                except Exception as e:
                    print(e)
                    print(type(e))
                    raise
                else:
                    # update the internal reference with the fetched results
                    self.tick_bitmap.update(multicall_tick_bitmaps)

                try:
                    with multicall(block_identifier=block_number):
                        multicall_tick_data = {
                            tick: (liquidityNet, liquidityGross)
                            for word_position, bitmap in multicall_tick_bitmaps.items()
                            for tick, liquidityNet, liquidityGross in self.lens._brownie_contract.getPopulatedTicksInWord(
                                self.address,
                                word_position,
                            )
                            if bitmap
                        }
                except Exception as e:
                    print(e)
                    print(type(e))
                    raise
                else:
                    self.tick_data.update(multicall_tick_data)

            else:
                # fetch words one by one
                try:
                    if tick_bitmap := self._brownie_contract.tickBitmap(
                        word_position
                    ):
                        _tick_data = self.lens._brownie_contract.getPopulatedTicksInWord(
                            self.address,
                            word_position,
                            block_identifier=block_number,
                        )
                    else:
                        _tick_data = ()
                except:
                    raise
                else:
                    if tick_bitmap:
                        for (
                            tick,
                            liquidityNet,
                            liquidityGross,
                        ) in _tick_data:
                            self.tick_data[tick] = (
                                liquidityNet,
                                liquidityGross,
                            )
                    self.tick_bitmap.update({word_position: tick_bitmap})

        return self.tick_data

    def _update_pool_state(self):
        self.state = {
            "liquidity": self.liquidity,
            "sqrt_price_x96": self.sqrt_price_x96,
            "tick": self.tick,
        }

    def __UniswapV3Pool_swap(
        self,
        zeroForOne: bool,
        amountSpecified: int,
        sqrtPriceLimitX96: int,
        override_start_liquidity: Optional[int] = None,
        override_start_sqrt_price_x96: Optional[int] = None,
        override_start_tick: Optional[int] = None,
        override_tick_data: Optional[
            dict
        ] = None,  # TODO: support tick data overrides
        override_tick_bitmap: Optional[
            dict
        ] = None,  # TODO: support tick bitmap overrides
    ) -> Tuple[int, int, int, int, int]:

        """
        This function is ported and adapted from the UniswapV3Pool.sol contract
        at https://github.com/Uniswap/v3-core/blob/main/contracts/UniswapV3Pool.sol

        It is called by the `calculate_tokens_in_from_tokens_out` and `calculate_tokens_out_from_tokens_in` methods to calculate
        swap amounts, ticks crossed, liquidity changes at various ticks, etc.

        It is a double-underscore method and is thus obscured from external access (but still accessible if you know how).
        """

        if amountSpecified == 0:
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
            # WIP: support override
            # sqrtPriceLimitX96 < self.sqrt_price_x96
            sqrtPriceLimitX96 < sqrt_price_x96
            and sqrtPriceLimitX96 > TickMath.MIN_SQRT_RATIO
            if zeroForOne
            # WIP: support override
            # else sqrtPriceLimitX96 > self.sqrt_price_x96
            else sqrtPriceLimitX96 > sqrt_price_x96
            and sqrtPriceLimitX96 < TickMath.MAX_SQRT_RATIO
        ):
            raise EVMRevertError(f"SPL")

        cache = {
            "liquidityStart": liquidity,
            "tickCumulative": 0,
            # ignored attributes:
            #   - blockTimestamp
            #   - feeProtocol
            #   - secondsPerLiquidityCumulativeX128
            #   - computedLatestObservation
        }

        exactInput: bool = amountSpecified > 0

        state = {
            "amountSpecifiedRemaining": amountSpecified,
            "amountCalculated": 0,
            "sqrtPriceX96": sqrt_price_x96,
            "tick": tick,
            "liquidity": cache["liquidityStart"],
            # ignored attributes:
            #   - feeGrowthGlobalX128
            #   - protocolFee
        }

        while (
            state["amountSpecifiedRemaining"] != 0
            and state["sqrtPriceX96"] != sqrtPriceLimitX96
        ):

            step = {}

            step["sqrtPriceStartX96"] = state["sqrtPriceX96"]

            while True:
                try:
                    (
                        step["tickNext"],
                        step["initialized"],
                    ) = TickBitmap.nextInitializedTickWithinOneWord(
                        self.tick_bitmap,
                        state["tick"],
                        self.tick_spacing,
                        zeroForOne,
                    )
                except BitmapWordUnavailableError as e:
                    wordPos = e.args[-1]
                    # BUG: 'word_position=XXX inside known range' exception is being thrown here
                    # when the helper is being updated by multiple threads
                    # print(f"(swap) {self.name} fetching word {wordPos}")
                    self._get_tick_data_at_word(wordPos)
                else:
                    break

            # ensure that we do not overshoot the min/max tick, as the tick bitmap is not aware of these bounds
            if step["tickNext"] < TickMath.MIN_TICK:
                step["tickNext"] = TickMath.MIN_TICK
            elif step["tickNext"] > TickMath.MAX_TICK:
                step["tickNext"] = TickMath.MAX_TICK

            # get the price for the next tick
            step["sqrtPriceNextX96"] = TickMath.getSqrtRatioAtTick(
                step["tickNext"]
            )

            # compute values to swap to the target tick, price limit, or point where input/output amount is exhausted
            (
                state["sqrtPriceX96"],
                step["amountIn"],
                step["amountOut"],
                step["feeAmount"],
            ) = SwapMath.computeSwapStep(
                state["sqrtPriceX96"],
                sqrtPriceLimitX96
                if (
                    step["sqrtPriceNextX96"] < sqrtPriceLimitX96
                    if zeroForOne
                    else step["sqrtPriceNextX96"] > sqrtPriceLimitX96
                )
                else step["sqrtPriceNextX96"],
                state["liquidity"],
                state["amountSpecifiedRemaining"],
                self.fee,
            )

            if exactInput:
                state["amountSpecifiedRemaining"] -= to_int256(
                    step["amountIn"] + step["feeAmount"]
                )
                state["amountCalculated"] = to_int256(
                    state["amountCalculated"] - step["amountOut"]
                )
            else:
                state["amountSpecifiedRemaining"] += to_int256(
                    step["amountOut"]
                )
                state["amountCalculated"] = to_int256(
                    state["amountCalculated"]
                    + step["amountIn"]
                    + step["feeAmount"]
                )

            # shift tick if we reached the next price
            if state["sqrtPriceX96"] == step["sqrtPriceNextX96"]:
                # if the tick is initialized, run the tick transition
                if step["initialized"]:

                    # use the default of (0,0) so the tuple assignment works. Throws exception
                    # if the default value (None) is returned from get()
                    liquidityNet, liquidityGross = self.tick_data.get(
                        step["tickNext"],
                        (0, 0),
                    )

                    if (liquidityNet, liquidityGross) == (0, 0):
                        raise ArbitrageError(
                            f"(UniswapLpCycle) swap function indicated tick={step['tickNext']} was initialized, but tick_data has no record at this tick!"
                        )

                    if zeroForOne:
                        liquidityNet = -liquidityNet

                    state["liquidity"] = LiquidityMath.addDelta(
                        state["liquidity"], liquidityNet
                    )

                state["tick"] = (
                    step["tickNext"] - 1 if zeroForOne else step["tickNext"]
                )

            elif state["sqrtPriceX96"] != step["sqrtPriceStartX96"]:
                # recompute unless we're on a lower tick boundary (i.e. already transitioned ticks), and haven't moved
                state["tick"] = TickMath.getTickAtSqrtRatio(
                    state["sqrtPriceX96"]
                )

        amount0, amount1 = (
            (
                amountSpecified - state["amountSpecifiedRemaining"],
                state["amountCalculated"],
            )
            if zeroForOne == exactInput
            else (
                state["amountCalculated"],
                amountSpecified - state["amountSpecifiedRemaining"],
            )
        )

        return (
            amount0,
            amount1,
            state["sqrtPriceX96"],
            state["liquidity"],
            state["tick"],
        )

    def auto_update(
        self,
        silent: bool = True,
        block_number: Optional[int] = None,
    ) -> Tuple[bool, dict]:
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

        with self.lock:

            updated = False

            # use the block_number if provided, otherwise pull from Brownie
            if block_number is None:
                block_number = chain.height

            # only process calls if the submitted block number (or retrieved block number)
            # is equal to or exceeds the block number of the last update

            if block_number < self.update_block:
                raise ExternalUpdateError(
                    f"Current state recorded at block {self.update_block}, received update for stale block {block_number}"
                )

            try:
                _sqrt_price_x96, _tick, *_ = self._brownie_contract.slot0(
                    block_identifier=block_number,
                )
                _liquidity = self._brownie_contract.liquidity(
                    block_identifier=block_number
                )
            except:
                raise
            else:
                self.update_block = block_number

                if (
                    _sqrt_price_x96 != self.sqrt_price_x96
                    or _tick != self.tick
                ):
                    updated = True
                    self.sqrt_price_x96 = _sqrt_price_x96
                    self.tick = _tick

                if _liquidity != self.liquidity:
                    updated = True
                    self.liquidity = _liquidity

                if updated:
                    self._update_pool_state()

                if not silent:
                    print(f"Liquidity: {self.liquidity}")
                    print(f"SqrtPriceX96: {self.sqrt_price_x96}")
                    print(f"Tick: {self.tick}")

        return updated, self.state

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: Optional[dict] = None,
    ) -> int:
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

        Some swaps cannot consume the entire input amount.

        Accepts a dictionary of state values (`override_state`) to allow calculations beginning from an
        arbitrary starting point. This dictionary must have one or more of the following keys:
            - 'liquidity'
            - 'sqrt_price_x96'
            - 'tick'
            - 'tick_data'  (not yet implemented)
            - 'tick_bitmap'  (not yet implemented)
        """

        # TODO: adjust return so a delta is returned as the second parameter. e.g. attempting to swap 1000 tokens
        # but only 999 are consumed by the swap, a delta of 1 is returned as the second value.

        if token_in not in (self.token0, self.token1):
            raise LiquidityPoolError("token_in not found!")

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_in == self.token0 else False

        if override_state is None:
            override_state = {}

        try:
            # delegate calculations to the ported `swap` function
            (amount0_delta, amount1_delta, *_,) = self.__UniswapV3Pool_swap(
                zeroForOne=zeroForOne,
                amountSpecified=token_in_quantity,
                sqrtPriceLimitX96=(
                    TickMath.MIN_SQRT_RATIO + 1
                    if zeroForOne
                    else TickMath.MAX_SQRT_RATIO - 1
                ),
                override_start_liquidity=override_state.get("liquidity"),
                override_start_sqrt_price_x96=override_state.get(
                    "sqrt_price_x96"
                ),
                override_start_tick=override_state.get("tick"),
                override_tick_bitmap=override_state.get("tick_bitmap"),
                override_tick_data=override_state.get("tick_data"),
            )
        except EVMRevertError as e:
            raise LiquidityPoolError(f"Simulated execution reverted: {e}")
        else:
            # if zeroForOne:
            #     if token_in_quantity != amount0:
            #         print(f"input not completely consumed!")
            #         print(f"{token_in_quantity=}")
            #         print(f"{amount0=}")
            #         print(f"{amount1=}")
            # else:
            #     if token_in_quantity != amount1:
            #         print(f"input not completely consumed!")
            #         print(f"{token_in_quantity=}")
            #         print(f"{amount0=}")
            #         print(f"{amount1=}")

            # return (
            #     (
            #         -amount1,
            #         amount0 - token_in_quantity,
            #     )
            #     if zeroForOne
            #     else (
            #         -amount0,
            #         amount1 - token_in_quantity,
            #     )
            # )

            return -amount1_delta if zeroForOne else -amount0_delta

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: Optional[dict] = None,
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
            raise LiquidityPoolError("token_in not found!")

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_out == self.token1 else False

        if override_state is None:
            override_state = {}

        try:
            # delegate calculations to the ported `swap` function
            (amount0_delta, amount1_delta, *_,) = self.__UniswapV3Pool_swap(
                zeroForOne=zeroForOne,
                amountSpecified=-token_out_quantity,
                sqrtPriceLimitX96=(
                    TickMath.MIN_SQRT_RATIO + 1
                    if zeroForOne
                    else TickMath.MAX_SQRT_RATIO - 1
                ),
                override_start_liquidity=override_state.get("liquidity"),
                override_start_sqrt_price_x96=override_state.get(
                    "sqrt_price_x96"
                ),
                override_start_tick=override_state.get("tick"),
                override_tick_bitmap=override_state.get("tick_bitmap"),
                override_tick_data=override_state.get("tick_data"),
            )
        except EVMRevertError as e:
            raise LiquidityPoolError(f"Simulated execution reverted: {e}")
        else:
            amountIn, amountOutReceived = (
                (uint256(amount0_delta), uint256(-amount1_delta))
                if zeroForOne
                else (uint256(amount1_delta), uint256(-amount0_delta))
            )
            return amountIn

    def external_update(
        self,
        updates: dict,
        block_number: Optional[int] = None,
        silent: bool = True,
    ) -> bool:
        """
        Accepts and processes a dict with at least one key from:
            - `tick`
            - `liquidity`
            - `sqrt_price_x96`
            - `liquidity_change`: tuple with (liquidity_delta, lower_tick, upper_tick)

        If any have changed, update the `self.state` dict and `self.update_block`

        Dict entries with keys other than the above will be ignored.

        If block_number is provided, it will be checked. If omitted, the values are assumed valid and processed.

        Returns a bool indicating whether any updated state value was found and processed

        Uses a lock to guard state-modifying methods that might cause race conditions
        when used with threads.
        """

        if not (
            set(["liquidity", "sqrt_price_x96", "tick", "liquidity_change"])
            & set(updates.keys())
        ):
            raise ValueError(
                "At least one of (liquidity, sqrt_price_x96, tick, liquidity_change) must be provided"
            )

        # assert set(
        #     ["liquidity", "sqrt_price_x96", "tick", "liquidity_change"]
        # ) & set(
        #     updates.keys()
        # ), "At least one of (liquidity, sqrt_price_x96, tick, liquidity_change) must be provided"

        # if block_number was not provided, pull from the Brownie chain object
        if block_number is None:
            block_number = chain.height
            print(
                f"(V3LiquidityPool.external_update) block_number was not provided, using {block_number} from chain"
            )

        if block_number < self.update_block:
            raise ExternalUpdateError(
                f"Current state recorded at block {self.update_block}, received update for stale block {block_number}"
            )

        updated = False

        for key, value in updates.items():
            if key == "tick":
                self.tick = value
                updated = True
            elif key == "liquidity":
                self.liquidity = value
                updated = True
            elif key == "sqrt_price_x96":
                self.sqrt_price_x96 = value
                updated = True
            elif key == "liquidity_change":
                liquidity_delta, lower_tick, upper_tick = value

                if lower_tick <= self.tick <= upper_tick:
                    # prev_liq = self.liquidity
                    self.liquidity += liquidity_delta
                    # new_liq = self.liquidity
                    # print(f"In-range liquidity: was {prev_liq}, now {new_liq}")
                    self.state["liquidity"] = self.liquidity
                    updated = True

                for i, tick in enumerate([lower_tick, upper_tick]):

                    tick_word, _ = self._get_tick_bitmap_position(tick)

                    # check if the word containing this tick is known, fetch if not
                    if tick_word not in self.tick_bitmap.keys():
                        # print(
                        #     f"(external_update) word {tick_word} missing, fetching single tick..."
                        # )
                        self._get_tick_data_at_word(
                            word_position=tick_word,
                            single_word=True,
                            # NOTE: we fetch the previous block because if multiple liquidity events
                            # are emitted this block, delta values will be incorrectly applied unless
                            # we start from a previous block state
                            block_number=block_number - 1,
                        )

                    # get the liquidity info for this tick, or set to zero if previously uninitialized
                    if tick_liquidity := self.tick_data.get(tick):
                        (
                            tick_liquidity_net,
                            tick_liquidity_gross,
                        ) = tick_liquidity
                    else:
                        # print(f"Tick {tick} initialized")
                        TickBitmap.flipTick(
                            self.tick_bitmap,
                            tick,
                            self.tick_spacing,
                        )

                        tick_liquidity_net = 0
                        tick_liquidity_gross = 0

                    # print("tick data before")
                    # print(
                    #     f"({tick_liquidity_net}, {tick_liquidity_gross})"
                    # )

                    # MINT: add liquidity at lower tick (i==0), subtract at upper tick (i==1)
                    # BURN: subtract liquidity at lower tick (i==0), add at upper tick (i==1)
                    # NOTE: for burn events (removing liquidity), event_liquidity is negated,
                    # but the logic holds (liquidityNet is reduced at the start of the range,
                    # and increased at the end of the range)

                    new_liquidity_net = (
                        tick_liquidity_net + liquidity_delta
                        if i == 0
                        else tick_liquidity_net - liquidity_delta
                    )
                    new_liquidity_gross = (
                        tick_liquidity_gross + liquidity_delta
                    )

                    if new_liquidity_gross == 0:
                        # print(f"Tick {tick} cleared")
                        del self.tick_data[tick]
                        TickBitmap.flipTick(
                            self.tick_bitmap,
                            tick,
                            self.tick_spacing,
                        )
                        continue
                    else:
                        self.tick_data[tick] = (
                            new_liquidity_net,
                            new_liquidity_gross,
                        )

                updated = True

            else:
                print(f"Unknown key-value pair ({key}:{value})")

        if updated:
            self.update_block = block_number
            self.state.update(
                {
                    "tick": self.tick,
                    "liquidity": self.liquidity,
                    "sqrt_price_x96": self.sqrt_price_x96,
                }
            )

        if not silent:
            print(f"Liquidity: {self.liquidity}")
            print(f"SqrtPriceX96: {self.sqrt_price_x96}")
            print(f"Tick: {self.tick}")
            print(
                f"liquidity event: {liquidity_delta} in tick range [{lower_tick},{upper_tick}], pool: {self.name}"
            )

        return updated

    def simulate_swap(
        self,
        token_in: Optional[Erc20Token] = None,
        token_in_quantity: Optional[int] = None,
        token_out: Optional[Erc20Token] = None,
        token_out_quantity: Optional[int] = None,
        sqrt_price_limit: Optional[int] = None,
        override_state: Optional[dict] = None,
    ) -> Tuple[dict]:
        """
        [TBD]
        """

        if not (
            (token_in and token_in_quantity)
            or (token_out and token_out_quantity)
        ):
            raise ValueError

        if token_in and token_out:
            raise ValueError(
                "Incompatible options! Provide token_in or token_out, but not both"
            )

        if token_in and token_in not in (self.token0, self.token1):
            raise LiquidityPoolError("token_in not found!")
        if token_out and token_out not in (self.token0, self.token1):
            raise LiquidityPoolError("token_out not found!")

        # determine whether the swap is token0 -> token1
        if token_in is not None and token_in_quantity:
            zeroForOne = True if token_in == self.token0 else False
        elif token_out is not None and token_out_quantity:
            zeroForOne = True if token_out == self.token1 else False

        if override_state is None:
            override_state = {}

        try:
            # delegate calculations to the ported `swap` function
            (
                amount0_delta,
                amount1_delta,
                end_sqrtprice,
                end_liquidity,
                end_tick,
            ) = self.__UniswapV3Pool_swap(
                zeroForOne=zeroForOne,
                amountSpecified=token_in_quantity
                if token_in_quantity
                else -token_out_quantity,
                sqrtPriceLimitX96=sqrt_price_limit
                if sqrt_price_limit is not None
                else (
                    TickMath.MIN_SQRT_RATIO + 1
                    if zeroForOne
                    else TickMath.MAX_SQRT_RATIO - 1
                ),
                override_start_liquidity=override_state.get("liquidity"),
                override_start_sqrt_price_x96=override_state.get(
                    "sqrt_price_x96"
                ),
                override_start_tick=override_state.get("tick"),
                override_tick_bitmap=override_state.get("tick_bitmap"),
                override_tick_data=override_state.get("tick_data"),
            )
        except EVMRevertError as e:
            raise LiquidityPoolError(f"Simulated execution reverted: {e}")
        else:
            return (
                {
                    "amount0_delta": amount0_delta,
                    "amount1_delta": amount1_delta,
                },
                {
                    "liquidity": end_liquidity,
                    "sqrt_price_x96": end_sqrtprice,
                    "tick": end_tick,
                },
            )


class V3LiquidityPool(BaseV3LiquidityPool):
    def _derived():
        pass
