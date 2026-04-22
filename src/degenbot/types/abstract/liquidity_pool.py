from abc import ABC

from eth_typing import ChecksumAddress
from hexbytes import HexBytes


class AbstractLiquidityPool:
    address: ChecksumAddress
    name: str

    def __eq__(self, other: object) -> bool:
        match other:
            case AbstractLiquidityPool():
                return self.address == other.address
            case HexBytes():
                return self.address.lower() == other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() == "0x" + other.hex().lower()
            case str():
                return self.address.lower() == other.lower()
            case _:
                return NotImplemented

    def __lt__(self, other: object) -> bool:
        match other:
            case AbstractLiquidityPool():
                return self.address < other.address
            case HexBytes():
                return self.address.lower() < other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() < "0x" + other.hex().lower()
            case str():
                return self.address.lower() < other.lower()
            case _:
                return NotImplemented

    def __gt__(self, other: object) -> bool:
        match other:
            case AbstractLiquidityPool():
                return self.address > other.address
            case HexBytes():
                return self.address.lower() > other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() > "0x" + other.hex().lower()
            case str():
                return self.address.lower() > other.lower()
            case _:
                return NotImplemented

    def __hash__(self) -> int:
        return hash(self.address)

    def __str__(self) -> str:
        return self.name


class AbstractUniswapV2Pool(AbstractLiquidityPool, ABC):
    """
    Abstract base class for Uniswap V2-like constant product pools with directional fees.

    Expected attributes:
        token0, token1: Token objects
        fee_token0, fee_token1: Fraction
        state: Object with reserves_token0, reserves_token1
        address: ChecksumAddress
    """


class AbstractConcentratedLiquidityPool(AbstractLiquidityPool, ABC):
    """
    Abstract base class for concentrated liquidity pools (Uniswap V3/V4).

    Expected attributes:
        token0, token1: Token objects
        fee: int (numerator)
        FEE_DENOMINATOR: int
        liquidity: int
        sqrt_price_x96: int
        tick: int
        tick_bitmap: dict
        tick_data: dict
        tick_spacing: int
        sparse_liquidity_map: bool
        state: Object with liquidity, sqrt_price_x96, tick
        address: ChecksumAddress
    """


class AbstractAerodromeV2Pool(AbstractLiquidityPool, ABC):
    """
    Abstract base class for Aerodrome V2 pools.

    Expected attributes:
        token0, token1: Token objects
        fee: Fraction
        stable: bool
        state: Object with reserves_token0, reserves_token1
        address: ChecksumAddress
    """
