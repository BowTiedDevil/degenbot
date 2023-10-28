import dataclasses
from typing import List, Tuple

from ..erc20_token import Erc20Token


@dataclasses.dataclass(slots=True, frozen=True)
class ArbitrageCalculationResult:
    id: str
    input_token: Erc20Token
    profit_token: Erc20Token
    input_amount: int
    profit_amount: int
    swap_amounts: List


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapPoolSwapVector:
    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolSwapAmounts:
    amounts: Tuple[int, int]


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolSwapAmounts:
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int
