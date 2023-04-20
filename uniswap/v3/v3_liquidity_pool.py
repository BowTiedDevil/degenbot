from abc import ABC, abstractmethod
from decimal import Decimal
from threading import Lock
from typing import List, Optional, Tuple

from brownie import Contract, chain, multicall, network
from web3 import Web3

from degenbot.exceptions import (
    ArbitrageError,
    BitmapWordUnavailableError,
    BlockUnavailableError,
    EVMRevertError,
    ExternalUpdateError,
    LiquidityPoolError,
)
from degenbot.manager.token_manager import Erc20TokenHelperManager
from degenbot.token import Erc20Token
from degenbot.uniswap.v3.abi import UNISWAP_V3_POOL_ABI
from degenbot.uniswap.v3.libraries import (
    LiquidityMath,
    SwapMath,
    TickBitmap,
    TickMath,
)
from degenbot.uniswap.v3.libraries.Helpers import *
from degenbot.uniswap.v3.tick_lens import TickLens


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
        lens: Optional[Contract] = None,
        tokens: Optional[List[Erc20Token]] = None,
        name: str = "",
        update_method: str = "polling",
        abi: Optional[list] = None,
        extra_words: int = 10,
        silent: bool = False,
        tick_data: dict = None,
        tick_bitmap: dict = None,
    ):

        self.tick_data: dict
        self.tick_bitmap: dict

        self._token_manager = Erc20TokenHelperManager(chain.id)

        # held by the _get_tick_data_at_word method, which will retrieve
        # and store liquidity and bitmap data
        self.tick_lock = Lock()

        # held by the auto_update and external_update method, which will
        # retrieve and store mutable state data (liquidity, tick, sqrtPrice, etc)
        self.update_lock = Lock()

        self.update_block = chain.height

        self.uniswap_version = 3

        if tokens is not None:
            if len(tokens) != 2:
                raise ValueError(
                    f"Expected exactly two tokens, found {len(tokens)}"
                )

        self.address = Web3.toChecksumAddress(address)

        if abi is None:
            abi = UNISWAP_V3_POOL_ABI

        self._brownie_contract = Contract.from_abi(
            name="", address=address, abi=abi
        )

        if lens:
            self.lens = lens
        else:
            self.lens = TickLens()

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
        else:
            self.token0 = self._token_manager.get_erc20token(
                address=self._brownie_contract.token0(),
                min_abi=True,
                silent=True,
                unload_brownie_contract_after_init=True,
            )
            self.token1 = self._token_manager.get_erc20token(
                address=self._brownie_contract.token1(),
                min_abi=True,
                silent=True,
                unload_brownie_contract_after_init=True,
            )

        self.fee = self._brownie_contract.fee()  # immutable

        # check that the address is a valid V3 pool (see https://github.com/Uniswap/v3-periphery/blob/main/contracts/libraries/PoolAddress.sol)
        FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        POOL_INIT_HASH = "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"

        computed_pool_address = Web3.toChecksumAddress(
            Web3.keccak(
                hexstr="0xff"
                + FACTORY[2:]
                + Web3.keccak(
                    eth_abi.encode(
                        ["address", "address", "uint24"],
                        [self.token0.address, self.token1.address, self.fee],
                    )
                ).hex()[2:]
                + POOL_INIT_HASH[2:]
            )[12:]
        )
        if computed_pool_address != address:
            raise ValueError(
                f"Pool address {address} does not match deterministic address {computed_pool_address} from factory"
            )

        self.liquidity = self._brownie_contract.liquidity(
            block_identifier=self.update_block
        )
        self.tick_spacing = self._brownie_contract.tickSpacing()  # immutable
        slot0 = self._brownie_contract.slot0(
            block_identifier=self.update_block
        )
        self.sqrt_price_x96 = slot0[0]
        self.tick = slot0[1]

        self._update_method = update_method
        self.extra_words = extra_words

        # default to a sparse bitmap
        self.tick_bitmap = {"sparse": True}

        if tick_bitmap is not None:
            self.tick_bitmap.update(tick_bitmap)

            # self.tick_bitmap.update(
            #     {
            #         word: {
            #             "bitmap": 0,
            #             "block": None,
            #         }
            #         for word in (
            #             set(range(MIN_INT16, MAX_INT16 + 1))
            #             - set(self.tick_bitmap.keys())
            #         )
            #     }
            # )

            # if a snapshot was provided, assume it is complete (sparse=False)
            self.tick_bitmap["sparse"] = False

        if tick_data is not None:
            self.tick_data = tick_data
        else:
            word_position, _ = self._get_tick_bitmap_position(self.tick)
            self.tick_data = {}
            self._update_tick_data_at_word(
                word_position,
                single_word=True,
                block_number=self.update_block,
            )

        if name:
            self.name = name
        else:
            self.name = (
                f"{self.token0}-{self.token1} (V3, {self.fee/10000:.2f}%)"
            )

        self.state = {}
        self._update_pool_state()

        if not silent:
            print(self.name)
            print(f"• Token 0: {self.token0}")
            print(f"• Token 1: {self.token1}")
            print(f"• Liquidity: {self.liquidity}")
            print(f"• SqrtPrice: {self.sqrt_price_x96}")
            print(f"• Tick: {self.tick}")

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
        self.state = {
            "last_liquidity_update": self.liquidity_update_block,
            "liquidity": self.liquidity,
            "sqrt_price_x96": self.sqrt_price_x96,
            "tick": self.tick,
        }

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

        if word_position in self.tick_bitmap.keys():
            print(f"returning early, {word_position=} found")
            print(self.tick_bitmap[word_position])
            return

        # print(f"updating tick data for pool: {self.name}")

        with self.tick_lock:

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
                ) - set(self.tick_bitmap.keys())

                # print(f"fetching words: {words}")

                # fetch the tick bitmaps for the range
                try:
                    with multicall(block_identifier=block_number):
                        multicall_tick_bitmaps = {
                            word: {
                                "bitmap": self._brownie_contract.tickBitmap(
                                    word
                                ),
                                "block": block_number,
                            }
                            for word in words
                        }
                    with multicall(block_identifier=block_number):
                        multicall_tick_data = {
                            tick: {
                                "liquidityNet": liquidityNet,
                                "liquidityGross": liquidityGross,
                                "block": block_number,
                            }
                            for word_position, bitmap in multicall_tick_bitmaps.items()
                            for tick, liquidityNet, liquidityGross in self.lens._brownie_contract.getPopulatedTicksInWord(
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
                    # update the bitmaps
                    self.tick_bitmap.update(multicall_tick_bitmaps)
                    # update the liquidity data
                    self.tick_data.update(multicall_tick_data)

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
                    self.tick_bitmap[word_position] = {
                        "bitmap": single_tick_bitmap,
                        "block": block_number,
                    }
                    if single_tick_bitmap:
                        for (
                            tick,
                            liquidity_net,
                            liquidity_gross,
                        ) in single_tick_data:
                            self.tick_data[tick] = {
                                "liquidityNet": liquidity_net,
                                "liquidityGross": liquidity_gross,
                                "block": block_number,
                            }

    def _update_pool_state(self) -> None:
        self.state = {
            "liquidity": self.liquidity,
            "sqrt_price_x96": self.sqrt_price_x96,
            "tick": self.tick,
        }

    def __UniswapV3Pool_swap(
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

        It is a double-underscore method and is thus obscured from external access (but still accessible if you know how).
        """

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

        cache = {
            "liquidityStart": liquidity,
            "tickCumulative": 0,
            # ignored attributes:
            #   - blockTimestamp
            #   - feeProtocol
            #   - secondsPerLiquidityCumulativeX128
            #   - computedLatestObservation
        }

        exactInput: bool = amount_specified > 0

        state = {
            "amountSpecifiedRemaining": amount_specified,
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
            and state["sqrtPriceX96"] != sqrt_price_limit_x96
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
                    missing_word = e.args[-1]
                    if self.tick_bitmap["sparse"]:
                        # BUG: 'word_position=XXX inside known range' exception is being thrown here
                        # when the helper is being updated by multiple threads
                        # print(f"(swap) {self.name} fetching word {wordPos}")
                        self._update_tick_data_at_word(
                            missing_word,
                            # single_word=True,
                        )
                    else:
                        self.tick_bitmap[missing_word] = {
                            "bitmap": 0,
                            "block": None,
                        }
                else:
                    # nextInitializedTickWithinOneWord will search up to 256 ticks away, which may
                    # return a tick in an adjacent word if there are no initialized ticks in the current word.
                    # This word may not be known to the helper, so check and fetch the containing word for this tick

                    # BUGFIX: previously called position directly, which implies tickSpacing=1,
                    # so the call returned an inaccurate word and short-circuited the optimization
                    tick_next_word, _ = self._get_tick_bitmap_position(
                        step["tickNext"]
                    )

                    if (
                        self.tick_bitmap["sparse"]
                        and tick_next_word not in self.tick_bitmap.keys()
                    ):
                        # print(
                        #     f'tickNext={step["tickNext"]} out of range! Fetching word={tick_next_word}'
                        #     f"\n{self.name}"
                        # )
                        self._update_tick_data_at_word(
                            tick_next_word,
                            single_word=True,
                        )
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
                sqrt_price_limit_x96
                if (
                    step["sqrtPriceNextX96"] < sqrt_price_limit_x96
                    if zeroForOne
                    else step["sqrtPriceNextX96"] > sqrt_price_limit_x96
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

                    # use the default value (0, 0) so the tuple assignment works. Throws exception
                    # if the default value None is returned from get()
                    # liquidityNet, liquidityGross = self.tick_data.get(
                    #     step["tickNext"],
                    #     (0, 0),
                    # )

                    try:
                        # liquidityNet, liquidityGross = self.tick_data[
                        #     step["tickNext"]
                        # ]
                        liquidityNet = self.tick_data[step["tickNext"]][
                            "liquidityNet"
                        ]
                        liquidityGross = self.tick_data[step["tickNext"]][
                            "liquidityGross"
                        ]
                    except KeyError:
                        # current_tick_word, _ = self._get_tick_bitmap_position(
                        #     state["tick"]
                        # )
                        # next_tick_word, _ = self._get_tick_bitmap_position(
                        #     step["tickNext"]
                        # )
                        raise ArbitrageError(
                            "Tick bitmap or liquidity data is out of date"
                            # f"(UniswapLpCycle) swap function indicated tick={step['tickNext']} was initialized, but tick_data has no data for this tick!"
                            # f"\nPool address = {self.address}"
                            # f"\nCurrent: Tick={state['tick']}, Word={current_tick_word}"
                            # f"\nNext   : Tick={step['tickNext']}, Word={next_tick_word}"
                            # f"\nBlock  : {chain.height}"
                        ) from None

                    # if (liquidityNet, liquidityGross) == (0, 0):
                    #     current_tick_word, _ = self._get_tick_bitmap_position(
                    #         state["tick"]
                    #     )
                    #     next_tick_word, _ = self._get_tick_bitmap_position(
                    #         step["tickNext"]
                    #     )
                    #     # WIP: investigate if this error is thrown when the next tick is on a word boundary
                    #     raise ArbitrageError(
                    #         f"(UniswapLpCycle) swap function indicated tick={step['tickNext']} was initialized, but tick_data has no data for this tick!"
                    #         f"\nPool address = {self.address}"
                    #         f"\nCurrent: Tick={state['tick']}, Word={current_tick_word}"
                    #         f"\nNext   : Tick={step['tickNext']}, Word={next_tick_word}"
                    #         f"\nBlock  : {chain.height}"
                    #     )

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
                amount_specified - state["amountSpecifiedRemaining"],
                state["amountCalculated"],
            )
            if zeroForOne == exactInput
            else (
                state["amountCalculated"],
                amount_specified - state["amountSpecifiedRemaining"],
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

        with self.update_lock:

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
                print(f"Liquidity: {self.liquidity}")
                print(f"SqrtPriceX96: {self.sqrt_price_x96}")
                print(f"Tick: {self.tick}")

            # WORKAROUND: update the block even if there are no state changes
            # pools were being repeatedly caught by "stale pool" checks
            self.update_block = block_number

        return updated, self.state

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: Optional[dict] = None,
        with_remainder: bool = False,
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

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_in == self.token0 else False

        if override_state is None:
            override_state = {}

        try:
            # delegate calculations to the ported `swap` function
            (amount0_delta, amount1_delta, *_,) = self.__UniswapV3Pool_swap(
                zeroForOne=zeroForOne,
                amount_specified=token_in_quantity,
                sqrt_price_limit_x96=(
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
            raise ValueError("token_in not found!")

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_out == self.token1 else False

        if override_state is None:
            override_state = {}

        try:
            # delegate calculations to the ported `swap` function
            (amount0_delta, amount1_delta, *_,) = self.__UniswapV3Pool_swap(
                zeroForOne=zeroForOne,
                amount_specified=-token_out_quantity,
                sqrt_price_limit_x96=(
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
            raise LiquidityPoolError(
                f"Simulated execution reverted: {e}"
            ) from e
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
        fetch_missing: bool = True,
        force: bool = False,  # added primarily to support liquidity bootstrapping without excessive refactoring
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

        Uses a lock to guard state-modifying methods that might cause race conditions when used with threads.
        """

        # if we have supplied a full snapshot during loading, disable the fetching mechanism
        if not self.tick_bitmap["sparse"]:
            fetch_missing = False

        if not (
            set(["liquidity", "sqrt_price_x96", "tick", "liquidity_change"])
            & set(updates.keys())
        ):
            raise ValueError(
                "At least one of (liquidity, sqrt_price_x96, tick, liquidity_change) must be provided"
            )

        # if block_number was not provided, pull from Brownie
        if block_number is None:
            block_number = chain.height
            print(
                f"(V3LiquidityPool.external_update) block_number was not provided, using {block_number} from chain"
            )

        # Check if submitted block number is less than the recorded block and raise error if so.
        # This allows same-block updates since multiple state-changing events may occur sequentially in a block
        if block_number < self.update_block and not force:
            raise ExternalUpdateError(
                f"Current state recorded at block {self.update_block}, received update for stale block {block_number}"
            )

        for key in updates.keys():
            if key not in [
                "tick",
                "liquidity",
                "sqrt_price_x96",
                "liquidity_change",
            ]:
                print(f"Unknown key-value pair ({key}:{updates[key]})")

        with self.update_lock:

            updated_state = False

            # TODO: deal separately with helper-level update_block attribute OR update_block inside tick_data and tick_bitmap

            # IMPROVEMENT: replaced unnecessary loop in favor of specific updates
            # for key, value in updates.items():
            #     if key == "tick":
            #         self.tick = value
            #         updated_state = True
            #     elif key == "liquidity":
            #         self.liquidity = value
            #         updated_state = True
            #     elif key == "sqrt_price_x96":
            #         self.sqrt_price_x96 = value
            #         updated_state = True
            #     elif key == "liquidity_change":

            try:
                self.tick = updates["tick"]
            except KeyError:
                pass
            else:
                updated_state = True

            try:
                self.liquidity = updates["liquidity"]
            except KeyError:
                pass
            else:
                updated_state = True

            try:
                self.sqrt_price_x96 = updates["sqrt_price_x96"]
            except KeyError:
                pass
            else:
                updated_state = True

            try:
                liquidity_change = updates["liquidity_change"]
            except KeyError:
                pass
            else:
                liquidity_delta, lower_tick, upper_tick = liquidity_change

                with self.tick_lock:

                    # Mint/Burn events may affect the current liquidity if the current tick is
                    # in the tick range associated with this event, so check and adjust
                    if lower_tick <= self.tick <= upper_tick:
                        self.liquidity += liquidity_delta
                        updated_state = True

                    for i, tick in enumerate([lower_tick, upper_tick]):

                        tick_word, _ = self._get_tick_bitmap_position(tick)

                        if tick_word not in self.tick_bitmap.keys():

                            # the tick bitmap must be available for the word prior to flipping
                            # the initialized status of any tick

                            if fetch_missing:
                                # print(
                                #     f"(external_update) {tick_word=} not found in tick_bitmap {self.tick_bitmap.keys()=}"
                                # )
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
                                self.tick_bitmap[tick_word] = {
                                    "bitmap": 0,
                                    "block": None,
                                }

                            # Get the liquidity info for this tick. get() returns None if the key
                            # is not found, which indicates that the tick was uninitialized
                            # liquidity_at_tick: dict = self.tick_data.get(tick)

                        try:
                            tick_liquidity_net = self.tick_data[tick][
                                "liquidityNet"
                            ]
                            tick_liquidity_gross = self.tick_data[tick][
                                "liquidityGross"
                            ]
                        except KeyError:
                            TickBitmap.flipTick(
                                self.tick_bitmap,
                                tick,
                                self.tick_spacing,
                                update_block=block_number,
                            )
                            tick_liquidity_net = 0
                            tick_liquidity_gross = 0

                        # if liquidity_at_tick is None:
                        #     TickBitmap.flipTick(
                        #         self.tick_bitmap,
                        #         tick,
                        #         self.tick_spacing,
                        #         update_block=block_number,
                        #     )
                        #     tick_liquidity_net = 0
                        #     tick_liquidity_gross = 0
                        # else:
                        #     tick_liquidity_net = liquidity_at_tick[
                        #         "liquidityNet"
                        #     ]
                        #     tick_liquidity_gross = liquidity_at_tick[
                        #         "liquidityGross"
                        #     ]

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

                        # Delete entirely if there is no liquidity referencing this tick
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
                            self.tick_data[tick] = {
                                "liquidityNet": new_liquidity_net,
                                "liquidityGross": new_liquidity_gross,
                                "block": block_number,
                            }

                updated_state = True

            if not silent:
                print(f"Liquidity: {self.liquidity}")
                print(f"SqrtPriceX96: {self.sqrt_price_x96}")
                print(f"Tick: {self.tick}")
                print(
                    f"liquidity event: {liquidity_delta} in tick range [{lower_tick},{upper_tick}], pool: {self.name}"
                )

            if updated_state:
                self.update_block = block_number
                self.state.update(
                    {
                        "tick": self.tick,
                        "liquidity": self.liquidity,
                        "sqrt_price_x96": self.sqrt_price_x96,
                    }
                )

            return updated_state

    def simulate_swap(
        self,
        token_in: Optional[Erc20Token] = None,
        token_in_quantity: Optional[int] = None,
        token_out: Optional[Erc20Token] = None,
        token_out_quantity: Optional[int] = None,
        sqrt_price_limit: Optional[int] = None,
        override_state: Optional[dict] = None,
    ) -> dict:
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
            raise ValueError("token_in not found!")
        if token_out and token_out not in (self.token0, self.token1):
            raise ValueError("token_out not found!")

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
                amount_specified=token_in_quantity
                if token_in_quantity
                else -token_out_quantity,
                sqrt_price_limit_x96=sqrt_price_limit
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
            raise LiquidityPoolError(
                f"Simulated execution reverted: {e}"
            ) from e
        else:
            return {
                "amount0_delta": amount0_delta,
                "amount1_delta": amount1_delta,
                "liquidity": end_liquidity,
                "sqrt_price_x96": end_sqrtprice,
                "tick": end_tick,
            }


class V3LiquidityPool(BaseV3LiquidityPool):
    def _derived():
        pass
