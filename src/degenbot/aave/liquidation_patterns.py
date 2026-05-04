"""
Liquidation pattern detection for multi-liquidation scenarios.

This module centralizes the logic for handling complex liquidation patterns:
- SINGLE: 1 liquidation with 1 burn event (standard)
- COMBINED_BURN: N liquidations sharing 1 burn event (Issue 0056)
- SEPARATE_BURNS: N liquidations with N separate burn events (Issue 0065)
"""

from collections.abc import Callable

import eth_abi.abi
from eth_typing import ChecksumAddress

from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.operation_types import OperationType
from degenbot.aave.pattern_types import (
    LiquidationGroup,
    LiquidationPatternContext,
)
from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent
from degenbot.cli.aave_utils import decode_address
from degenbot.logging import logger


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

        # TODO: see if these events can be pre-filtered
        if key not in groups:
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
