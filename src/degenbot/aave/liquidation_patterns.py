"""
Liquidation pattern detection for multi-liquidation scenarios.

This module centralizes the logic for handling complex liquidation patterns:
- SINGLE: 1 liquidation with 1 burn event (standard)
- COMBINED_BURN: N liquidations sharing 1 burn event (Issue 0056)
- SEPARATE_BURNS: N liquidations with N separate burn events (Issue 0065)
"""

from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum, auto

import eth_abi.abi
from eth_typing import ChecksumAddress

from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.operation_types import OperationType
from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent
from degenbot.cli.aave_utils import decode_address
from degenbot.logging import logger


class LiquidationPattern(Enum):
    """
    Patterns for handling multiple liquidations of the same (user, debt_asset).
    """

    SINGLE = auto()  # 1 liquidation, 1 burn
    COMBINED_BURN = auto()  # N liquidations, 1 combined burn (Issue 0056)
    SEPARATE_BURNS = auto()  # N liquidations, N burns (Issue 0065)


@dataclass
class LiquidationGroup:
    """
    Represents a group of liquidations sharing the same (user, debt_v_token).
    """

    user: ChecksumAddress
    debt_v_token: ChecksumAddress

    liquidations: list[tuple[int, int, int]] = field(default_factory=list)
    """List of (operation_id, debt_to_cover, pool_event_log_index) tuples."""

    burn_events: list[tuple[int, int]] = field(default_factory=list)
    """List of (burn_event_log_index, burn_amount) tuples."""

    @property
    def liquidation_count(self) -> int:
        return len(self.liquidations)

    @property
    def burn_event_count(self) -> int:
        return len(self.burn_events)

    @property
    def total_debt_to_cover(self) -> int:
        return sum(debt for _, debt, _ in self.liquidations)

    def detect_pattern(self) -> LiquidationPattern:
        """Determine the liquidation pattern for this group."""
        if self.liquidation_count == 1:
            return LiquidationPattern.SINGLE
        if self.burn_event_count == 1:
            return LiquidationPattern.COMBINED_BURN
        return LiquidationPattern.SEPARATE_BURNS


@dataclass
class LiquidationPatternContext:
    """
    Context for pattern-aware liquidation processing.

    Replaces liquidation_aggregates, liquidation_counts, and processed_liquidations.
    """

    # Detected pattern for each (user, debt_v_token) pair
    patterns: dict[tuple[ChecksumAddress, ChecksumAddress], LiquidationPattern] = field(
        default_factory=dict
    )

    # Detailed group information
    groups: dict[tuple[ChecksumAddress, ChecksumAddress], LiquidationGroup] = field(
        default_factory=dict
    )

    # Track which groups have been processed (for COMBINED_BURN pattern)
    processed_groups: set[tuple[ChecksumAddress, ChecksumAddress]] = field(default_factory=set)

    def get_pattern(
        self, user: ChecksumAddress, debt_v_token: ChecksumAddress
    ) -> LiquidationPattern | None:
        """Get the pattern for a specific (user, debt_v_token)."""
        return self.patterns.get((user, debt_v_token))

    def get_group(
        self, user: ChecksumAddress, debt_v_token: ChecksumAddress
    ) -> LiquidationGroup | None:
        """Get the liquidation group for detailed analysis."""
        return self.groups.get((user, debt_v_token))

    def is_processed(self, user: ChecksumAddress, debt_v_token: ChecksumAddress) -> bool:
        """Check if a group has been fully processed."""
        return (user, debt_v_token) in self.processed_groups

    def mark_processed(self, user: ChecksumAddress, debt_v_token: ChecksumAddress) -> None:
        """Mark a group as fully processed."""
        self.processed_groups.add((user, debt_v_token))


def detect_liquidation_patterns(
    operations: list[Operation],
    scaled_token_events: list[ScaledTokenEvent],
    get_v_token_for_underlying: Callable[[ChecksumAddress], ChecksumAddress | None],
) -> LiquidationPatternContext:
    """
    Analyze all liquidations in a transaction and detect patterns.

    This is the main entry point called during preprocessing.

    Args:
        operations: List of operations from transaction parsing
        scaled_token_events: List of scaled token events
        get_v_token_for_underlying: Function to get vToken address from underlying

    Returns:
        LiquidationPatternContext with detected patterns
    """

    liquidation_types = {
        OperationType.LIQUIDATION,
        OperationType.GHO_LIQUIDATION,
    }

    # Group liquidations by (user, debt_v_token)
    groups: dict[tuple[ChecksumAddress, ChecksumAddress], LiquidationGroup] = {}

    for op in operations:
        if op.operation_type not in liquidation_types:
            continue
        if op.pool_event is None:
            continue

        # Extract user and debt asset from LiquidationCall event
        """
        Event definition:
            event LiquidationCall(
                address indexed collateralAsset,
                address indexed debtAsset,
                address indexed user,
                uint256 debtToCover,
                uint256 liquidatedCollateralAmount,
                address liquidator,
                bool receiveAToken
            );
        """
        debt_asset = decode_address(op.pool_event["topics"][2])
        user = decode_address(op.pool_event["topics"][3])

        # Get vToken address
        debt_v_token = get_v_token_for_underlying(debt_asset)
        if debt_v_token is None:
            continue

        key = (user, debt_v_token)

        if key not in groups:
            groups[key] = LiquidationGroup(user=user, debt_v_token=debt_v_token)

        # Extract debt_to_cover
        debt_to_cover, _, _, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "address", "bool"],
            data=op.pool_event["data"],
        )

        groups[key].liquidations.append((op.operation_id, debt_to_cover, op.pool_event["logIndex"]))

    # Associate burn events with groups
    for event in scaled_token_events:
        if event.event_type not in {
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_BURN,
        }:
            continue

        key = (event.user_address, event.event["address"])

        if key not in groups:
            # logger.warning(f"Orphaned burn event for {key}")
            continue

        groups[key].burn_events.append((event.event["logIndex"], event.amount))

    # Detect patterns
    patterns = {}
    for key, group in groups.items():
        pattern = group.detect_pattern()
        patterns[key] = pattern

        logger.debug(
            f"Liquidation pattern detected: {pattern.name} for user={key[0]}, "
            f"debt_v_token={key[1]}\n"
            f"  Liquidations: {group.liquidation_count}\n"
            f"  Burn events: {group.burn_event_count}\n"
            f"  Total debt: {group.total_debt_to_cover}"
        )

    return LiquidationPatternContext(patterns=patterns, groups=groups)
