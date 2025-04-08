# ruff: noqa: A005

import dataclasses

from eth_typing import BlockNumber, ChecksumAddress
from hexbytes import HexBytes

from degenbot.erc20_token import Erc20Token


class AbstractSwapAmounts:
    pass


@dataclasses.dataclass(slots=True, frozen=True)
class ArbitrageCalculationResult:
    id: str
    input_token: Erc20Token
    profit_token: Erc20Token
    input_amount: int
    profit_amount: int
    swap_amounts: tuple[AbstractSwapAmounts, ...]
    state_block: BlockNumber | None

    def __post_init__(self) -> None:
        assert self.input_amount != 0


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolSwapAmounts(AbstractSwapAmounts):
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
class UniswapV2PoolSwapAmounts(AbstractSwapAmounts):
    pool: ChecksumAddress
    amounts_in: tuple[int, int]
    amounts_out: tuple[int, int]
    recipient: ChecksumAddress | None = None

    def __post_init__(self) -> None:
        assert self.amounts_in != (0, 0)
        assert self.amounts_out != (0, 0)
        assert 0 in self.amounts_in
        assert 0 in self.amounts_out


@dataclasses.dataclass(slots=True)
class UniswapV3PoolSwapAmounts(AbstractSwapAmounts):
    pool: ChecksumAddress
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int
    recipient: ChecksumAddress | None = None

    def __post_init__(self) -> None:
        assert self.amount_specified != 0


@dataclasses.dataclass(slots=True)
class UniswapV4PoolSwapAmounts(AbstractSwapAmounts):
    address: ChecksumAddress
    id: HexBytes
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int
    recipient: ChecksumAddress | None = None

    def __post_init__(self) -> None:
        assert self.amount_specified != 0


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolVector:
    token_in: Erc20Token
    token_out: Erc20Token

    def __post_init__(self) -> None:
        assert self.token_in != self.token_out
