"""
Liquidation processing functions for Aave V3.

This module handles liquidation event processing including multi-liquidation patterns
like COMBINED_BURN and SEPARATE_BURNS.
"""

from typing import TYPE_CHECKING

from degenbot.aave.liquidation_patterns import detect_liquidation_patterns
from degenbot.cli.aave.types import TransactionContext
from degenbot.cli.aave.utils import _get_v_token_for_underlying

if TYPE_CHECKING:
    from degenbot.cli.aave_transaction_operations import Operation


def _preprocess_liquidation_aggregates(
    tx_context: TransactionContext,
    operations: list["Operation"],
) -> None:
    """
    Preprocess liquidations to detect patterns and prepare for processing.

    Detects whether multiple liquidations share the same debt asset and
    determines if they use combined or separate burn events.

    See debug/aave/0056 and debug/aave/0065 for pattern details.
    """

    tx_context.liquidation_patterns = detect_liquidation_patterns(
        operations=operations,
        scaled_token_events=tx_context.scaled_token_events,
        get_v_token_for_underlying=lambda addr: _get_v_token_for_underlying(
            session=tx_context.session,
            market=tx_context.market,
            underlying_address=addr,
        ),
    )
