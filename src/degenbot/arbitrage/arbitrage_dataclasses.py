import dataclasses
from typing import Any, List, Tuple

from ..erc20_token import Erc20Token


@dataclasses.dataclass(slots=True, frozen=True)
class ArbitrageCalculationResult:
    id: str
    input_token: Erc20Token
    profit_token: Erc20Token
    input_amount: int
    profit_amount: int
    swap_amounts: List[Any]


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolSwapAmounts:
    token_in: Erc20Token
    token_in_index: int
    token_out: Erc20Token
    token_out_index: int
    amount_in: int
    min_amount_out: int
    underlying: bool


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapPoolSwapVector:
    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolSwapAmounts:
    amounts: Tuple[int, int]
    amounts_in: Tuple[int, int] | None = None


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolSwapAmounts:
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolVector:
    token_in: Erc20Token
    token_out: Erc20Token
