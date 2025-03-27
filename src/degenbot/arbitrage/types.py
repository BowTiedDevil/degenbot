# ruff: noqa: A005

import dataclasses
from typing import Any

from eth_typing import BlockNumber, ChecksumAddress

from degenbot.erc20_token import Erc20Token


@dataclasses.dataclass(slots=True, frozen=True)
class ArbitrageCalculationResult:
    id: str
    input_token: Erc20Token
    profit_token: Erc20Token
    input_amount: int
    profit_amount: int
    swap_amounts: list[Any]
    state_block: BlockNumber | None


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolSwapAmounts:
    token_in: Erc20Token
    token_in_index: int
    token_out: Erc20Token
    token_out_index: int
    amount_in: int
    min_amount_out: int
    underlying: bool

    def __post_init__(self) -> None:
        assert self.token_in != self.token_out


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapPoolSwapVector:
    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool

    def __post_init__(self) -> None:
        assert self.token_in != self.token_out


@dataclasses.dataclass(slots=True)
class UniswapV2PoolSwapAmounts:
    pool: ChecksumAddress
    amounts_in: tuple[int, int]
    amounts_out: tuple[int, int]
    recipient: ChecksumAddress | None = None


@dataclasses.dataclass(slots=True)
class UniswapV3PoolSwapAmounts:
    pool: ChecksumAddress
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int
    recipient: ChecksumAddress | None = None


@dataclasses.dataclass(slots=True)
class UniswapV4PoolSwapAmounts:
    pool: ChecksumAddress
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int
    recipient: ChecksumAddress | None = None


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolVector:
    token_in: Erc20Token
    token_out: Erc20Token

    def __post_init__(self) -> None:
        assert self.token_in != self.token_out
