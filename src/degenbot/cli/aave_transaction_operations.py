"""Aave V3 transaction operation parser.

Parses transaction events into logical operations based on asset flows.
Provides strict validation with detailed plain-text error reporting.
"""

from __future__ import annotations

import operator
from dataclasses import dataclass, field
from enum import Enum, auto
from typing import TYPE_CHECKING, TypedDict

from eth_abi.abi import decode
from hexbytes import HexBytes

from degenbot.aave.events import AaveV3PoolEvent, AaveV3ScaledTokenEvent
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.logging import logger

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
    from web3.types import LogReceipt


def _topic_to_address(topic: HexBytes | str) -> ChecksumAddress:
    """Extract Ethereum address from event topic.

    Handles both HexBytes objects (from web3.py) and hex strings (from JSON).
    The address is the last 40 hex characters (20 bytes) of the topic.

    Args:
        topic: Event topic as HexBytes or hex string (e.g., "0x000...d322a490...")

    Returns:
        ChecksumAddress: The extracted address
    """
    if isinstance(topic, str):
        # Already a hex string, extract last 40 chars
        return get_checksum_address("0x" + topic[-40:])
    # HexBytes object, call .hex() method
    return get_checksum_address("0x" + topic.hex()[-40:])


def _decode_hex_data(data: str | HexBytes) -> bytes:
    """Convert hex string (with or without 0x prefix) to bytes."""
    if isinstance(data, (HexBytes, bytes)):
        return bytes(data)
    if isinstance(data, str) and data.startswith("0x"):
        data = data[2:]
    return bytes.fromhex(data)


def _get_topic_str(topic: HexBytes | str) -> str:
    """Convert topic to hex string without 0x prefix.

    Handles both HexBytes objects (from web3.py) and hex strings (from JSON).
    """
    if isinstance(topic, str):
        return topic.lstrip("0x")
    return topic.hex()


# ============================================================================
# ENUMS AND CONSTANTS
# ============================================================================


class OperationType(Enum):
    """Types of Aave operations based on asset flows."""

    # Standard operations
    SUPPLY = auto()  # SUPPLY -> COLLATERAL_MINT
    WITHDRAW = auto()  # WITHDRAW -> COLLATERAL_BURN
    BORROW = auto()  # BORROW -> DEBT_MINT
    REPAY = auto()  # REPAY -> DEBT_BURN

    # Composite operations
    REPAY_WITH_ATOKENS = auto()  # REPAY -> DEBT_BURN + COLLATERAL_BURN
    LIQUIDATION = auto()  # LIQUIDATION_CALL -> DEBT_BURN + COLLATERAL_BURN
    SELF_LIQUIDATION = auto()  # LIQUIDATION_CALL -> DEBT_MINT + COLLATERAL_MINT

    # GHO-specific operations
    GHO_BORROW = auto()  # BORROW -> GHO_DEBT_MINT
    GHO_REPAY = auto()  # REPAY -> GHO_DEBT_BURN
    GHO_LIQUIDATION = auto()  # LIQUIDATION_CALL -> GHO_DEBT_BURN + COLLATERAL_BURN
    GHO_FLASH_LOAN = auto()  # DEFICIT_CREATED -> GHO_DEBT_BURN

    # Standalone events
    INTEREST_ACCRUAL = auto()  # Mint/Burn with no pool event
    BALANCE_TRANSFER = auto()  # Standalone BalanceTransfer
    MINT_TO_TREASURY = auto()  # Pool minting aTokens to treasury (no SUPPLY event)
    UNKNOWN = auto()


# GHO Token Address (Ethereum Mainnet)
GHO_TOKEN_ADDRESS = get_checksum_address("0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f")

# GHO Variable Debt Token Address (Ethereum Mainnet)
GHO_VARIABLE_DEBT_TOKEN_ADDRESS = get_checksum_address("0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B")

# Token event topic hashes
TRANSFER_TOPIC = HexBytes("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")


# ============================================================================
# DATA CLASSES
# ============================================================================


@dataclass(frozen=True)
class AssetFlow:
    """Represents a single asset movement in an operation."""

    asset_address: ChecksumAddress
    from_address: ChecksumAddress
    to_address: ChecksumAddress
    amount: int
    event_type: str  # "Mint", "Burn", "Transfer", etc.
    event_log_index: int


@dataclass(frozen=True)
class ScaledTokenEvent:
    """Wrapper for scaled token events with decoded data."""

    event: LogReceipt
    event_type: str  # "COLLATERAL_MINT", "COLLATERAL_BURN", "DEBT_MINT", etc.
    user_address: ChecksumAddress
    caller_address: ChecksumAddress | None  # For Mint events
    from_address: ChecksumAddress | None  # For Burn events
    target_address: ChecksumAddress | None  # For Burn events
    amount: int
    balance_increase: int
    index: int

    @property
    def is_interest_accrual(self) -> bool:
        """Check if this is pure interest accrual (value == balanceIncrease)."""
        return self.amount == self.balance_increase

    @property
    def is_collateral(self) -> bool:
        return self.event_type.startswith("COLLATERAL")

    @property
    def is_debt(self) -> bool:
        return self.event_type.startswith("DEBT") or self.event_type.startswith("GHO")

    @property
    def is_mint(self) -> bool:
        return self.event_type.endswith("MINT")

    @property
    def is_burn(self) -> bool:
        return self.event_type.endswith("BURN")


@dataclass(frozen=True)
class Operation:
    """A single logical operation with complete asset flow context."""

    operation_id: int
    operation_type: OperationType

    # Core events
    pool_event: LogReceipt | None
    scaled_token_events: list[ScaledTokenEvent]

    # Supporting events
    transfer_events: list[LogReceipt]
    balance_transfer_events: list[LogReceipt]

    # Computed asset flows
    asset_flows: list[AssetFlow] = field(default_factory=list)

    # Validation state
    validation_errors: list[str] = field(default_factory=list)

    def is_valid(self) -> bool:
        """Check if operation passed validation."""
        return len(self.validation_errors) == 0

    def get_all_events(self) -> list[LogReceipt]:
        """Get all events involved in this operation."""
        events = []
        seen_log_indices: set[int] = set()

        if self.pool_event:
            events.append(self.pool_event)
            seen_log_indices.add(self.pool_event["logIndex"])

        for ev in self.scaled_token_events:
            if ev.event["logIndex"] not in seen_log_indices:
                events.append(ev.event)
                seen_log_indices.add(ev.event["logIndex"])

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
        """Get all log indices involved in this operation."""
        return [e["logIndex"] for e in self.get_all_events()]


class EventMatchResult(TypedDict):
    """Result of a successful event match."""

    pool_event: LogReceipt | None
    should_consume: bool
    extraction_data: dict[str, int | bool]


# ============================================================================
# EXCEPTIONS
# ============================================================================


class TransactionValidationError(Exception):
    """Raised when transaction validation fails.

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

        # Build comprehensive dump
        dump = self._build_error_dump()
        super().__init__(dump)

    def _build_error_dump(self) -> str:
        """Build human-readable error report."""
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
        """Format a single event for display."""
        topic = event["topics"][0]
        topic_name = self._get_event_name(topic)

        lines = [
            f"[{event['logIndex']}] {topic_name}",
            f"    Address: {event['address']}",
            f"    Topic: {topic.hex()}",
        ]

        # Add indexed parameters
        if len(event["topics"]) > 1:
            for j, t in enumerate(event["topics"][1:], 1):
                addr = self._try_decode_address(t)
                if addr:
                    lines.append(f"    Topic[{j}] (address): {addr}")
                else:
                    lines.append(f"    Topic[{j}]: {t.hex()}")

        # Add data
        data_str = event["data"].hex()
        if len(data_str) > 60:
            data_str = data_str[:30] + "..." + data_str[-30:]
        lines.extend((f"    Data: {data_str}", ""))

        return lines

    def _format_operation(self, op: Operation) -> list[str]:
        """Format a single operation for display."""
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
            lines.append("  Status: OK Valid")

        lines.append("")
        return lines

    def _get_event_name(self, topic: HexBytes) -> str:
        """Get human-readable event name from topic."""
        for pool_event in AaveV3PoolEvent:
            if pool_event.value == topic:
                return pool_event.name
        for scaled_token_event in AaveV3ScaledTokenEvent:
            if scaled_token_event.value == topic:
                return scaled_token_event.name
        if topic == TRANSFER_TOPIC:
            return "Transfer"
        return "UNKNOWN"

    def _try_decode_address(self, topic: HexBytes | str) -> ChecksumAddress | None:
        """Try to decode topic as address.

        Handles both HexBytes objects (with .hex() method) and strings.
        """
        try:
            # Handle both HexBytes (has .hex() method) and strings
            if isinstance(topic, str):
                # Already a hex string, extract last 40 chars
                hex_str = topic[-40:]
            else:
                # HexBytes object, call .hex() method
                hex_str = topic.hex()[-40:]
            return get_checksum_address("0x" + hex_str)
        except Exception:
            return None


# ============================================================================
# PARSER
# ============================================================================


class TransactionOperationsParser:
    """Parses transaction events into logical operations."""

    def __init__(
        self,
        gho_token_address: ChecksumAddress | None = None,
        token_type_mapping: dict[ChecksumAddress, str] | None = None,
        pool_address: ChecksumAddress | None = None,
        debt_token_to_reserve: dict[ChecksumAddress, ChecksumAddress] | None = None,
    ) -> None:
        """Initialize parser.

        Args:
            gho_token_address: Address of GHO variable debt token.
                              Defaults to mainnet address if not provided.
            token_type_mapping: Mapping of token addresses to their types.
                               Keys are token addresses, values are "aToken" or "vToken".
            pool_address: Address of the Aave Pool contract.
                         Used to detect mintToTreasury operations.
            debt_token_to_reserve: Mapping of debt token addresses to their underlying reserve addresses.
                                  Used to correctly match debt burns with repay events in flash loan scenarios.
        """
        self.gho_token_address = gho_token_address or GHO_VARIABLE_DEBT_TOKEN_ADDRESS
        # Normalize token type mapping keys to checksum addresses
        self.token_type_mapping = {
            get_checksum_address(k): v for k, v in (token_type_mapping or {}).items()
        }
        self.pool_address = pool_address
        # Normalize debt token to reserve mapping
        self.debt_token_to_reserve = {
            get_checksum_address(k): get_checksum_address(v)
            for k, v in (debt_token_to_reserve or {}).items()
        }

    def _get_reserve_for_debt_token(
        self, debt_token_address: ChecksumAddress
    ) -> ChecksumAddress | None:
        """Get the underlying reserve address for a debt token.

        Args:
            debt_token_address: The debt token (vToken) address.

        Returns:
            The underlying reserve asset address, or None if not found.
        """
        return self.debt_token_to_reserve.get(get_checksum_address(debt_token_address))

    def parse(self, events: list[LogReceipt], tx_hash: HexBytes) -> TransactionOperations:
        """Parse events into operations."""
        if not events:
            return TransactionOperations(
                tx_hash=tx_hash,
                block_number=0,
                operations=[],
                unassigned_events=[],
            )

        block_number = events[0]["blockNumber"]

        # Step 1: Identify pool events (anchors for operations)
        pool_events = self._extract_pool_events(events)

        # Step 2: Identify and decode scaled token events
        scaled_events = self._extract_scaled_token_events(events)

        # Step 3: Group into operations
        operations: list[Operation] = []
        assigned_log_indices: set[int] = set()

        for i, pool_event in enumerate(pool_events):
            operation = self._create_operation_from_pool_event(
                operation_id=i,
                pool_event=pool_event,
                scaled_events=scaled_events,
                all_events=events,
                assigned_indices=assigned_log_indices,
            )
            if operation:
                operations.append(operation)
                # Track assigned events
                assigned_log_indices.update(operation.get_event_log_indices())

        # Step 4b: Create MINT_TO_TREASURY operations for unassigned scaled token mints
        # where the user is the Pool contract (protocol reserves being minted to treasury)
        mint_to_treasury_ops = self._create_mint_to_treasury_operations(
            scaled_events=scaled_events,
            assigned_indices=assigned_log_indices,
            starting_operation_id=len(operations),
        )
        operations.extend(mint_to_treasury_ops)
        assigned_log_indices.update(
            ev.event["logIndex"] for op in mint_to_treasury_ops for ev in op.scaled_token_events
        )

        # Step 4c: Create INTEREST_ACCRUAL operations for unassigned scaled token events
        # that represent interest accrual (amount == balance_increase)
        # Skip DEBT_MINT extraction if there's a LIQUIDATION_CALL (flash loan pattern)
        interest_accrual_ops = self._create_interest_accrual_operations(
            scaled_events=scaled_events,
            assigned_indices=assigned_log_indices,
            starting_operation_id=len(operations),
            all_events=events,
        )
        operations.extend(interest_accrual_ops)
        assigned_log_indices.update(
            ev.event["logIndex"] for op in interest_accrual_ops for ev in op.scaled_token_events
        )
        # Also track transfer_events that were matched to interest accrual operations
        assigned_log_indices.update(
            ev["logIndex"] for op in interest_accrual_ops for ev in op.transfer_events
        )

        # Step 4d: Create TRANSFER operations for unassigned transfer events
        transfer_ops = self._create_transfer_operations(
            scaled_events=scaled_events,
            assigned_indices=assigned_log_indices,
            starting_operation_id=len(operations),
            existing_operations=operations,
        )
        operations.extend(transfer_ops)

        # Step 4d: Handle unassigned events
        unassigned_events = [
            e
            for e in events
            if e["logIndex"] not in assigned_log_indices and e["topics"][0] != TRANSFER_TOPIC
        ]

        # Step 5: Validate all operations
        for op in operations:
            self._validate_operation(op)

        return TransactionOperations(
            tx_hash=tx_hash,
            block_number=block_number,
            operations=operations,
            unassigned_events=unassigned_events,
        )

    def _extract_pool_events(self, events: list[LogReceipt]) -> list[LogReceipt]:
        """Extract pool-level events (SUPPLY, WITHDRAW, etc.)."""
        pool_topics = {
            AaveV3PoolEvent.SUPPLY.value.hex(),
            AaveV3PoolEvent.WITHDRAW.value.hex(),
            AaveV3PoolEvent.BORROW.value.hex(),
            AaveV3PoolEvent.REPAY.value.hex(),
            AaveV3PoolEvent.LIQUIDATION_CALL.value.hex(),
            AaveV3PoolEvent.DEFICIT_CREATED.value.hex(),
        }

        return sorted(
            [e for e in events if _get_topic_str(e["topics"][0]) in pool_topics],
            key=operator.itemgetter("logIndex"),
        )

    def _extract_scaled_token_events(self, events: list[LogReceipt]) -> list[ScaledTokenEvent]:
        """Extract and decode scaled token events."""
        result = []

        for event in events:
            topic = _get_topic_str(event["topics"][0])

            if topic == AaveV3ScaledTokenEvent.MINT.value.hex():
                ev = self._decode_mint_event(event)
                if ev:
                    result.append(ev)

            elif topic == AaveV3ScaledTokenEvent.BURN.value.hex():
                ev = self._decode_burn_event(event)
                if ev:
                    result.append(ev)

            elif topic == AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value.hex():
                ev = self._decode_balance_transfer_event(event)
                if ev:
                    result.append(ev)

            elif topic == TRANSFER_TOPIC.hex():
                # Handle ERC20 Transfer events for aTokens and vTokens
                ev = self._decode_transfer_event(event)
                if ev:
                    result.append(ev)

        return sorted(result, key=lambda e: e.event["logIndex"])

    def _decode_mint_event(self, event: LogReceipt) -> ScaledTokenEvent | None:
        """Decode a Mint event."""
        try:
            caller = _topic_to_address(event["topics"][1])
            user = _topic_to_address(event["topics"][2])
            # Convert hex string to bytes for eth_abi.decode
            amount, balance_increase, index = decode(
                ["uint256", "uint256", "uint256"], _decode_hex_data(event["data"])
            )

            # Determine event type based on token type
            token_address = get_checksum_address(event["address"])
            if token_address == self.gho_token_address:
                event_type = "GHO_DEBT_MINT"
            else:
                # Use token type mapping to determine if this is a collateral or debt mint
                token_type = self.token_type_mapping.get(token_address)
                if token_type == "aToken":
                    event_type = "COLLATERAL_MINT"
                elif token_type == "vToken":
                    event_type = "DEBT_MINT"
                else:
                    # Fallback for unknown tokens - default to collateral
                    event_type = "COLLATERAL_MINT"

            return ScaledTokenEvent(
                event=event,
                event_type=event_type,
                user_address=user,
                caller_address=caller,
                from_address=None,
                target_address=None,
                amount=amount,
                balance_increase=balance_increase,
                index=index,
            )
        except Exception:
            return None

    def _decode_burn_event(self, event: LogReceipt) -> ScaledTokenEvent | None:
        """Decode a Burn event."""
        try:
            from_addr = _topic_to_address(event["topics"][1])
            target = _topic_to_address(event["topics"][2])
            amount, balance_increase, index = decode(
                ["uint256", "uint256", "uint256"], _decode_hex_data(event["data"])
            )

            # Determine event type based on token type
            token_address = get_checksum_address(event["address"])
            if token_address == self.gho_token_address:
                event_type = "GHO_DEBT_BURN"
            else:
                # Use token type mapping to determine if this is a collateral or debt burn
                token_type = self.token_type_mapping.get(token_address)
                if token_type == "aToken":
                    event_type = "COLLATERAL_BURN"
                elif token_type == "vToken":
                    event_type = "DEBT_BURN"
                else:
                    # Fallback for unknown tokens
                    event_type = "UNKNOWN_BURN"

            return ScaledTokenEvent(
                event=event,
                event_type=event_type,
                user_address=from_addr,
                caller_address=None,
                from_address=from_addr,
                target_address=target,
                amount=amount,
                balance_increase=balance_increase,
                index=index,
            )
        except Exception:
            return None

    def _decode_balance_transfer_event(self, event: LogReceipt) -> ScaledTokenEvent | None:
        """Decode a BalanceTransfer event.

        BalanceTransfer events represent internal scaled balance movements in aTokens.
        During liquidations, collateral may be transferred to the treasury instead of burned.
        """
        try:
            from_addr = _topic_to_address(event["topics"][1])
            to_addr = _topic_to_address(event["topics"][2])
            # BalanceTransfer data: amount, index
            amount, index = decode(["uint256", "uint256"], _decode_hex_data(event["data"]))

            # Determine event type based on token type
            token_address = get_checksum_address(event["address"])
            if token_address == self.gho_token_address:
                # GHO doesn't have BalanceTransfer events, but handle just in case
                event_type = "GHO_DEBT_TRANSFER"
            else:
                # Use token type mapping to determine if this is collateral or debt
                token_type = self.token_type_mapping.get(token_address)
                if token_type == "aToken":
                    event_type = "COLLATERAL_TRANSFER"
                elif token_type == "vToken":
                    event_type = "DEBT_TRANSFER"
                else:
                    # Fallback for unknown tokens
                    event_type = "UNKNOWN_TRANSFER"

            return ScaledTokenEvent(
                event=event,
                event_type=event_type,
                user_address=from_addr,  # The user whose balance decreased
                caller_address=None,
                from_address=from_addr,
                target_address=to_addr,
                amount=amount,
                balance_increase=0,  # BalanceTransfer doesn't have balanceIncrease
                index=index,
            )
        except Exception:
            return None

    def _decode_transfer_event(self, event: LogReceipt) -> ScaledTokenEvent | None:
        """Decode an ERC20 Transfer event for aTokens/vTokens.

        Transfer events are standard ERC20 events that occur when aTokens or vTokens
        are transferred between users (e.g., user -> aggregator).
        """
        try:
            from_addr = _topic_to_address(event["topics"][1])
            to_addr = _topic_to_address(event["topics"][2])
            # Transfer data: amount
            (amount,) = decode(["uint256"], _decode_hex_data(event["data"]))

            # Determine event type based on token type
            token_address = get_checksum_address(event["address"])
            if token_address == self.gho_token_address:
                event_type = "GHO_DEBT_TRANSFER"
            else:
                # Use token type mapping to determine if this is collateral or debt
                token_type = self.token_type_mapping.get(token_address)
                if token_type == "aToken":
                    event_type = "COLLATERAL_TRANSFER"
                elif token_type == "vToken":
                    event_type = "DEBT_TRANSFER"
                else:
                    # Fallback for unknown tokens
                    event_type = "UNKNOWN_TRANSFER"

            return ScaledTokenEvent(
                event=event,
                event_type=event_type,
                user_address=from_addr,  # The user whose balance decreased
                caller_address=None,
                from_address=from_addr,
                target_address=to_addr,
                amount=amount,
                balance_increase=0,  # Transfer doesn't have balanceIncrease
                index=0,  # Transfer doesn't have index
            )
        except Exception:
            return None

    def _create_operation_from_pool_event(
        self,
        operation_id: int,
        pool_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
    ) -> Operation | None:
        """Create operation starting from a pool event."""
        topic = _get_topic_str(pool_event["topics"][0])

        if topic == AaveV3PoolEvent.SUPPLY.value.hex():
            return self._create_supply_operation(
                operation_id, pool_event, scaled_events, all_events, assigned_indices
            )
        if topic == AaveV3PoolEvent.WITHDRAW.value.hex():
            return self._create_withdraw_operation(
                operation_id, pool_event, scaled_events, all_events, assigned_indices
            )
        if topic == AaveV3PoolEvent.BORROW.value.hex():
            return self._create_borrow_operation(
                operation_id, pool_event, scaled_events, all_events, assigned_indices
            )
        if topic == AaveV3PoolEvent.REPAY.value.hex():
            return self._create_repay_operation(
                operation_id, pool_event, scaled_events, all_events, assigned_indices
            )
        if topic == AaveV3PoolEvent.LIQUIDATION_CALL.value.hex():
            return self._create_liquidation_operation(
                operation_id, pool_event, scaled_events, all_events, assigned_indices
            )
        if topic == AaveV3PoolEvent.DEFICIT_CREATED.value.hex():
            return self._create_deficit_operation(
                operation_id, pool_event, scaled_events, all_events, assigned_indices
            )

        return None

    def _create_supply_operation(
        self,
        operation_id: int,
        supply_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
    ) -> Operation:
        """Create SUPPLY operation."""
        # Decode SUPPLY event
        # Supply(address indexed reserve, address user, address indexed onBehalfOf,
        #        uint256 amount, uint16 indexed referralCode)
        # topics[1] = reserve, topics[2] = onBehalfOf, topics[3] = referralCode
        # data = (user [32 bytes], amount [32 bytes])
        reserve = self._decode_address(supply_event["topics"][1])
        on_behalf_of = self._decode_address(supply_event["topics"][2])

        # Find collateral mint for this user
        # For SUPPLY: look for mints where value > balance_increase (standard deposit)
        # Match on onBehalfOf (beneficiary) from the SUPPLY event, which corresponds
        # to the user_address in the collateral mint event
        collateral_mint = None
        for ev in scaled_events:
            # Only match mints that represent deposits (value > balance_increase)
            # Mints where balance_increase > value are interest accrual for withdrawals
            if all([
                ev.event_type == "COLLATERAL_MINT",
                ev.user_address == on_behalf_of,
                ev.event["logIndex"] not in assigned_indices,
                ev.amount > ev.balance_increase,
            ]):
                collateral_mint = ev
                break

        scaled_token_events = [collateral_mint] if collateral_mint else []

        # Also look for matching Transfer events from zero address (ERC20 mint)
        # These represent the same supply operation
        # Match on onBehalfOf (beneficiary) as the target of the transfer
        transfer_events = []
        if collateral_mint:
            for ev in scaled_events:
                if all([
                    ev.event_type == "COLLATERAL_TRANSFER",
                    ev.from_address == ZERO_ADDRESS,
                    ev.target_address == on_behalf_of,
                    ev.amount == collateral_mint.amount,
                    ev.event["address"] == collateral_mint.event["address"],
                    ev.event["logIndex"] not in assigned_indices,
                ]):
                    transfer_events.append(ev.event)
                    break  # Only match one transfer per mint

        return Operation(
            operation_id=operation_id,
            operation_type=OperationType.SUPPLY,
            pool_event=supply_event,
            scaled_token_events=scaled_token_events,
            transfer_events=transfer_events,
            balance_transfer_events=[],
        )

    def _create_withdraw_operation(
        self,
        operation_id: int,
        withdraw_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
    ) -> Operation:
        """Create WITHDRAW operation."""
        # Decode WITHDRAW event
        reserve = self._decode_address(withdraw_event["topics"][1])
        user = self._decode_address(withdraw_event["topics"][2])
        (amount,) = decode(["uint256"], _decode_hex_data(withdraw_event["data"]))

        # Find collateral burn for this user
        collateral_burn = None
        for ev in scaled_events:
            if all([
                ev.event_type in {"COLLATERAL_BURN", "UNKNOWN_BURN"},
                ev.user_address == user,
                ev.event["logIndex"] not in assigned_indices,
            ]):
                collateral_burn = ev
                break

        # Find interest accrual mints for this user
        # During withdrawals, interest accrues before the burn (balance_increase > 0)
        # Only match mints for the same token as the burn
        interest_mints: list[ScaledTokenEvent] = []
        if collateral_burn:
            burn_token_address = collateral_burn.event["address"]
            for ev in scaled_events:
                if ev.event["logIndex"] in assigned_indices:
                    continue

                # Interest accrual mints have balance_increase > 0
                # and must be for the same token as the burn
                if all([
                    ev.event_type == "COLLATERAL_MINT",
                    ev.user_address == user,
                    ev.balance_increase > 0,
                    ev.event["address"] == burn_token_address,
                ]):
                    interest_mints.append(ev)

        # Also look for matching Transfer events to zero address (ERC20 burn)
        # These represent the same withdrawal operation
        transfer_events = []
        for ev in scaled_events:
            if all([
                ev.event_type == "COLLATERAL_TRANSFER",
                ev.from_address == user,
                ev.target_address == ZERO_ADDRESS,
                ev.event["logIndex"] not in assigned_indices,
            ]):
                transfer_events.append(ev.event)
                break  # Only match one transfer per burn

        scaled_token_events: list[ScaledTokenEvent] = []
        if collateral_burn:
            scaled_token_events.append(collateral_burn)
        scaled_token_events.extend(interest_mints)

        return Operation(
            operation_id=operation_id,
            operation_type=OperationType.WITHDRAW,
            pool_event=withdraw_event,
            scaled_token_events=scaled_token_events,
            transfer_events=transfer_events,
            balance_transfer_events=[],
        )

    def _create_borrow_operation(
        self,
        operation_id: int,
        borrow_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
    ) -> Operation:
        """Create BORROW operation."""
        # Decode BORROW event
        reserve = self._decode_address(borrow_event["topics"][1])
        on_behalf_of = self._decode_address(borrow_event["topics"][2])

        # Check if GHO
        is_gho = reserve == GHO_TOKEN_ADDRESS

        # Find debt mint
        # For flash loan scenarios (same user, multiple mints in one tx),
        # we need to match by token address to ensure correct pairing
        debt_mint = None
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue

            if ev.user_address == on_behalf_of and any([
                is_gho and ev.event_type == "GHO_DEBT_MINT",
                not is_gho and ev.event_type == "DEBT_MINT",
            ]):
                # Skip interest accrual mints (amount == balance_increase)
                # These should be handled by INTEREST_ACCRUAL operations
                # Only match actual borrow mints (amount > balance_increase)
                if ev.amount == ev.balance_increase:
                    continue
                # Match by token address to handle flash loan scenarios
                # where same user has multiple mints for different assets
                # If debt_token_to_reserve mapping is provided, use it for matching
                if self.debt_token_to_reserve:
                    mint_token_address = get_checksum_address(ev.event["address"])
                    reserve_asset = self._get_reserve_for_debt_token(mint_token_address)
                    if reserve_asset and reserve_asset.lower() == reserve.lower():
                        debt_mint = ev
                        break
                else:
                    # Fallback to old behavior if mapping not provided
                    debt_mint = ev
                    break

        scaled_token_events = [debt_mint] if debt_mint else []
        op_type = OperationType.GHO_BORROW if is_gho else OperationType.BORROW

        # Also look for matching Transfer events from zero address (ERC20 mint)
        # These represent the same borrow operation
        transfer_events = []
        if debt_mint:
            for ev in scaled_events:
                if all([
                    ev.event_type == "DEBT_TRANSFER",
                    ev.from_address == ZERO_ADDRESS,
                    ev.target_address == on_behalf_of,
                    ev.amount == debt_mint.amount,
                    ev.event["address"] == debt_mint.event["address"],
                    ev.event["logIndex"] not in assigned_indices,
                ]):
                    transfer_events.append(ev.event)
                    break  # Only match one transfer per mint

        return Operation(
            operation_id=operation_id,
            operation_type=op_type,
            pool_event=borrow_event,
            scaled_token_events=scaled_token_events,
            transfer_events=transfer_events,
            balance_transfer_events=[],
        )

    def _create_repay_operation(
        self,
        operation_id: int,
        repay_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
    ) -> Operation:
        """Create REPAY operation."""
        # Decode REPAY event
        reserve = self._decode_address(repay_event["topics"][1])
        user = self._decode_address(repay_event["topics"][2])
        _, use_a_tokens = decode(["uint256", "bool"], _decode_hex_data(repay_event["data"]))

        is_gho = reserve == GHO_TOKEN_ADDRESS

        # Find debt burn (normal case)
        # For flash loan scenarios (same user, multiple burns in one tx),
        # we need to match by token address to ensure correct pairing
        debt_burn = None
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue

            if is_gho:
                if ev.event_type == "GHO_DEBT_BURN":
                    if ev.user_address == user:
                        debt_burn = ev
                        break
            elif ev.event_type in {"DEBT_BURN", "UNKNOWN_BURN"}:
                if ev.user_address == user:
                    # Match by token address to handle flash loan scenarios
                    # where same user has multiple burns for different assets
                    # If debt_token_to_reserve mapping is provided, use it for matching
                    if self.debt_token_to_reserve:
                        burn_token_address = get_checksum_address(ev.event["address"])
                        reserve_asset = self._get_reserve_for_debt_token(burn_token_address)
                        if reserve_asset and reserve_asset.lower() == reserve.lower():
                            debt_burn = ev
                            break
                    else:
                        # Fallback to old behavior if mapping not provided
                        debt_burn = ev
                        break

        scaled_token_events = [debt_burn] if debt_burn else []

        # If use_a_tokens, also look for collateral burn and corresponding BalanceTransfer
        balance_transfer_events: list[LogReceipt] = []
        if use_a_tokens and not is_gho:
            collateral_burn = None
            for ev in scaled_events:
                if ev.event["logIndex"] in assigned_indices:
                    continue
                if ev.event_type == "COLLATERAL_BURN" and ev.user_address == user:
                    collateral_burn = ev
                    break

            if collateral_burn:
                scaled_token_events.append(collateral_burn)
                # Note: We intentionally do NOT include debt_mint here.
                # The debt_mint is for interest accrual and should be a separate
                # operation or handled as unassigned, not grouped with this
                # repayWithATokens operation which should only have 1 debt event.

                # Also look for the corresponding BalanceTransfer event
                # When repaying with aTokens, the aTokens are transferred to the Pool
                # and then burned. The BalanceTransfer represents this internal transfer.
                for ev in scaled_events:
                    if ev.event["logIndex"] in assigned_indices:
                        continue
                    # Check if this BalanceTransfer is from the user
                    # and is for the same token as the collateral burn
                    if all([
                        ev.event_type == "COLLATERAL_TRANSFER",
                        ev.index > 0,
                        ev.from_address == user,
                        ev.event["address"] == collateral_burn.event["address"],
                    ]):
                        balance_transfer_events.append(ev.event)
                        break

                op_type = OperationType.REPAY_WITH_ATOKENS
            else:
                op_type = OperationType.REPAY
        else:
            op_type = OperationType.GHO_REPAY if is_gho else OperationType.REPAY

        # Also look for matching Transfer events to zero address (ERC20 burn)
        # These represent the same repay operation
        transfer_events = []
        if debt_burn:
            for ev in scaled_events:
                if all([
                    ev.event_type == "DEBT_TRANSFER",
                    ev.from_address == user,
                    ev.target_address == ZERO_ADDRESS,
                    ev.amount == debt_burn.amount,
                    ev.event["address"] == debt_burn.event["address"],
                    ev.event["logIndex"] not in assigned_indices,
                ]):
                    transfer_events.append(ev.event)
                    break  # Only match one transfer per burn

        return Operation(
            operation_id=operation_id,
            operation_type=op_type,
            pool_event=repay_event,
            scaled_token_events=scaled_token_events,
            transfer_events=transfer_events,
            balance_transfer_events=balance_transfer_events,
        )

    def _create_liquidation_operation(
        self,
        operation_id: int,
        liquidation_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
    ) -> Operation:
        """Create LIQUIDATION operation."""
        # Decode LIQUIDATION_CALL
        _collateral_asset = self._decode_address(liquidation_event["topics"][1])
        debt_asset = self._decode_address(liquidation_event["topics"][2])
        user = self._decode_address(liquidation_event["topics"][3])

        is_gho = debt_asset == GHO_TOKEN_ADDRESS

        # Find debt burn (standard liquidation)
        debt_burn = None
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue

            if ev.user_address == user and any([
                is_gho and ev.event_type == "GHO_DEBT_BURN",
                not is_gho and ev.event_type in {"DEBT_BURN", "UNKNOWN_BURN"},
            ]):
                debt_burn = ev
                break

        # Find collateral burn and/or transfer(s)
        # During liquidations, borrower may have BOTH collateral burned AND multiple transfers
        collateral_burn = None
        collateral_transfers: list[ScaledTokenEvent] = []
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue

            if ev.event_type in {"COLLATERAL_BURN", "UNKNOWN_BURN"}:
                if ev.user_address == user and not collateral_burn:
                    collateral_burn = ev
            elif ev.event_type == "COLLATERAL_TRANSFER":
                if ev.user_address == user:
                    collateral_transfers.append(ev)
            elif ev.event_type == "UNKNOWN_TRANSFER" and ev.user_address == user:
                # Fallback: when token address isn't in mapping, BalanceTransfer
                # decodes as UNKNOWN_TRANSFER. Match by user address - if the
                # transfer is FROM the liquidated user, it's likely collateral.
                collateral_transfers.append(ev)

        scaled_token_events: list[ScaledTokenEvent] = []
        balance_transfer_events: list[LogReceipt] = []
        if debt_burn:
            scaled_token_events.append(debt_burn)
        if collateral_burn:
            scaled_token_events.append(collateral_burn)
        if collateral_transfers:
            # Add all collateral transfers to scaled_token_events
            # Both ERC20 Transfers (index=0) and BalanceTransfer events (index>0)
            # are collateral events that should be validated together
            for transfer in collateral_transfers:
                scaled_token_events.append(transfer)
                # Track BalanceTransfer events separately so ERC20 Transfers can use
                # them for proper scaling during processing
                if transfer.index > 0:
                    balance_transfer_events.append(transfer.event)

        op_type = OperationType.GHO_LIQUIDATION if is_gho else OperationType.LIQUIDATION

        return Operation(
            operation_id=operation_id,
            operation_type=op_type,
            pool_event=liquidation_event,
            scaled_token_events=scaled_token_events,
            transfer_events=[],
            balance_transfer_events=balance_transfer_events,
        )

    def _create_deficit_operation(
        self,
        operation_id: int,
        deficit_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
    ) -> Operation:
        """Create DEFICIT_CREATED operation.

        DEFICIT_CREATED indicates bad debt write-off. When the asset is GHO,
        it's a GHO flash loan that requires a debt burn. For other assets,
        it's a standalone deficit event with no associated debt burn.

        Note: DEFICIT_CREATED can also be emitted during GHO liquidations as
        part of the bad debt write-off mechanism. In such cases, the GHO debt
        burn should be matched to the LIQUIDATION_CALL operation, not a
        separate flash loan operation.
        """
        user = self._decode_address(deficit_event["topics"][1])
        asset = self._decode_address(deficit_event["topics"][2])

        # Check if this is a GHO deficit (flash loan) or non-GHO deficit
        is_gho_deficit = asset == GHO_TOKEN_ADDRESS

        # Check if there's a LIQUIDATION_CALL for the same user in this transaction
        # If so, this DEFICIT_CREATED is part of the liquidation, not a standalone flash loan
        has_liquidation_for_user = False
        for event in all_events:
            if event["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value:
                liquidation_user = self._decode_address(event["topics"][3])
                if liquidation_user == user:
                    has_liquidation_for_user = True
                    break

        scaled_token_events: list[ScaledTokenEvent] = []
        if is_gho_deficit and not has_liquidation_for_user:
            # Find GHO debt burn for GHO flash loans only if not part of liquidation
            for ev in scaled_events:
                if ev.event["logIndex"] in assigned_indices:
                    continue

                if ev.event_type == "GHO_DEBT_BURN" and ev.user_address == user:
                    scaled_token_events.append(ev)
                    break

        # If this DEFICIT_CREATED is part of a liquidation, mark it as UNKNOWN
        # so it doesn't interfere with liquidation processing
        if is_gho_deficit and has_liquidation_for_user:
            operation_type = OperationType.UNKNOWN
        elif is_gho_deficit:
            operation_type = OperationType.GHO_FLASH_LOAN
        else:
            operation_type = OperationType.UNKNOWN

        return Operation(
            operation_id=operation_id,
            operation_type=operation_type,
            pool_event=deficit_event,
            scaled_token_events=scaled_token_events,
            transfer_events=[],
            balance_transfer_events=[],
        )

    def _create_interest_accrual_operations(
        self,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        starting_operation_id: int,
        all_events: list[LogReceipt],
    ) -> list[Operation]:
        """Create INTEREST_ACCRUAL operations for unassigned interest events.

        Interest accrual events are mints where amount == balance_increase.
        These represent pure interest accrual with no corresponding pool event.
        When an ERC20 Transfer event exists for the same interest (from ZERO_ADDRESS),
        it is paired with the Mint event to avoid double-counting.

        Also handles unassigned debt burn events to ensure debt balances are properly
        reduced when burn events don't match REPAY operations (e.g., flash loans).

        Args:
            scaled_events: All scaled token events from the transaction
            assigned_indices: Set of log indices already assigned to operations
            starting_operation_id: The next available operation ID
            all_events: All events from the transaction (to check for LIQUIDATION_CALL)

        Returns:
            List of INTEREST_ACCRUAL operations
        """
        # Check for pool events that indicate complex transactions
        # In these transactions, DEBT_MINT events may be associated with operations, not interest
        has_liquidation = any(
            ev["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value for ev in all_events
        )
        has_borrow = any(ev["topics"][0] == AaveV3PoolEvent.BORROW.value for ev in all_events)
        has_repay = any(ev["topics"][0] == AaveV3PoolEvent.REPAY.value for ev in all_events)
        # Check if there's a collateral burn in the transaction (indicates repayWithATokens)
        has_collateral_burn = any(ev.event_type == "COLLATERAL_BURN" for ev in scaled_events)
        operations: list[Operation] = []
        operation_id = starting_operation_id
        local_assigned: set[int] = set()

        for ev in scaled_events:
            # Skip already assigned events
            if ev.event["logIndex"] in assigned_indices or ev.event["logIndex"] in local_assigned:
                continue

            # Handle unassigned debt burn events
            # These can occur in flash loans or other edge cases where a burn
            # doesn't match a REPAY operation but should still reduce debt
            if ev.event_type in {"DEBT_BURN", "GHO_DEBT_BURN"}:
                operations.append(
                    Operation(
                        operation_id=operation_id,
                        operation_type=OperationType.INTEREST_ACCRUAL,
                        pool_event=None,
                        scaled_token_events=[ev],
                        transfer_events=[],
                        balance_transfer_events=[],
                    )
                )
                operation_id += 1
                continue

            # Only process mint events that represent interest accrual
            if ev.event_type not in {"COLLATERAL_MINT", "DEBT_MINT", "GHO_DEBT_MINT"}:
                continue

            # Skip DEBT_MINT in liquidation transactions - these may be
            # protocol operations that should not create INTEREST_ACCRUAL.
            # For borrow/repay transactions, only skip if it's not interest
            # accrual (balance_increase >= amount). Interest accrual can occur
            # during any transaction type including flash loans.
            if ev.event_type in {"DEBT_MINT", "GHO_DEBT_MINT"}:
                # Interest accrual: balance_increase >= amount
                # - balance_increase > amount: net interest after repayment (in _burnScaled)
                # - balance_increase == amount: pure interest accrual (in _accrueDebtOnAction)
                # Always process interest accrual, regardless of transaction type
                is_interest_accrual = ev.balance_increase >= ev.amount
                if is_interest_accrual:
                    # Process as INTEREST_ACCRUAL
                    pass
                elif has_liquidation:
                    # Skip non-interest mints during liquidation (e.g., flash borrows)
                    continue
                elif has_borrow:
                    # Skip DEBT_MINT during borrow (flash loan) if not interest accrual
                    continue
                elif has_repay and not has_collateral_burn:
                    # Skip DEBT_MINT during REPAY if not interest accrual
                    continue

            # Interest accrual: process all unassigned mint events
            # Note: For pure interest, amount == balance_increase
            # But sometimes amount < balance_increase (small deposit + interest)
            # Include dust mints (balance_increase == 0) which still need to update last_index
            # Look for matching Transfer event from ZERO_ADDRESS (interest accrual)
            # For mints from ZERO_ADDRESS, the target_address is the recipient (user)
            # Match by amount if it's a pure interest accrual, or by target_address if there's a deposit
            transfer_events = []
            for transfer_ev in scaled_events:
                if (
                    transfer_ev.event_type == "COLLATERAL_TRANSFER"
                    and transfer_ev.from_address == ZERO_ADDRESS
                    and transfer_ev.target_address == ev.user_address
                    and transfer_ev.event["address"] == ev.event["address"]
                    and transfer_ev.event["logIndex"] not in assigned_indices
                    and transfer_ev.event["logIndex"] not in local_assigned
                ):
                    # For pure interest, Transfer amount matches Mint amount
                    # For deposit + interest, Transfer amount may be less than Mint amount
                    if transfer_ev.amount == ev.amount or transfer_ev.amount < ev.amount:
                        transfer_events.append(transfer_ev.event)
                        local_assigned.add(transfer_ev.event["logIndex"])
                        break  # Only match one transfer per mint

            operations.append(
                Operation(
                    operation_id=operation_id,
                    operation_type=OperationType.INTEREST_ACCRUAL,
                    pool_event=None,
                    scaled_token_events=[ev],
                    transfer_events=transfer_events,
                    balance_transfer_events=[],
                )
            )
            operation_id += 1

        return operations

    def _create_mint_to_treasury_operations(
        self,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        starting_operation_id: int,
    ) -> list[Operation]:
        """Create MINT_TO_TREASURY operations for unassigned scaled token mints to the Pool.

        When the Pool contract calls mintToTreasury(), it emits ScaledTokenMint events
        where the caller_address is the Pool itself. These represent protocol reserves being
        minted to the treasury and should be treated as SUPPLY operations for the Pool.

        Args:
            scaled_events: All scaled token events from the transaction
            assigned_indices: Set of log indices already assigned to operations
            starting_operation_id: The next available operation ID

        Returns:
            List of MINT_TO_TREASURY operations
        """
        operations: list[Operation] = []
        operation_id = starting_operation_id

        if not self.pool_address:
            return operations

        for ev in scaled_events:
            # Skip already assigned events
            if ev.event["logIndex"] in assigned_indices:
                continue

            # Only process collateral mints where the caller is the Pool contract
            if ev.event_type != "COLLATERAL_MINT":
                continue

            # Check if caller is the Pool (mintToTreasury calls have Pool as caller)
            if ev.caller_address != self.pool_address:
                continue

            # This is a mint to treasury - create operation
            logger.debug(
                f"Creating MINT_TO_TREASURY for event at logIndex {ev.event['logIndex']}, "
                f"user={ev.user_address}, amount={ev.amount}"
            )
            operations.append(
                Operation(
                    operation_id=operation_id,
                    operation_type=OperationType.MINT_TO_TREASURY,
                    pool_event=None,
                    scaled_token_events=[ev],
                    transfer_events=[],
                    balance_transfer_events=[],
                )
            )
            operation_id += 1

        return operations

    def _create_transfer_operations(
        self,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        starting_operation_id: int,
        existing_operations: list[Operation],
    ) -> list[Operation]:
        """Create TRANSFER operations for unassigned transfer events.

        Transfer events (ERC20 Transfer for aTokens/vTokens) are standalone
        and don't have corresponding pool events. When both an ERC20 Transfer
        and a BalanceTransfer event exist for the same transfer, they are paired
        together and the BalanceTransfer amount (which includes interest) is used.

        During liquidations, the BalanceTransfer event may already be assigned to
        the LIQUIDATION operation (as the collateral transfer from borrower to
        liquidator). In this case, we look for it in the existing operations.

        Args:
            scaled_events: All scaled token events from the transaction
            assigned_indices: Set of log indices already assigned to operations
            starting_operation_id: The next available operation ID
            existing_operations: Operations already created (for finding BalanceTransfer events
                               that were assigned to LIQUIDATION operations)

        Returns:
            List of TRANSFER operations for unassigned transfer events
        """
        operations: list[Operation] = []
        operation_id = starting_operation_id
        local_assigned: set[int] = set()  # Track assignments within this function

        for ev in scaled_events:
            # Skip already assigned events (both externally and locally)
            if ev.event["logIndex"] in assigned_indices or ev.event["logIndex"] in local_assigned:
                continue

            # Only process transfer events
            if ev.event_type not in {
                "COLLATERAL_TRANSFER",
                "DEBT_TRANSFER",
                "GHO_DEBT_TRANSFER",
            }:
                continue

            # Check if this is an ERC20 Transfer event (index=0 means no index from event)
            # BalanceTransfer events have index > 0
            is_erc20_transfer = ev.index == 0

            balance_transfer_event: ScaledTokenEvent | None = None

            if is_erc20_transfer:
                # Look for a corresponding BalanceTransfer event
                # BalanceTransfer events are decoded from SCALED_TOKEN_BALANCE_TRANSFER topic
                # and have index > 0
                for bt_ev in scaled_events:
                    if (
                        bt_ev.event["logIndex"] in assigned_indices
                        or bt_ev.event["logIndex"] in local_assigned
                    ):
                        continue

                    # Check if this is a BalanceTransfer event (has index > 0)
                    if bt_ev.index == 0:
                        continue

                    # Check if from/to addresses match and it's the same token
                    if (
                        bt_ev.from_address == ev.from_address
                        and bt_ev.target_address == ev.target_address
                        and bt_ev.event["address"] == ev.event["address"]
                        and bt_ev.event_type == ev.event_type
                    ):
                        # Found matching BalanceTransfer
                        balance_transfer_event = bt_ev
                        local_assigned.add(bt_ev.event["logIndex"])
                        break

                # If not found in unassigned events, check existing operations
                # (e.g., BalanceTransfer assigned to LIQUIDATION operation)
                if balance_transfer_event is None:
                    for op in existing_operations:
                        for bt_ev in op.scaled_token_events:
                            # Check if this is a BalanceTransfer event (has index > 0)
                            if bt_ev.index == 0:
                                continue

                            # Check if from/to addresses match and it's the same token
                            if (
                                bt_ev.from_address == ev.from_address
                                and bt_ev.target_address == ev.target_address
                                and bt_ev.event["address"] == ev.event["address"]
                                and bt_ev.event_type == ev.event_type
                            ):
                                # Found matching BalanceTransfer in existing operation
                                balance_transfer_event = bt_ev
                                break
                        if balance_transfer_event:
                            break

            # Create TRANSFER operation with both events if found
            balance_transfer_events = []
            if balance_transfer_event:
                balance_transfer_events.append(balance_transfer_event.event)

            operations.append(
                Operation(
                    operation_id=operation_id,
                    operation_type=OperationType.BALANCE_TRANSFER,
                    pool_event=None,
                    scaled_token_events=[ev],
                    transfer_events=[],
                    balance_transfer_events=balance_transfer_events,
                )
            )
            operation_id += 1

        # Update the assigned_indices set with locally assigned events
        assigned_indices.update(local_assigned)

        return operations

    def _validate_operation(self, op: Operation) -> None:
        """Strict validation of operation completeness."""
        errors = []

        validators = {
            OperationType.SUPPLY: self._validate_supply,
            OperationType.WITHDRAW: self._validate_withdraw,
            OperationType.BORROW: self._validate_borrow,
            OperationType.GHO_BORROW: self._validate_gho_borrow,
            OperationType.REPAY: self._validate_repay,
            OperationType.REPAY_WITH_ATOKENS: self._validate_repay_with_atokens,
            OperationType.GHO_REPAY: self._validate_gho_repay,
            OperationType.LIQUIDATION: self._validate_liquidation,
            OperationType.GHO_LIQUIDATION: self._validate_gho_liquidation,
            OperationType.GHO_FLASH_LOAN: self._validate_flash_loan,
            OperationType.INTEREST_ACCRUAL: self._validate_interest_accrual,
            OperationType.BALANCE_TRANSFER: self._validate_balance_transfer,
            OperationType.MINT_TO_TREASURY: self._validate_mint_to_treasury,
        }

        validator = validators.get(op.operation_type)
        if validator:
            errors.extend(validator(op))

        # Set validation errors
        object.__setattr__(op, "validation_errors", errors)

    def _validate_supply(self, op: Operation) -> list[str]:
        """Validate SUPPLY operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing SUPPLY pool event")
            return errors

        # Should have exactly 1 collateral mint
        collateral_mints = [e for e in op.scaled_token_events if e.is_collateral]
        if len(collateral_mints) != 1:
            errors.append(f"Expected 1 collateral mint for SUPPLY, got {len(collateral_mints)}")

        return errors

    def _validate_withdraw(self, op: Operation) -> list[str]:
        """Validate WITHDRAW operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing WITHDRAW pool event")
            return errors

        # Should have exactly 1 collateral burn
        # Edge case: In complex vault/strategy transactions, a WITHDRAW may not have
        # a corresponding Burn event if the collateral is handled through an adapter
        # or intermediate contract.
        # See TX 0xe6811c1ee3be2981338d910c6e421d092b4f6e3c0b763a6319b2b7cd731e2fb9
        # Note: WITHDRAW can have both a burn (primary) and interest accrual mints
        collateral_burns = [e for e in op.scaled_token_events if e.is_collateral and e.is_burn]
        if len(collateral_burns) > 1:
            errors.append(
                f"Expected at most 1 collateral burn for WITHDRAW, got {len(collateral_burns)}"
            )
        # Note: len(collateral_burns) == 0 is allowed for edge cases like vault rebalances
        # where collateral may be handled through flash loans or adapter contracts

        return errors

    def _validate_borrow(self, op: Operation) -> list[str]:
        """Validate BORROW operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing BORROW pool event")
            return errors

        # Should have exactly 1 debt mint
        debt_mints = [e for e in op.scaled_token_events if e.is_debt]
        if len(debt_mints) != 1:
            errors.append(f"Expected 1 debt mint for BORROW, got {len(debt_mints)}")

        return errors

    def _validate_gho_borrow(self, op: Operation) -> list[str]:
        """Validate GHO BORROW operation."""
        errors = self._validate_borrow(op)

        # Additional GHO-specific validation
        gho_mints = [e for e in op.scaled_token_events if e.event_type == "GHO_DEBT_MINT"]
        if len(gho_mints) != 1:
            errors.append(f"Expected 1 GHO debt mint for GHO_BORROW, got {len(gho_mints)}")

        return errors

    def _validate_repay(self, op: Operation) -> list[str]:
        """Validate REPAY operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing REPAY pool event")
            return errors

        # Can have 0 or 1 debt burns (0 = interest-only repayment, 1 = principal repayment)
        debt_burns = [e for e in op.scaled_token_events if e.is_debt]
        if len(debt_burns) > 1:
            errors.append(f"Expected 0 or 1 debt burns for REPAY, got {len(debt_burns)}")

        return errors

    def _validate_repay_with_atokens(self, op: Operation) -> list[str]:
        """Validate REPAY_WITH_ATOKENS operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing REPAY pool event")
            return errors

        # Should have 0 or 1 debt events (burn or mint) and 1 collateral burn
        # Note: When interest exceeds repayment, debt mints instead of burns
        # Note: In some edge cases, debt burn may not be emitted if debt is fully covered by interest
        debt_events = [e for e in op.scaled_token_events if e.is_debt]
        collateral_burns = [e for e in op.scaled_token_events if e.is_collateral and e.is_burn]

        if len(debt_events) > 1:
            errors.append(
                f"Expected 0 or 1 debt events for REPAY_WITH_ATOKENS, got {len(debt_events)}"
            )
        if len(collateral_burns) != 1:
            errors.append(
                f"Expected 1 collateral burn for REPAY_WITH_ATOKENS, got {len(collateral_burns)}"
            )

        return errors

    def _validate_gho_repay(self, op: Operation) -> list[str]:
        """Validate GHO REPAY operation."""
        errors = self._validate_repay(op)

        # GHO repay can emit either BURN (debt reduction) or MINT (interest > repayment)
        # When interest accrued exceeds repayment amount, the debt token mints instead of burns
        gho_events = [
            e for e in op.scaled_token_events if e.event_type in {"GHO_DEBT_BURN", "GHO_DEBT_MINT"}
        ]
        if len(gho_events) > 1:
            errors.append(f"Expected 0 or 1 GHO debt event for GHO_REPAY, got {len(gho_events)}")

        return errors

    def _validate_liquidation(self, op: Operation) -> list[str]:
        """Validate LIQUIDATION operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing LIQUIDATION_CALL pool event")
            return errors

        # Should have 1 collateral event (burn or transfer) and 0 or 1 debt burns
        # Flash loan liquidations have 0 debt burns (debt repaid via flash loan)
        # Standard liquidations have 1 debt burn
        # Collateral may be burned OR transferred to treasury (BalanceTransfer)
        debt_burns = [e for e in op.scaled_token_events if e.is_debt]
        collateral_events = [e for e in op.scaled_token_events if e.is_collateral]

        if len(debt_burns) > 1:
            errors.append(
                f"Expected 0 or 1 debt burns for LIQUIDATION, got {len(debt_burns)}. "
                f"DEBUG NOTE: Check if debt/collateral events are being assigned to wrong operations. "
                f"Current debt burns: {[e.event['logIndex'] for e in debt_burns]}. "
                f"User in LIQUIDATION_CALL: {self._decode_address(op.pool_event['topics'][3])}"
            )

        if len(collateral_events) < 1:
            errors.append(
                f"Expected at least 1 collateral event (burn or transfer) for LIQUIDATION, got {len(collateral_events)}. "
                f"DEBUG NOTE: Check collateral asset matching and user address consistency. "
                f"Current collateral events: {[e.event['logIndex'] for e in collateral_events]}. "
                f"User in LIQUIDATION_CALL: {self._decode_address(op.pool_event['topics'][3])}"
            )

        return errors

    def _validate_gho_liquidation(self, op: Operation) -> list[str]:
        """Validate GHO LIQUIDATION operation."""
        errors = self._validate_liquidation(op)

        # Additional GHO-specific validation
        gho_burns = [e for e in op.scaled_token_events if e.event_type == "GHO_DEBT_BURN"]
        if len(gho_burns) > 1:
            errors.append(
                f"Expected 0 or 1 GHO debt burn for GHO_LIQUIDATION, got {len(gho_burns)}. "
                f"DEBUG NOTE: Dust liquidations may have 0 burns (zero debt to cover)."
            )

        return errors

    def _validate_flash_loan(self, op: Operation) -> list[str]:
        """Validate FLASH_LOAN (DEFICIT_CREATED) operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing DEFICIT_CREATED pool event")
            return errors

        # Should have exactly 1 GHO debt burn
        gho_burns = [e for e in op.scaled_token_events if e.event_type == "GHO_DEBT_BURN"]
        if len(gho_burns) != 1:
            errors.append(
                f"Expected 1 GHO debt burn for FLASH_LOAN, got {len(gho_burns)}. "
                f"DEBUG NOTE: Flash loans should have exactly one debt burn."
            )

        return errors

    def _validate_interest_accrual(self, op: Operation) -> list[str]:
        """Validate INTEREST_ACCRUAL operation.

        Interest accrual operations have no pool event. The scaled token event
        represents pure interest accrual where amount == balance_increase.
        Also includes dust mints (balance_increase == 0) from discount updates
        that still need to update the user's last_index.
        """
        errors = []

        # Should have no pool event (interest accrual is standalone)
        if op.pool_event is not None:
            errors.append("INTEREST_ACCRUAL should not have a pool event")

        # Should have exactly 1 scaled token event (the mint)
        if len(op.scaled_token_events) != 1:
            errors.append(
                f"Expected 1 scaled token event for INTEREST_ACCRUAL, got {len(op.scaled_token_events)}"
            )

        # Allow both interest accrual (balance_increase > 0) and dust mints (balance_increase == 0)
        # Dust mints occur during discount updates and still need to update last_index

        return errors

    def _validate_balance_transfer(self, op: Operation) -> list[str]:
        """Validate BALANCE_TRANSFER operation."""
        errors = []

        # Should have no pool event (transfers are standalone)
        if op.pool_event is not None:
            errors.append("BALANCE_TRANSFER should not have a pool event")

        # Should have exactly 1 scaled token event (the transfer)
        if len(op.scaled_token_events) != 1:
            errors.append(
                f"Expected 1 scaled token event for BALANCE_TRANSFER, got {len(op.scaled_token_events)}"
            )

        # The event should be a transfer event
        if op.scaled_token_events:
            ev = op.scaled_token_events[0]
            if ev.event_type not in {"COLLATERAL_TRANSFER", "DEBT_TRANSFER", "GHO_DEBT_TRANSFER"}:
                errors.append(
                    f"BALANCE_TRANSFER event should be a transfer type, got {ev.event_type}"
                )

        return errors

    def _validate_mint_to_treasury(self, op: Operation) -> list[str]:
        """Validate MINT_TO_TREASURY operation."""
        errors = []

        # Should have no pool event (treasury mints are standalone)
        if op.pool_event is not None:
            errors.append("MINT_TO_TREASURY should not have a pool event")

        # Should have exactly 1 scaled token event (the mint)
        if len(op.scaled_token_events) != 1:
            errors.append(
                f"Expected 1 scaled token event for MINT_TO_TREASURY, got {len(op.scaled_token_events)}"
            )

        # The event should be a collateral mint
        if op.scaled_token_events:
            ev = op.scaled_token_events[0]
            if ev.event_type != "COLLATERAL_MINT":
                errors.append(
                    f"MINT_TO_TREASURY event should be COLLATERAL_MINT, got {ev.event_type}"
                )

        return errors

    def _decode_address(self, topic: HexBytes | str) -> ChecksumAddress:
        """Decode topic as address."""
        return _topic_to_address(topic)


# ============================================================================
# TRANSACTION OPERATIONS CONTAINER
# ============================================================================


class TransactionOperations:
    """Container for all operations in a transaction."""

    def __init__(
        self,
        tx_hash: HexBytes,
        block_number: int,
        operations: list[Operation],
        unassigned_events: list[LogReceipt],
    ):
        self.tx_hash = tx_hash
        self.block_number = block_number
        self.operations = operations
        self.unassigned_events = unassigned_events

    def validate(self, all_events: list[LogReceipt]) -> None:
        """Strict validation - fails on any unmet expectation."""
        all_errors = []

        # Check all operations are valid
        for op in self.operations:
            if not op.is_valid():
                all_errors.extend([
                    f"Operation {op.operation_id} ({op.operation_type.name}): {err}"
                    for err in op.validation_errors
                ])

        # Check for unassigned required events
        required_unassigned = [e for e in self.unassigned_events if self._is_required_pool_event(e)]
        if required_unassigned:
            all_errors.append(
                f"{len(required_unassigned)} required pool events unassigned: "
                f"{[e['logIndex'] for e in required_unassigned]}. "
                f"DEBUG NOTE: Investigate why these events were not assigned to any operation. "
                f"They may need special handling or indicate a parsing bug."
            )

        # Check for ambiguous event assignments
        assigned_indices: dict[int, int] = {}  # logIndex -> operation_id
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

        if all_errors:
            raise TransactionValidationError(
                message="Transaction validation failed:\n" + "\n".join(all_errors),
                tx_hash=self.tx_hash,
                events=all_events,
                operations=self.operations,
            )

    def _is_required_pool_event(self, event: LogReceipt) -> bool:
        """Check if an event must be part of an operation."""
        pool_topics = {
            AaveV3PoolEvent.SUPPLY.value,
            AaveV3PoolEvent.WITHDRAW.value,
            AaveV3PoolEvent.BORROW.value,
            AaveV3PoolEvent.REPAY.value,
            AaveV3PoolEvent.LIQUIDATION_CALL.value,
            AaveV3PoolEvent.DEFICIT_CREATED.value,
        }
        return event["topics"][0] in pool_topics

    def get_operation_for_event(self, event: LogReceipt) -> Operation | None:
        """Find which operation contains a given event."""
        target_log_index = event["logIndex"]
        for op in self.operations:
            if target_log_index in op.get_event_log_indices():
                return op
        return None
