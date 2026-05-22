"""
Detection result types for Curve pool construction.

Each frozen dataclass holds the output of one detection step, providing a
narrow interface between detection sub-modules and the builder orchestrator.
"""

from dataclasses import dataclass

from eth_typing import ChecksumAddress


@dataclass(frozen=True)
class CoinDiscoveryResult:
    """Result of coin enumeration for a Curve pool."""

    token_addresses: tuple[ChecksumAddress, ...]
    balances: tuple[int, ...]
    coin_prototype: str  # "coins(uint256)" or "coins(int128)"
    balance_prototype: str  # "balances(uint256)" or "balances(int128)"


@dataclass(frozen=True)
class LendingDetectionResult:
    """Result of lending token detection for a Curve pool."""

    use_lending: tuple[bool, ...]
    precision_multipliers: tuple[int, ...] | None  # None if no overrides needed


@dataclass(frozen=True)
class MetapoolDetectionResult:
    """Result of metapool detection for a Curve pool."""

    is_meta: bool
    base_pool_address: ChecksumAddress | None  # None if not a metapool
    tokens_underlying: tuple[ChecksumAddress, ...] | None  # None if not a metapool


@dataclass(frozen=True)
class CryptoDetectionResult:
    """Result of crypto pool parameter detection."""

    is_crypto: bool  # True if fee_gamma > 0
    fee_gamma: int | None
    mid_fee: int | None
    out_fee: int | None
    gamma: int | None
    offpeg_fee_multiplier: int | None


@dataclass(frozen=True)
class ARampingResult:
    """Result of A ramping parameter detection."""

    initial_a: int | None
    initial_a_time: int | None
    future_a: int | None
    future_a_time: int | None
    has_ramping: bool  # True if all four values were fetched
