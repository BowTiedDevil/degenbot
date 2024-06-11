import dataclasses
from typing import Any, List, Tuple

from eth_typing import ChecksumAddress

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


@dataclasses.dataclass(slots=True)
class UniswapV2PoolSwapAmounts:
    pool: ChecksumAddress
    amounts_in: Tuple[int, int]
    amounts_out: Tuple[int, int]
    recipient: ChecksumAddress | None = None


@dataclasses.dataclass(slots=True)
class UniswapV3PoolSwapAmounts:
    pool: ChecksumAddress
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int
    recipient: ChecksumAddress | None = None


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolVector:
    token_in: Erc20Token
    token_out: Erc20Token
