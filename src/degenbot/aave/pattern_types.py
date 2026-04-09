"""
Liquidation pattern types for multi-liquidation scenarios.

This module contains pure data types for liquidation pattern detection:
- SINGLE: 1 liquidation with 1 burn event (standard)
- COMBINED_BURN: N liquidations sharing 1 burn event (Issue 0056)
- SEPARATE_BURNS: N liquidations with N separate burn events (Issue 0065)

This module has no dependencies on CLI modules to avoid circular imports.
"""

from dataclasses import dataclass, field
from enum import Enum, auto

from eth_typing import ChecksumAddress


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
