from abc import ABC, abstractmethod
from copy import deepcopy
from typing import Tuple

from brownie import Contract
from brownie.convert import to_address
from degenbot import Erc20Token
from degenbot.exceptions import DegenbotError

from .abi import V3_LP_ABI
from .libraries import (
    LiquidityMath,
    TickBitmap,
    TickMath,
    SwapMath,
)
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

    def __init__(self, address: str, lens: Contract = None):
        self.address = to_address(address)

        try:
            self._brownie_contract = Contract(address=address)
        except:
            try:
                self._brownie_contract = Contract.from_explorer(
                    address=address, silent=True
                )
            except:
                try:
                    self._brownie_contract = Contract.from_abi(
                        name="", address=address, abi=V3_LP_ABI
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
            self.token0 = self._brownie_contract.token0()
            self.token1 = self._brownie_contract.token1()
            self.fee = self._brownie_contract.fee()
            self.slot0 = self._brownie_contract.slot0()
            self.liquidity = self._brownie_contract.liquidity()
            self.tick_spacing = self._brownie_contract.tickSpacing()
            self.sqrt_price_x96 = self.slot0[0]
            self.tick = self.slot0[1]
            self.factory = self._brownie_contract.factory()
            self.tick_data = {}
            self.tick_word, _ = self.get_tick_bitmap_position(self.tick)
            self.tick_bitmap = {}
            self.get_tick_data_at_word(self.tick_word)
        except:
            raise

    def auto_update(self):
        """
        Retrieves the current slot0 and liquidity values from the LP,
        stores any that have changed, and returns a tuple with an update status
        boolean and a dictionary holding the current mutable values:
            - liquidity
            - sqrt_price_x96
            - tick
        """
        updated = False
        try:
            if (slot0 := self._brownie_contract.slot0()) != self.slot0:
                updated = True
                self.slot0 = slot0
                self.sqrt_price_x96 = self.slot0[0]
                self.tick = self.slot0[1]
            if (
                liquidity := self._brownie_contract.liquidity()
            ) != self.liquidity:
                updated = True
                self.liquidity = liquidity
        except:
            raise
        else:
            return updated, {
                "liquidity": self.liquidity,
                "sqrt_price_x96": self.sqrt_price_x96,
                "tick": self.tick,
            }

    def get_tick_bitmap_position(self, tick) -> Tuple[int, int]:
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
        return TickBitmap.position(tick // self.tick_spacing)

    def get_tick_data_at_word(self, word_position: int):
        """
        Gets the initialized tick values at a specific word (a 32 byte number
        representing 256 ticks at the tickSpacing interval), stores
        the liquidity values in the `self.tick_data` dictionary using the tick
        as the key, and updates the tick_bitmap dict.
        """
        try:
            tick_data = self.lens._brownie_contract.getPopulatedTicksInWord(
                self.address, word_position
            )
            self.tick_bitmap.update(
                {word_position: self._brownie_contract.tickBitmap(word_position)}
            )

        except:
            raise
        else:
            for (tick, liquidityNet, liquidityGross) in tick_data:
                self.tick_data[tick] = liquidityNet, liquidityGross
            return tick_data

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int = None,
    ):
        """
        This function implements the common degenbot interface `calculate_tokens_out_from_tokens_in`
        to calculate the number of tokens received (out) from a given number of tokens deposited (in).

        The UniswapV3 liquidity pool function `swap` is adapted from
        https://github.com/Uniswap/v3-core/blob/main/contracts/UniswapV3Pool.sol
        and used to calculate swap amounts, ticks crossed, liquidity changes at various ticks, etc.
        """

        def swap(
            zeroForOne: bool,
            amountSpecified: int,
            sqrtPriceLimitX96: int,
        ) -> Tuple[int, int]:

            slot0Start = deepcopy(self.slot0)

            cache = {
                "liquidityStart": self.liquidity,
                "tickCumulative": 0,
            }

            exactInput: bool = amountSpecified > 0

            state = {
                "amountSpecifiedRemaining": amountSpecified,
                "amountCalculated": 0,
                "sqrtPriceX96": slot0Start["sqrtPriceX96"],
                "tick": slot0Start["tick"],
                "liquidity": cache["liquidityStart"],
            }

            while (
                state["amountSpecifiedRemaining"] != 0
                and state["sqrtPriceX96"] != sqrtPriceLimitX96
            ):
                step = {}
                step["sqrtPriceStartX96"] = state["sqrtPriceX96"]

                (
                    step["tickNext"],
                    step["initialized"],
                ) = TickBitmap.nextInitializedTickWithinOneWord(
                    self.tick_bitmap,
                    state["tick"],
                    self.tick_spacing,
                    zeroForOne,
                )

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
                    state["amountSpecifiedRemaining"] -= (
                        step["amountIn"] + step["feeAmount"]
                    )
                    state["amountCalculated"] = (
                        state["amountCalculated"] - step["amountOut"]
                    )

                else:
                    state["amountSpecifiedRemaining"] += step["amountOut"]
                    state["amountCalculated"] = (
                        state["amountCalculated"]
                        + step["amountIn"]
                        + step["feeAmount"]
                    )

                if state["sqrtPriceX96"] == step["sqrtPriceNextX96"]:
                    # if the tick is initialized, run the tick transition
                    if step["initialized"]:
                        
                        print(step['tickNext'])
                        
                        liquidityNet, _ = self.tick_data[step["tickNext"]]
                        print(liquidityNet)

                        if zeroForOne:
                            liquidityNet = -liquidityNet

                        state["liquidity"] = LiquidityMath.addDelta(
                            state["liquidity"], liquidityNet
                        )

                    state["tick"] = (
                        step["tickNext"] - 1
                        if zeroForOne
                        else step["tickNext"]
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

        if token_in not in (self.token0, self.token1):
            raise DegenbotError("token_in not found!")

        # determine whether the swap is token0 -> token1
        zeroForOne = True if token_in == self.token0 else False

        # delegate calculations to the re-implemented `swap` function
        amount0, amount1 = swap(
            zeroForOne=zeroForOne,
            amountSpecified=token_in_quantity,
            sqrtPriceLimitX96=(
                TickMath.MIN_SQRT_RATIO + 1
                if zeroForOne
                else TickMath.MAX_SQRT_RATIO - 1
            ),
        )
        return -amount1 if zeroForOne else -amount0


class V3LiquidityPool(BaseV3LiquidityPool):
    def _derived():
        pass
