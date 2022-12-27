from abc import ABC, abstractmethod
from typing import Tuple, List

from decimal import Decimal

from brownie import Contract, chain, network, multicall
from brownie.convert import to_address

from degenbot.token import Erc20Token
from degenbot.exceptions import (
    EVMRevertError,
    ExternalUpdateError,
    LiquidityPoolError,
)

from warnings import catch_warnings, simplefilter

from .abi import UNISWAP_V3_POOL_ABI
from .libraries import LiquidityMath, SwapMath, TickBitmap, TickMath
from .libraries.Helpers import *
from .tick_lens import TickLens


class BaseV3LiquidityPool(ABC):
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
        lens: Contract = None,
        tokens: List[Erc20Token] = [],
        name: str = "",
        update_method: str = "polling",
        abi: list = None,
        # unload_brownie_contract_after_init: bool = False,
        populate_ticks: bool = True,
    ):

        block_number = chain.height

        self.uniswap_version = 3

        if tokens:
            assert len(tokens) == 2, LiquidityPoolError(
                "Expected exactly two tokens"
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
                assert self.token0.address == self._brownie_contract.token0()
                assert self.token1.address == self._brownie_contract.token1()
            else:
                self.token0 = Erc20Token(self._brownie_contract.token0())
                self.token1 = Erc20Token(self._brownie_contract.token1())

            self.fee = self._brownie_contract.fee()  # immutable
            self.slot0 = self._brownie_contract.slot0(
                block_identifier=block_number
            )
            self.liquidity = self._brownie_contract.liquidity(
                block_identifier=block_number
            )
            self.tick_spacing = (
                self._brownie_contract.tickSpacing()
            )  # immutable
            self.sqrt_price_x96 = self.slot0[0]
            self.tick = self.slot0[1]
            self.tick_data = {}
            self.tick_bitmap = {}
            self.tick_words = {}
            if populate_ticks:
                _tick_word, _ = self._get_tick_bitmap_position(self.tick)
                self._get_tick_data_at_word(_tick_word)

        except:
            raise

        self._update_method = update_method

        if name:
            self.name = name
        else:
            self.name = f"{self.token0.symbol}-{self.token1.symbol} (V3, {self.fee/10000:.2f}%)"

        self.state = {
            "liquidity": self.liquidity,
            "sqrt_price_x96": self.sqrt_price_x96,
            "tick": self.tick,
        }

        self.update_block = block_number

    def __str__(self):
        """
        Return the pool name when the object is included in a print statement, or cast as a string
        """
        return self.name

    def __UniswapV3Pool_swap(
        self,
        zeroForOne: bool,
        amountSpecified: int,
        sqrtPriceLimitX96: int,
    ) -> Tuple[int, int]:

        """
        This function is ported and adapted from the UniswapV3Pool.sol contract
        at https://github.com/Uniswap/v3-core/blob/main/contracts/UniswapV3Pool.sol

        It is called by the `calculate_tokens_in_from_tokens_out` and `calculate_tokens_out_from_tokens_in` methods to calculate
        swap amounts, ticks crossed, liquidity changes at various ticks, etc.

        It is a double-underscore method and is thus obscured from external access (but still accessible if you know how).
        """

        # TODO: redefine all asserts as if checks that raise an `EVMRevertError`
        assert amountSpecified != 0, "AS"

        assert (
            sqrtPriceLimitX96 < self.slot0["sqrtPriceX96"]
            and sqrtPriceLimitX96 > TickMath.MIN_SQRT_RATIO
            if zeroForOne
            else sqrtPriceLimitX96 > self.slot0["sqrtPriceX96"]
            and sqrtPriceLimitX96 < TickMath.MAX_SQRT_RATIO
        ), EVMRevertError("SPL")

        cache = {
            "liquidityStart": self.liquidity,
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
            "sqrtPriceX96": self.slot0["sqrtPriceX96"],
            "tick": self.slot0["tick"],
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
                except TickBitmap.BitmapWordUnavailable as e:
                    wordPos = e.args[-1]
                    # print(f"TickBitmap word missing! Fetching word {wordPos}")
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

                    liquidityNet, _ = self.tick_data[step["tickNext"]]

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

        return amount0, amount1

    def auto_update(
        self,
        silent: bool = True,
        block_number: int = None,
    ) -> Tuple[bool, dict]:
        """
        Retrieves the current slot0 and liquidity values from the LP, stores any that have changed,
        and returns a tuple with a status boolean indicating whether any update was found,
        and a dictionary holding current state values:
            - liquidity
            - sqrt_price_x96
            - tick
        """

        updated = False

        # use the block_number if provided, otherwise pull from Brownie
        if not block_number:
            block_number = chain.height

        # only process calls if the submitted block number (or retrieved block number)
        # is equal to or exceeds the block number of the last update
        if block_number >= self.update_block:
            try:
                if (
                    slot0 := self._brownie_contract.slot0(
                        block_identifier=block_number,
                    )
                ) != self.slot0:
                    updated = True
                    self.slot0 = slot0
                    self.sqrt_price_x96 = slot0[0]
                    self.tick = slot0[1]
                if (
                    liquidity := self._brownie_contract.liquidity(
                        block_identifier=block_number
                    )
                ) != self.liquidity:
                    updated = True
                    self.liquidity = liquidity
            except:
                raise
            else:
                self.update_block = block_number
                if not silent:
                    print(f"Liquidity: {self.liquidity}")
                    print(f"SqrtPriceX96: {self.sqrt_price_x96}")
                    print(f"Tick: {self.tick}")
                if updated:
                    self.state = {
                        "liquidity": self.liquidity,
                        "sqrt_price_x96": self.sqrt_price_x96,
                        "tick": self.tick,
                    }

        return updated, self.state

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int = None,
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

        Note that this wrapper function always assumes that the sqrt_price_limitx96 argument is unset, thus the
        swap calculation will continue until the target amount is satisfied, regardless of price impact

        Some swaps cannot consume the entire input amount.
        """

        # TODO: adjust return so a delta is returned as the second parameter. e.g. attempting to swap 1000 tokens
        # but only 999 are consumed by the swap, a delta of 1 is returned as the second value.

        if token_in not in (self.token0, self.token1):
            raise LiquidityPoolError("token_in not found!")

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_in == self.token0 else False

        try:
            # delegate calculations to the ported `swap` function
            amount0, amount1 = self.__UniswapV3Pool_swap(
                zeroForOne=zeroForOne,
                amountSpecified=token_in_quantity,
                sqrtPriceLimitX96=(
                    TickMath.MIN_SQRT_RATIO + 1
                    if zeroForOne
                    else TickMath.MAX_SQRT_RATIO - 1
                ),
            )
        except EVMRevertError:
            # TODO: better define actions for this exception
            raise
            print(f"type={type(e)}")
            raise EVMRevertError(
                f"(V3LiquidityPool) caught exception inside LP helper {self.name}: {e}"
                # f"\ntoken_in={token_in}"
                # f"\ntoken_in_quantity={token_in_quantity}"
            )
        except:
            raise
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

            return -amount1 if zeroForOne else -amount0

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int = None,
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
        """

        if token_out not in (self.token0, self.token1):
            raise LiquidityPoolError("token_in not found!")

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_out == self.token1 else False

        # delegate calculations to the re-implemented `swap` function
        amount0Delta, amount1Delta = self.__UniswapV3Pool_swap(
            zeroForOne=zeroForOne,
            amountSpecified=-token_out_quantity,
            sqrtPriceLimitX96=(
                TickMath.MIN_SQRT_RATIO + 1
                if zeroForOne
                else TickMath.MAX_SQRT_RATIO - 1
            ),
        )
        amountIn, amountOutReceived = (
            (uint256(amount0Delta), uint256(-amount1Delta))
            if zeroForOne
            else (uint256(amount1Delta), uint256(-amount0Delta))
        )
        return amountIn

    def external_update(
        self, updates: dict, block_number: int = None, silent: bool = True
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
        """

        assert set(
            ["liquidity", "sqrt_price_x96", "tick", "liquidity_change"]
        ) & set(
            updates.keys()
        ), "At least one of (liquidity, sqrt_price_x96, tick, liquidity_change) must be provided"

        if block_number is not None and block_number < self.update_block:
            raise ExternalUpdateError(
                f"Current state recorded at block {self.update_block}, received update for stale block {updates.get('block_number')}"
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
                # TODO: flag for liquidity changes in `self.state` that are not in-range
                # (might result in profitable arbs that cross ticks)
                liquidity_delta, lower_tick, upper_tick = value
                if lower_tick <= self.tick <= upper_tick:
                    prev_liq = self.liquidity
                    self.liquidity += liquidity_delta
                    new_liq = self.liquidity
                    print(f"In-range liquidity: was {prev_liq}, now {new_liq}")
                    self.state["liquidity"] = self.liquidity
                    updated = True

                words_fetched = []  # track any word fetched during the loop
                for i, tick in enumerate([lower_tick, upper_tick]):

                    tick_word, _ = self._get_tick_bitmap_position(tick)

                    # check if the word containing this tick has not been fetched, and fetch if not
                    # NOTE: the word is added to `words_fetched` so it can be skipped later if the other tick is
                    # in the same word (since the fetched values include the liquidity changes from this event)
                    if not (self.tick_words.get(tick_word)):
                        print(f"word {tick_word} missing, fetching...")
                        self._get_tick_data_at_word(
                            tick_word,
                            single_tick=True,
                            block_number=block_number,
                        )
                        words_fetched.append(tick_word)
                    else:
                        if tick_word in words_fetched:
                            print(
                                f"skipping tick ({tick}), already fetched word ({tick_word})"
                            )
                            continue

                        # get the liquidity info for this tick, or set to zero if previously uninitialized
                        if tick_liquidity := self.tick_data.get(tick):
                            (
                                tick_liquidity_net,
                                tick_liquidity_gross,
                            ) = tick_liquidity
                        else:
                            print(f"found uninitialized tick: {tick}")
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
                            del self.tick_data[tick]
                            print(f"Tick {tick} cleared")
                            continue
                        else:
                            self.tick_data[tick] = (
                                new_liquidity_net,
                                new_liquidity_gross,
                            )
                        # print("tick data after")
                        # print(pool_helper.tick_data.get(tick))

                updated = True

        if updated:
            self.update_block = (
                block_number
                if block_number and block_number > self.update_block
                else chain.height
            )
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
        single_tick: bool = False,
        block_number: int = None,
    ) -> dict:
        """
        Gets the initialized tick values at a specific word (a 32 byte number
        representing 256 ticks at the tickSpacing interval), stores
        the liquidity values in the `self.tick_data` dictionary using the tick
        as the key, and updates the tick_bitmap and tick_words dict.
        """

        if not block_number:
            block_number = chain.height

        # check if multicall is available for the connected network
        if (
            network.main.CONFIG.active_network.get("multicall2")
            and not single_tick
        ):
            # TODO: make extra_words value configurable (constructor argument?)
            extra_words = 50
            if not self.tick_words:

                # empty tick_words, so just fetch the requested word
                lower_word = word_position
                upper_word = word_position + 1
            else:
                # determine the direction of the requested word
                if word_position > (max_word := max(self.tick_words.keys())):
                    # going up
                    lower_word = max_word + 1
                    upper_word = lower_word + extra_words
                elif word_position < (min_word := min(self.tick_words.keys())):
                    # going down
                    lower_word = min_word - extra_words
                    upper_word = min_word
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
            else:
                # print(f"multicall tick_bitmap = {multicall_tick_bitmaps}")
                self.tick_bitmap.update(multicall_tick_bitmaps)
                for _word, _ in multicall_tick_bitmaps.items():
                    # TODO: remove tick_words (redundant)
                    self.tick_words.update({_word: True})

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
            else:
                # print(f"multicall tick_data = {multicall_tick_data}")
                self.tick_data.update(multicall_tick_data)

        else:
            # fetch ticks one by one
            try:
                if tick_bitmap := self._brownie_contract.tickBitmap(
                    word_position
                ):
                    tick_data = (
                        self.lens._brownie_contract.getPopulatedTicksInWord(
                            self.address,
                            word_position,
                            block_identifier=block_number,
                        )
                    )
                else:
                    tick_data = ()
            except:
                raise
            else:
                if tick_bitmap:
                    for (tick, liquidityNet, liquidityGross) in tick_data:
                        self.tick_data[tick] = liquidityNet, liquidityGross
                self.tick_bitmap.update({word_position: tick_bitmap})
                self.tick_words.update({word_position: True})

        return self.tick_data


class V3LiquidityPool(BaseV3LiquidityPool):
    def _derived():
        pass
