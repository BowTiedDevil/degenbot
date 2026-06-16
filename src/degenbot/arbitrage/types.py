"""Shared types for the arbitrage module."""

import dataclasses

import eth_abi.abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3

from degenbot.arbitrage.encoding import EncodedCall
from degenbot.erc20 import Erc20Token
from degenbot.types.aliases import BlockNumber


class AbstractSwapAmounts:
    """Base class for per-pool swap parameters and encoding."""

    def input_amount(self) -> int:
        """Return the input amount for this swap."""
        msg = f"{type(self).__name__} must implement input_amount()"
        raise NotImplementedError(msg)

    def output_amount(self) -> int:
        """Return the output amount for this swap."""
        msg = f"{type(self).__name__} must implement output_amount()"
        raise NotImplementedError(msg)

    def encode(self, *, recipient: ChecksumAddress) -> EncodedCall:
        """Encode this swap into an EVM call."""
        msg = f"{type(self).__name__} must implement encode()"
        raise NotImplementedError(msg)


@dataclasses.dataclass(slots=True, frozen=True)
class ArbitrageCalculationResult[SwapAmountType]:
    """The result of an arbitrage calculation containing profit details and swap amounts.

    This class is generic over the swap amount type. Calculations that build an
    instance should specify this type in the return annotation, e.g.:

    ```
    def calculate(
        self,
        state_overrides: Mapping[ChecksumAddress, UniswapV2PoolState],
        block_number: BlockNumber | None = None,
    ) -> ArbitrageCalculationResult[UniswapV2PoolSwapAmounts]: ...
    ```
    """

    id: str
    input_token: Erc20Token
    profit_token: Erc20Token
    input_amount: int
    profit_amount: int
    swap_amounts: tuple[SwapAmountType, ...]
    state_block: BlockNumber | None

    def __post_init__(self) -> None:
        """Post-initialization hook."""
        assert self.input_amount != 0


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolSwapAmounts(AbstractSwapAmounts):
    """CurveStableSwapPoolSwapAmounts class."""

    pool: ChecksumAddress
    token_in: Erc20Token
    token_in_index: int
    token_out: Erc20Token
    token_out_index: int
    amount_in: int
    min_amount_out: int
    underlying: bool

    def __post_init__(self) -> None:
        """Post-initialization hook."""
        assert self.token_in != self.token_out

    def input_amount(self) -> int:
        """Return input amount.

        Returns:
            The computed value.

        """
        return self.amount_in

    def output_amount(self) -> int:
        """Return output amount.

        Returns:
            The computed value.

        """
        return self.min_amount_out

    def encode(self, *, recipient: ChecksumAddress) -> EncodedCall:  # noqa: ARG002
        """Encode a Curve exchange() or exchange_underlying() call.

        Returns:
            The computed value.

        """
        if self.underlying:
            selector = Web3.keccak(text="exchange_underlying(int128,int128,uint256,uint256)")[:4]
        else:
            selector = Web3.keccak(text="exchange(int128,int128,uint256,uint256)")[:4]
        data = selector + eth_abi.abi.encode(
            types=["int128", "int128", "uint256", "uint256"],
            args=[
                self.token_in_index,
                self.token_out_index,
                self.amount_in,
                self.min_amount_out,
            ],
        )
        return EncodedCall(to=self.pool, data=data)


@dataclasses.dataclass(slots=True)
class UniswapV2PoolSwapAmounts(AbstractSwapAmounts):
    """UniswapV2PoolSwapAmounts class."""

    pool: ChecksumAddress
    amounts_in: tuple[int, int]
    amounts_out: tuple[int, int]
    recipient: ChecksumAddress | None = None

    def __post_init__(self) -> None:
        """Post-initialization hook."""
        assert self.amounts_in != (0, 0)
        assert self.amounts_out != (0, 0)
        assert 0 in self.amounts_in
        assert 0 in self.amounts_out

    def input_amount(self) -> int:
        """Return input amount.

        Returns:
            The computed value.

        """
        return max(self.amounts_in)

    def output_amount(self) -> int:
        """Return output amount.

        Returns:
            The computed value.

        """
        return max(self.amounts_out)

    def encode(self, *, recipient: ChecksumAddress) -> EncodedCall:
        """Encode a Uniswap V2 swap() call.

        Returns:
            The computed value.

        """
        selector = Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
        data = selector + eth_abi.abi.encode(
            types=["uint256", "uint256", "address", "bytes"],
            args=[*self.amounts_out, recipient, b""],
        )
        return EncodedCall(to=self.pool, data=data)


@dataclasses.dataclass(slots=True)
class UniswapV3PoolSwapAmounts(AbstractSwapAmounts):
    """UniswapV3PoolSwapAmounts class."""

    pool: ChecksumAddress
    amount_in: int
    amount_out: int
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int
    recipient: ChecksumAddress | None = None

    def __post_init__(self) -> None:
        """Post-initialization hook."""
        assert self.amount_specified != 0

    def input_amount(self) -> int:
        """Return input amount.

        Returns:
            The computed value.

        """
        return self.amount_in

    def output_amount(self) -> int:
        """Return output amount.

        Returns:
            The computed value.

        """
        return self.amount_out

    def encode(self, *, recipient: ChecksumAddress) -> EncodedCall:
        """Encode a Uniswap V3 swap() call.

        Returns:
            The computed value.

        """
        selector = Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
        data = selector + eth_abi.abi.encode(
            types=["address", "bool", "int256", "uint160", "bytes"],
            args=[
                recipient,
                self.zero_for_one,
                self.amount_specified,
                self.sqrt_price_limit_x96,
                b"",
            ],
        )
        return EncodedCall(to=self.pool, data=data)


@dataclasses.dataclass(slots=True, frozen=True)
class V4PoolKey:
    """V4 pool identification for swap encoding and on-chain dispatch."""

    currency0: ChecksumAddress
    currency1: ChecksumAddress
    fee: int
    tick_spacing: int
    hooks: ChecksumAddress


@dataclasses.dataclass(slots=True)
class UniswapV4PoolSwapAmounts(AbstractSwapAmounts):
    """UniswapV4PoolSwapAmounts class."""

    address: ChecksumAddress
    id: HexBytes
    pool_key: V4PoolKey
    amount_in: int
    amount_out: int
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int
    recipient: ChecksumAddress | None = None

    def __post_init__(self) -> None:
        """Post-initialization hook."""
        assert self.amount_specified != 0

    def input_amount(self) -> int:
        """Return input amount.

        Returns:
            The computed value.

        """
        return self.amount_in

    def output_amount(self) -> int:
        """Return output amount.

        Returns:
            The computed value.

        """
        return self.amount_out

    def encode(self, *, recipient: ChecksumAddress) -> EncodedCall:
        """Encode a Uniswap V4 swap() call.

        V4 swaps are dispatched through PoolManager which requires an
        unlock/swap callback pattern. The default implementation encodes
        the PoolManager.swap() call; use a custom PayloadComposer for
        executor-specific wrapping.
        """
        msg = (
            "V4 swap encoding requires a PayloadComposer. "
            "The pool_key and swap parameters are available on this "
            "object for custom compositors to use."
        )
        raise NotImplementedError(msg)


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolVector:
    """CurveStableSwapPoolVector class."""

    token_in: Erc20Token
    token_out: Erc20Token

    def __post_init__(self) -> None:
        """Post-initialization hook."""
        assert self.token_in != self.token_out
