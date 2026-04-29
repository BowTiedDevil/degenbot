"""
Core data types for Aave event processing.

These types define the interface between the operation parser (CLI layer)
and the event pipeline (aave layer). Moving them here breaks the circular
dependency where aave/enrichment.py imported from cli/.
"""

import operator
from dataclasses import dataclass, field
from enum import Enum

from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.events import (
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    ERC20Event,
    ScaledTokenEventType,
)
from degenbot.aave.operation_types import OperationType


class UserOperation(Enum):
    """User operation types for Aave V3 token events."""

    AAVE_REDEEM = "AAVE REDEEM"
    AAVE_STAKED = "AAVE STAKED"
    BORROW = "BORROW"
    DEPOSIT = "DEPOSIT"
    GHO_BORROW = "GHO BORROW"
    GHO_INTEREST_ACCRUAL = "GHO INTEREST ACCRUAL"
    GHO_REPAY = "GHO REPAY"
    REPAY = "REPAY"
    STKAAVE_TRANSFER = "stkAAVE TRANSFER"
    WITHDRAW = "WITHDRAW"


TOKEN_AMOUNT_MATCH_TOLERANCE = 2
SCALED_AMOUNT_POOL_REVISION = 9


@dataclass(frozen=True)
class ScaledTokenEvent:
    """
    Wrapper for scaled token events with human-readable decoded data.
    """

    event: LogReceipt
    event_type: ScaledTokenEventType
    user_address: ChecksumAddress
    caller_address: ChecksumAddress | None
    from_address: ChecksumAddress | None
    target_address: ChecksumAddress | None
    amount: int
    balance_increase: int | None
    index: int | None

    @property
    def is_collateral(self) -> bool:
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

    pool_revision: int

    pool_event: LogReceipt | None
    scaled_token_events: list[ScaledTokenEvent]

    transfer_events: list[LogReceipt]
    balance_transfer_events: list[LogReceipt]

    minted_to_treasury_amount: int | None = None
    debt_to_cover: int | None = None

    validation_errors: list[str] = field(default_factory=list)

    def is_valid(self) -> bool:
        return len(self.validation_errors) == 0

    def get_all_events(self) -> list[LogReceipt]:
        events = []
        seen_log_indices: set[int] = set()

        if self.pool_event:
            events.append(self.pool_event)
            seen_log_indices.add(self.pool_event["logIndex"])

        for scaled_token_event in self.scaled_token_events:
            if scaled_token_event.event["logIndex"] not in seen_log_indices:
                events.append(scaled_token_event.event)
                seen_log_indices.add(scaled_token_event.event["logIndex"])

        for ev in self.transfer_events:
            if ev["logIndex"] not in seen_log_indices:
                events.append(ev)
                seen_log_indices.add(ev["logIndex"])

        for ev in self.balance_transfer_events:
            if ev["logIndex"] not in seen_log_indices:
                events.append(ev)
                seen_log_indices.add(ev["logIndex"])

        return events

    def get_event_log_indices(self) -> list[int]:
        return [e["logIndex"] for e in self.get_all_events()]


class TransactionValidationError(Exception):
    """
    Raised when transaction validation fails.

    Provides comprehensive plain-text dump of all events and operations
    for debugging.
    """

    def __init__(
        self,
        message: str,
        tx_hash: HexBytes,
        events: list[LogReceipt],
        operations: list[Operation],
    ) -> None:
        self.tx_hash = tx_hash
        self.events = events
        self.operations = operations
        self.error_message = message

        dump = self._build_error_dump()
        super().__init__(dump)

    def _build_error_dump(self) -> str:
        lines = [
            "=" * 80,
            "TRANSACTION VALIDATION FAILED",
            "=" * 80,
            "",
            f"Transaction Hash: {self.tx_hash.to_0x_hex()}",
            f"Block: {self.events[0]['blockNumber'] if self.events else 'N/A'}",
            "",
            "-" * 40,
            "RAW EVENTS (sorted by logIndex)",
            "-" * 40,
            "",
        ]

        for event in sorted(self.events, key=operator.itemgetter("logIndex")):
            lines.extend(self._format_event(event))

        lines.extend([
            "",
            "-" * 40,
            f"PARSED OPERATIONS ({len(self.operations)})",
            "-" * 40,
            "",
        ])

        for op in self.operations:
            lines.extend(self._format_operation(op))

        lines.extend([
            "",
            "VALIDATION ERRORS:",
            "-" * 40,
            self.error_message,
            "=" * 80,
        ])

        return "\n".join(lines)

    def _format_event(self, event: LogReceipt) -> list[str]:
        topic = event["topics"][0]
        topic_name = self._get_event_name(topic)

        lines = [
            f"[{event['logIndex']}] {topic_name}",
            f"    Address: {event['address']}",
            f"    Topic: {topic.hex()}",
        ]

        if len(event["topics"]) > 1:
            for j, t in enumerate(event["topics"][1:], 1):
                addr = self._try_decode_address(t)
                if addr:
                    lines.append(f"    Topic[{j}] (address): {addr}")
                else:
                    lines.append(f"    Topic[{j}]: {t.hex()}")

        data_str = event["data"].hex()
        if len(data_str) > 60:  # noqa:PLR2004
            data_str = data_str[:30] + "..." + data_str[-30:]
        lines.extend((f"    Data: {data_str}", ""))

        return lines

    @staticmethod
    def _format_operation(op: Operation) -> list[str]:
        lines = [
            f"Operation {op.operation_id}: {op.operation_type.name}",
        ]

        if op.pool_event:
            lines.append(f"  Pool Event: logIndex={op.pool_event['logIndex']}")
        else:
            lines.append("  Pool Event: None")

        lines.append(f"  Scaled Token Events ({len(op.scaled_token_events)}):")
        for ev in op.scaled_token_events:
            lines.extend((
                f"    logIndex {ev.event['logIndex']}: {ev.event_type}",
                f"      user: {ev.user_address}",
                f"      amount: {ev.amount}",
                f"      balance_increase: {ev.balance_increase}",
            ))

        if op.validation_errors:
            lines.append("  VALIDATION ERRORS:")
            lines.extend(f"    X {err}" for err in op.validation_errors)
        else:
            lines.append("  Status: Valid")

        lines.append("")
        return lines

    @staticmethod
    def _get_event_name(topic: HexBytes) -> str:

        for pool_event in AaveV3PoolEvent:
            if pool_event.value == topic:
                return pool_event.name
        for scaled_token_event in AaveV3ScaledTokenEvent:
            if scaled_token_event.value == topic:
                return scaled_token_event.name
        if topic == ERC20Event.TRANSFER.value:
            return "Transfer"
        return "UNKNOWN"

    @staticmethod
    def _try_decode_address(topic: HexBytes | str) -> ChecksumAddress | None:
        from degenbot.checksum_cache import get_checksum_address

        try:
            hex_str = topic[-40:] if isinstance(topic, str) else topic.hex()[-40:]
            return get_checksum_address("0x" + hex_str)
        except (AttributeError, TypeError, ValueError):
            return None


class TransactionOperations:
    """Container for all operations in a transaction."""

    def __init__(
        self,
        tx_hash: HexBytes,
        block_number: int,
        operations: list[Operation],
        unassigned_events: list[LogReceipt],
    ) -> None:
        self.tx_hash = tx_hash
        self.block_number = block_number
        self.operations = operations
        self.unassigned_events = unassigned_events

    def validate(self, all_events: list[LogReceipt]) -> None:
        """Strict validation - fails on any unmet expectation."""
        all_errors = []

        for op in self.operations:  # pragma: no cover
            if not op.is_valid():
                all_errors.extend([
                    f"Operation {op.operation_id} ({op.operation_type.name}): {err}"
                    for err in op.validation_errors
                ])

        required_unassigned = [e for e in self.unassigned_events if self._is_required_pool_event(e)]
        assert not required_unassigned

        scaled_token_topics = {
            AaveV3ScaledTokenEvent.BURN.value,
            AaveV3ScaledTokenEvent.MINT.value,
            AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value,
        }
        unassigned_scaled = [
            e for e in self.unassigned_events if e["topics"][0] in scaled_token_topics
        ]
        assert not unassigned_scaled

        assigned_indices: dict[int, int] = {}
        for op in self.operations:
            for log_idx in op.get_event_log_indices():
                if log_idx in assigned_indices:
                    all_errors.append(
                        f"Event at logIndex {log_idx} assigned to multiple operations: "
                        f"{assigned_indices[log_idx]} and {op.operation_id}. "
                        f"DEBUG NOTE: This event may need to be reusable. "
                        f"Investigate whether it can match multiple operations "
                        f"(e.g., LIQUIDATION_CALL or REPAY with useATokens)."
                    )
                assigned_indices[log_idx] = op.operation_id

        if all_errors:  # pragma: no cover
            raise TransactionValidationError(
                message="Transaction validation failed:\n" + "\n".join(all_errors),
                tx_hash=self.tx_hash,
                events=all_events,
                operations=self.operations,
            )

    @staticmethod
    def _is_required_pool_event(event: LogReceipt) -> bool:
        pool_topics = {
            AaveV3PoolEvent.SUPPLY.value,
            AaveV3PoolEvent.WITHDRAW.value,
            AaveV3PoolEvent.BORROW.value,
            AaveV3PoolEvent.REPAY.value,
            AaveV3PoolEvent.LIQUIDATION_CALL.value,
            AaveV3PoolEvent.DEFICIT_CREATED.value,
        }
        return event["topics"][0] in pool_topics
