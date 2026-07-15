"""Domain types for Aave V3 transaction operations.

Contains data classes and validation containers used to represent decoded on-chain
event data and parsed operations. These are pure domain types with no DB/Session/Provider
dependencies.
"""

from dataclasses import dataclass, field

from eth_typing import ChecksumAddress

from degenbot.aave.events import (
    ScaledTokenEventType,
)
from degenbot.aave.operation_types import OperationType
from degenbot.types.rpc_types import LogReceipt

# Token amount matching tolerance for ray math rounding differences
# Pool revision 9+ uses flooring ray division which can introduce ±2 wei variance
TOKEN_AMOUNT_MATCH_TOLERANCE = 2
SCALED_AMOUNT_POOL_REVISION = 9


@dataclass(frozen=True)
class ScaledTokenEvent:
    """Wrapper for scaled token events with human-readable decoded data."""

    event: LogReceipt
    event_type: ScaledTokenEventType
    user_address: ChecksumAddress
    caller_address: ChecksumAddress | None  # For Mint events
    from_address: ChecksumAddress | None  # For Burn events
    target_address: ChecksumAddress | None  # For Burn events
    amount: int
    balance_increase: int | None
    index: int | None

    @property
    def is_collateral(self) -> bool:
        """Check if collateral."""
        return self.event_type in {
            ScaledTokenEventType.COLLATERAL_BURN,
            ScaledTokenEventType.COLLATERAL_MINT,
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
            ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }

    @property
    def is_debt(self) -> bool:
        """Check if debt."""
        return self.event_type in {
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.DEBT_MINT,
            ScaledTokenEventType.DEBT_TRANSFER,
            ScaledTokenEventType.DEBT_INTEREST_BURN,
            ScaledTokenEventType.DEBT_INTEREST_MINT,
            ScaledTokenEventType.GHO_DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_MINT,
            ScaledTokenEventType.GHO_DEBT_TRANSFER,
            ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
            ScaledTokenEventType.GHO_DEBT_INTEREST_MINT,
            ScaledTokenEventType.ERC20_DEBT_TRANSFER,
        }

    @property
    def is_burn(self) -> bool:
        """Check if burn."""
        return self.event_type in {
            ScaledTokenEventType.COLLATERAL_BURN,
            ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.DEBT_INTEREST_BURN,
            ScaledTokenEventType.GHO_DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
        }


@dataclass(frozen=True)
class Operation:
    """A single logical operation with complete asset flow context."""

    operation_id: int
    operation_type: OperationType

    # Contract revisions at time of operation
    pool_revision: int

    # Core events
    pool_event: LogReceipt | None
    scaled_token_events: list[ScaledTokenEvent]

    # Supporting events
    transfer_events: list[LogReceipt]
    balance_transfer_events: list[LogReceipt]

    # MintedToTreasury amount for Pool Revision 8 (underlying amount = scaled amount)
    minted_to_treasury_amount: int | None = None

    # Debt amount from LiquidationCall event (in underlying units)
    # Used for accurate debt burn calculation
    # (Burn event amount + balance_increase can be off by 1 wei)
    debt_to_cover: int | None = None

    # Validation state
    validation_errors: list[str] = field(default_factory=list)

    def is_valid(self) -> bool:
        """Check if operation passed validation.

        Returns:
            The computed value.

        """
        return len(self.validation_errors) == 0

    def get_all_events(self) -> list[LogReceipt]:
        """Get all events involved in this operation.

        Returns:
            The computed value.

        """
        events = []
        seen_log_indices: set[int] = set()

        if self.pool_event:
            events.append(self.pool_event)
            seen_log_indices.add(self.pool_event["logIndex"])

        for scaled_token_event in [
            ev for ev in self.scaled_token_events if ev.event["logIndex"] not in seen_log_indices
        ]:
            events.append(scaled_token_event.event)
            seen_log_indices.add(scaled_token_event.event["logIndex"])

        for transfer_event in [
            ev for ev in self.transfer_events if ev["logIndex"] not in seen_log_indices
        ]:
            events.append(transfer_event)
            seen_log_indices.add(transfer_event["logIndex"])

        for balance_transfer_event in [
            ev for ev in self.balance_transfer_events if ev["logIndex"] not in seen_log_indices
        ]:
            events.append(balance_transfer_event)
            seen_log_indices.add(balance_transfer_event["logIndex"])

        return events

    def get_event_log_indices(self) -> list[int]:
        """Get all log indices involved in this operation.

        Returns:
            The computed value.

        """
        return [e["logIndex"] for e in self.get_all_events()]
