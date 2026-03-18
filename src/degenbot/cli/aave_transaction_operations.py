"""
Parses transaction events into logical operations based on asset flows.
Provides strict validation with detailed plain-text error reporting.
"""

import operator
from dataclasses import dataclass, field
from enum import Enum, auto

import eth_abi.abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from sqlalchemy.orm import Session
from web3.types import LogReceipt

from degenbot.aave.events import (
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    ERC20Event,
    ScaledTokenEventType,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave_types import TokenType
from degenbot.cli.aave_utils import decode_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.database.models.aave import AaveGhoToken, AaveV3Asset, AaveV3Contract, AaveV3Market
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.logging import logger

# Token amount matching tolerance for ray math rounding differences
# Pool revision 9+ uses flooring ray division which can introduce ±2 wei variance
TOKEN_AMOUNT_MATCH_TOLERANCE = 2
SCALED_AMOUNT_POOL_REVISION = 9


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
    IMPLICIT_BORROW = (
        auto()
    )  # DEBT_MINT without BORROW event (e.g., flash loans, internal operations)
    STKAAVE_TRANSFER = auto()  # stkAAVE (GHO Discount Token) transfer
    UNKNOWN = auto()


@dataclass(frozen=True)
class ScaledTokenEvent:
    """
    Wrapper for scaled token events with human-readable decoded data.
    """

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
        return self.event_type in {
            ScaledTokenEventType.COLLATERAL_BURN,
            ScaledTokenEventType.COLLATERAL_MINT,
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
            ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
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
        """Get all log indices involved in this operation."""
        return [e["logIndex"] for e in self.get_all_events()]


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
        if len(data_str) > 60:  # noqa:PLR2004
            data_str = data_str[:30] + "..." + data_str[-30:]
        lines.extend((f"    Data: {data_str}", ""))

        return lines

    @staticmethod
    def _format_operation(op: Operation) -> list[str]:
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

    @staticmethod
    def _get_event_name(topic: HexBytes) -> str:
        """Get human-readable event name from topic."""
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
        """Try to decode topic as address.

        Handles both HexBytes objects (with .hex() method) and strings.
        """
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

        # Check for unassigned scaled token events (Burn, Mint, BalanceTransfer)
        # These should always be matched to an operation, not left for INTEREST_ACCRUAL
        scaled_token_topics = {
            AaveV3ScaledTokenEvent.BURN.value,
            AaveV3ScaledTokenEvent.MINT.value,
            AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value,
        }
        unassigned_scaled = [
            e for e in self.unassigned_events if e["topics"][0] in scaled_token_topics
        ]
        if unassigned_scaled:
            all_errors.append(
                f"{len(unassigned_scaled)} scaled token events "
                f"(Burn/Mint/BalanceTransfer) unassigned: "
                f"{[e['logIndex'] for e in unassigned_scaled]}. "
                f"DEBUG NOTE: All scaled token events must be matched to operations. "
                f"Unassigned burns/mints/transfers indicate a matching bug."
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

    @staticmethod
    def _is_required_pool_event(event: LogReceipt) -> bool:
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


class TransactionOperationsParser:
    """Parses transaction events into logical operations."""

    def __init__(
        self,
        market: AaveV3Market,
        session: Session,
        pool_address: ChecksumAddress,
        treasury_address: ChecksumAddress | None = None,
    ) -> None:
        """
        Initialize parser.

        Args:
            market: Aave V3 market with assets containing aToken and vToken relationships.
            session: SQLAlchemy session for database queries.
            pool_address: Address of the Aave Pool contract. Used to detect mintToTreasury
                operations.
            treasury_address: Address of the Aave treasury. If not provided, will attempt to
                retrieve from the first aToken in the market.
        """

        self.market = market
        self.session = session
        self.pool_address = pool_address
        self.treasury_address = treasury_address or self._get_default_treasury_address()

        gho_asset = self._get_gho_asset()
        self.gho_token_address = gho_asset.token.address
        self.gho_vtoken_address = (
            gho_asset.v_token.address if gho_asset.v_token is not None else None
        )

    def _get_default_treasury_address(self) -> ChecksumAddress:
        """Get default treasury address for known markets."""
        # Known treasury addresses for major Aave markets
        known_treasuries: dict[int, ChecksumAddress] = {
            1: get_checksum_address("0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c"),  # Ethereum
        }

        if self.market.chain_id in known_treasuries:
            return known_treasuries[self.market.chain_id]

        msg = (
            f"Unknown treasury address for chain {self.market.chain_id}. "
            f"Please provide treasury_address parameter to TransactionOperationsParser."
        )
        raise ValueError(msg)

    def _get_gho_asset(self) -> AaveGhoToken:
        """Get GHO token asset for the current market."""

        gho_asset = self.session.scalar(
            select(AaveGhoToken)
            .join(AaveGhoToken.token)
            .where(Erc20TokenTable.chain == self.market.chain_id)
        )
        if gho_asset is None:
            msg = (
                f"GHO token not found for chain {self.market.chain_id}. "
                "Ensure that market has been activated."
            )
            raise ValueError(msg)
        return gho_asset

    def _get_token_type(self, token_address: ChecksumAddress) -> TokenType | None:
        """
        Get token type (aToken or vToken) for a given token address.

        Queries the database directly to avoid stale ORM relationship cache issues.

        Args:
            token_address: The token address to look up.

        Returns:
            TokenType.A_TOKEN, TokenType.V_TOKEN, TokenType.GHO_DISCOUNT or None if not found.
        """

        token_address = get_checksum_address(token_address)

        # Query database directly to avoid stale ORM cache
        # Check for aToken match

        if (
            self.session.scalar(
                select(AaveV3Asset)
                .join(AaveV3Asset.a_token)
                .where(
                    AaveV3Asset.market_id == self.market.id,
                    Erc20TokenTable.address == token_address,
                )
            )
            is not None
        ):
            return TokenType.A_TOKEN

        # Check for vToken match
        if (
            self.session.scalar(
                select(AaveV3Asset)
                .join(AaveV3Asset.v_token)
                .where(
                    AaveV3Asset.market_id == self.market.id,
                    Erc20TokenTable.address == token_address,
                )
            )
            is not None
        ):
            return TokenType.V_TOKEN

        # Check for GHO Discount Token
        gho_asset = self.session.scalar(
            select(AaveGhoToken)
            .join(AaveGhoToken.token)
            .where(Erc20TokenTable.chain == self.market.chain_id)
        )
        if gho_asset is not None and token_address == gho_asset.v_gho_discount_token:
            return TokenType.GHO_DISCOUNT

        return None

    def _get_reserve_for_debt_token(
        self, debt_token_address: ChecksumAddress
    ) -> ChecksumAddress | None:
        """Get the underlying reserve address for a debt token.

        Queries the database directly to avoid stale ORM relationship cache issues.

        Args:
            debt_token_address: The debt token (vToken) address.

        Returns:
            The underlying reserve asset address, or None if not found.
        """
        checksum_addr = get_checksum_address(debt_token_address)

        # Query database directly to avoid stale ORM cache
        asset = self.session.scalar(
            select(AaveV3Asset)
            .join(AaveV3Asset.v_token)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == checksum_addr,
            )
        )
        if asset is not None:
            return get_checksum_address(asset.underlying_token.address)
        return None

    def _get_a_token_for_asset(self, underlying_asset: ChecksumAddress) -> ChecksumAddress | None:
        """Get the aToken address for an underlying asset.

        Queries the database directly to avoid stale ORM relationship cache issues.

        Args:
            underlying_asset: The underlying asset address.

        Returns:
            The aToken contract address, or None if not found.
        """
        checksum_addr = get_checksum_address(underlying_asset)

        # Query database directly to avoid stale ORM cache
        asset = self.session.scalar(
            select(AaveV3Asset)
            .join(AaveV3Asset.underlying_token)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == checksum_addr,
            )
        )
        if asset is not None and asset.a_token is not None:
            return get_checksum_address(asset.a_token.address)
        return None

    def _get_v_token_for_asset(self, underlying_asset: ChecksumAddress) -> ChecksumAddress | None:
        """Get the vToken address for an underlying asset.

        Queries the database directly to avoid stale ORM relationship cache issues.

        Args:
            underlying_asset: The underlying asset address.

        Returns:
            The vToken contract address, or None if not found.
        """
        checksum_addr = get_checksum_address(underlying_asset)

        # Query database directly to avoid stale ORM cache
        asset = self.session.scalar(
            select(AaveV3Asset)
            .join(AaveV3Asset.underlying_token)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == checksum_addr,
            )
        )
        if asset is not None and asset.v_token is not None:
            return get_checksum_address(asset.v_token.address)
        return None

    def _get_pool_revision(self) -> int:
        """
        Get the Pool contract revision from the market.
        """

        pool_contract = self.session.scalar(
            select(AaveV3Contract).where(
                AaveV3Contract.market_id == self.market.id,
                AaveV3Contract.name == "POOL",
            )
        )
        assert pool_contract is not None
        assert pool_contract.revision is not None
        return pool_contract.revision

    def _get_a_token_asset_by_reserve(self, reserve_address: ChecksumAddress) -> AaveV3Asset | None:
        """
        Get the aToken asset for a given reserve address.
        """

        checksum_addr = get_checksum_address(reserve_address)

        return self.session.scalar(
            select(AaveV3Asset)
            .join(AaveV3Asset.underlying_token)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == checksum_addr,
            )
        )

    def _get_asset_by_token(
        self,
        token_address: ChecksumAddress,
        token_type: TokenType,
    ) -> AaveV3Asset | None:
        """
        Get the asset for a given token address.
        """

        checksum_addr = get_checksum_address(token_address)

        # Select the appropriate relationship based on token type
        if token_type == TokenType.A_TOKEN:
            relationship = AaveV3Asset.a_token
        elif token_type == TokenType.V_TOKEN:
            relationship = AaveV3Asset.v_token
        else:
            return None

        return self.session.scalar(
            select(AaveV3Asset)
            .join(relationship)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == checksum_addr,
            )
        )

    def _get_asset_by_a_token(self, a_token_address: ChecksumAddress) -> AaveV3Asset | None:
        """
        Get the asset for a given aToken address.
        """

        return self._get_asset_by_token(a_token_address, TokenType.A_TOKEN)

    def _get_asset_by_v_token(self, v_token_address: ChecksumAddress) -> AaveV3Asset | None:
        """
        Get the asset for a given vToken address.
        """

        return self._get_asset_by_token(v_token_address, TokenType.V_TOKEN)

    def parse(self, events: list[LogReceipt], tx_hash: HexBytes) -> TransactionOperations:
        """
        Parse events into operations.
        """

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

        # Get pool revision for amount scaling
        pool_revision = self._get_pool_revision()

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
                pool_revision=pool_revision,
            )
            if operation:
                operations.append(operation)
                # Track assigned events
                assigned_log_indices.update(operation.get_event_log_indices())

        # Step 4b: Create MINT_TO_TREASURY operations for unassigned scaled token mints
        # where the user is the Pool contract (protocol reserves being minted to treasury)
        # For Pool Revision 8, extract MintedToTreasury events to get the actual scaled amount
        minted_to_treasury_events = self._extract_minted_to_treasury_events(events)
        mint_to_treasury_ops = self._create_mint_to_treasury_operations(
            scaled_events=scaled_events,
            assigned_indices=assigned_log_indices,
            starting_operation_id=len(operations),
            pool_revision=pool_revision,
            minted_to_treasury_events=minted_to_treasury_events,
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
            pool_revision=pool_revision,
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
            pool_revision=pool_revision,
        )
        operations.extend(transfer_ops)

        # Step 4d: Handle unassigned events
        unassigned_events = [
            e
            for e in events
            if e["logIndex"] not in assigned_log_indices
            and e["topics"][0] != ERC20Event.TRANSFER.value
        ]

        # Step 5: Validate all operations
        for op in operations:
            self._validate_operation(op, tx_hash)

        return TransactionOperations(
            tx_hash=tx_hash,
            block_number=block_number,
            operations=operations,
            unassigned_events=unassigned_events,
        )

    @staticmethod
    def _extract_pool_events(events: list[LogReceipt]) -> list[LogReceipt]:
        """
        Extract pool-level events (SUPPLY, WITHDRAW, etc.).
        """

        pool_topics = {
            AaveV3PoolEvent.SUPPLY.value,
            AaveV3PoolEvent.WITHDRAW.value,
            AaveV3PoolEvent.BORROW.value,
            AaveV3PoolEvent.REPAY.value,
            AaveV3PoolEvent.LIQUIDATION_CALL.value,
            AaveV3PoolEvent.DEFICIT_CREATED.value,
        }

        return sorted(
            [e for e in events if e["topics"][0] in pool_topics],
            key=operator.itemgetter("logIndex"),
        )

    def _extract_scaled_token_events(self, events: list[LogReceipt]) -> list[ScaledTokenEvent]:
        """
        Extract and decode scaled token events.
        """

        result = []
        for event in events:
            topic = event["topics"][0]

            if topic == AaveV3ScaledTokenEvent.MINT.value:
                ev = self._decode_mint_event(event)
                if ev:
                    result.append(ev)

            elif topic == AaveV3ScaledTokenEvent.BURN.value:
                ev = self._decode_burn_event(event)
                if ev:
                    result.append(ev)

            elif topic == AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value:
                ev = self._decode_balance_transfer_event(event)
                if ev:
                    result.append(ev)

            elif topic == ERC20Event.TRANSFER.value:
                # Handle ERC20 Transfer events for aTokens, vTokens, and the GHO discount token if
                # that mechanism is active.
                ev = self._decode_transfer_event(event)
                if ev:
                    result.append(ev)

        return sorted(result, key=lambda e: e.event["logIndex"])

    def _decode_mint_event(self, event: LogReceipt) -> ScaledTokenEvent | None:
        """
        Decode a Mint event.

        Event definition:
            event Mint(
                address indexed caller,
                address indexed onBehalfOf,
                uint256 value,
                uint256 balanceIncrease,
                uint256 index
            );
        """

        caller = decode_address(event["topics"][1])
        user = decode_address(event["topics"][2])
        amount, balance_increase, index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=event["data"],
        )

        # Determine event type based on token type
        token_address = get_checksum_address(event["address"])
        if token_address == self.gho_vtoken_address:
            event_type = ScaledTokenEventType.GHO_DEBT_MINT
        else:
            # Use token type lookup to determine if this is a collateral or debt mint
            token_type = self._get_token_type(token_address)
            if token_type == TokenType.A_TOKEN:
                event_type = ScaledTokenEventType.COLLATERAL_MINT
            elif token_type == TokenType.V_TOKEN:
                event_type = ScaledTokenEventType.DEBT_MINT
            else:
                msg = "Unknown token type!"
                raise ValueError(msg)

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

    def _decode_burn_event(self, event: LogReceipt) -> ScaledTokenEvent | None:
        """Decode a Burn event."""

        from_addr = decode_address(event["topics"][1])
        target = decode_address(event["topics"][2])
        amount, balance_increase, index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=event["data"],
        )

        # Determine event type based on token type
        token_address = get_checksum_address(event["address"])
        if token_address == self.gho_vtoken_address:
            event_type = ScaledTokenEventType.GHO_DEBT_BURN
        else:
            # Use token type lookup to determine if this is a collateral or debt burn
            token_type = self._get_token_type(token_address)
            if token_type == TokenType.A_TOKEN:
                event_type = ScaledTokenEventType.COLLATERAL_BURN
            elif token_type == TokenType.V_TOKEN:
                event_type = ScaledTokenEventType.DEBT_BURN
            else:
                msg = "Unknown burn event!"
                raise ValueError(msg)

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

    def _decode_balance_transfer_event(self, event: LogReceipt) -> ScaledTokenEvent | None:
        """Decode a BalanceTransfer event.

        BalanceTransfer events represent internal scaled balance movements in aTokens.
        During liquidations, collateral may be transferred to the treasury instead of burned.
        """

        from_addr = decode_address(event["topics"][1])
        to_addr = decode_address(event["topics"][2])
        # BalanceTransfer data: amount, index
        amount, index = eth_abi.abi.decode(
            types=["uint256", "uint256"],
            data=event["data"],
        )

        # Determine event type based on token type
        token_address = get_checksum_address(event["address"])

        # Use token type lookup to determine if this is collateral or debt
        token_type = self._get_token_type(token_address)

        if token_type == TokenType.A_TOKEN:
            event_type = ScaledTokenEventType.COLLATERAL_TRANSFER
        elif token_type == TokenType.V_TOKEN:
            event_type = ScaledTokenEventType.DEBT_TRANSFER
        else:
            msg = "Unknown token type!"
            raise ValueError(msg)

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

    def _decode_transfer_event(self, event: LogReceipt) -> ScaledTokenEvent | None:
        """
        Decode an ERC20 Transfer event for three specific token types:
            - aToken
            - vToken
            - GHO vToken discount token
        """

        from_addr = decode_address(event["topics"][1])
        to_addr = decode_address(event["topics"][2])
        # Transfer data: amount
        (amount,) = eth_abi.abi.decode(
            types=["uint256"],
            data=event["data"],
        )

        # Determine event type based on token type
        token_address = get_checksum_address(event["address"])
        if token_address == self.gho_vtoken_address:
            event_type = ScaledTokenEventType.GHO_DEBT_TRANSFER
        else:
            # Use token type lookup to determine if this is collateral or debt
            token_type = self._get_token_type(token_address)
            if token_type == TokenType.A_TOKEN:
                # Standard ERC20 Transfer (not BalanceTransfer) - no index
                event_type = ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER
            elif token_type == TokenType.V_TOKEN:
                # Standard ERC20 Transfer (not BalanceTransfer) - no index
                event_type = ScaledTokenEventType.ERC20_DEBT_TRANSFER
            elif token_type == TokenType.GHO_DISCOUNT:
                # This is active only when the GHO vToken discount mechanism is active
                event_type = ScaledTokenEventType.DISCOUNT_TRANSFER
            else:
                return None

        return ScaledTokenEvent(
            event=event,
            event_type=event_type,
            user_address=from_addr,  # The user whose balance decreased
            caller_address=None,
            from_address=from_addr,
            target_address=to_addr,
            amount=amount,
            balance_increase=None,  # Transfer doesn't have balanceIncrease
            index=None,  # ERC20 Transfer doesn't have index
        )

    def _create_operation_from_pool_event(
        self,
        *,
        operation_id: int,
        pool_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation | None:
        """Create operation starting from a pool event."""
        topic = pool_event["topics"][0]

        if topic == AaveV3PoolEvent.SUPPLY.value:
            return self._create_supply_operation(
                operation_id=operation_id,
                supply_event=pool_event,
                scaled_events=scaled_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )
        if topic == AaveV3PoolEvent.WITHDRAW.value:
            return self._create_withdraw_operation(
                operation_id=operation_id,
                withdraw_event=pool_event,
                scaled_events=scaled_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )
        if topic == AaveV3PoolEvent.BORROW.value:
            return self._create_borrow_operation(
                operation_id=operation_id,
                borrow_event=pool_event,
                scaled_events=scaled_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )
        if topic == AaveV3PoolEvent.REPAY.value:
            return self._create_repay_operation(
                operation_id=operation_id,
                repay_event=pool_event,
                scaled_events=scaled_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )
        if topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
            return self._create_liquidation_operation(
                operation_id=operation_id,
                liquidation_event=pool_event,
                scaled_events=scaled_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )
        if topic == AaveV3PoolEvent.DEFICIT_CREATED.value:
            return self._create_deficit_operation(
                operation_id=operation_id,
                deficit_event=pool_event,
                scaled_events=scaled_events,
                all_events=all_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )

        msg = f"Could not determine operation from event topic {topic!r}"
        raise ValueError(msg)

    @staticmethod
    def _create_supply_operation(
        *,
        operation_id: int,
        supply_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """
        Create SUPPLY operation.

        Event definition:
            event Supply(
                address indexed reserve,
                address user,
                address indexed onBehalfOf,
                uint256 amount,
                uint16 indexed referralCode
            );
        """

        assert supply_event["topics"][0] == AaveV3PoolEvent.SUPPLY.value

        on_behalf_of = decode_address(supply_event["topics"][2])
        _user, supply_amount = eth_abi.abi.decode(
            types=["address", "uint256"], data=supply_event["data"]
        )

        # Find collateral mint for this user
        # For SUPPLY: look for mints where value > balance_increase (standard deposit)
        # Match on onBehalfOf (beneficiary) from the SUPPLY event, which corresponds
        # to the user_address in the collateral mint event
        collateral_mint: ScaledTokenEvent | None = None
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.event_type != ScaledTokenEventType.COLLATERAL_MINT:
                continue
            if ev.user_address != on_behalf_of:
                continue
            if ev.balance_increase is None:
                continue
            if ev.index is None:
                continue

            # Calculate expected mint principal
            # Pool revision 9 began pre-scaling the amount with flooring ray division.
            # Calculating it exactly requires injecting extra details about the position,
            # so this check will allow up to a TOKEN_AMOUNT_MATCH_TOLERANCE wei deviation
            # on pool revisions 9+
            #
            # see TX: 0x46dfb37518cad8e8749d858c7f166385e74aaeaa1775d4ab99804761b709d63a
            # for an example of a supply event amount=285000000000000000, but the associated
            # Mint has a principal amount=284999999999999998
            calculated_principal = ev.amount - ev.balance_increase
            if pool_revision >= SCALED_AMOUNT_POOL_REVISION:
                if abs(calculated_principal - supply_amount) > TOKEN_AMOUNT_MATCH_TOLERANCE:
                    continue
            elif calculated_principal != supply_amount:
                continue

            collateral_mint = ev
            break

        assert collateral_mint is not None, supply_event["transactionHash"].to_0x_hex()

        # Also look for matching Transfer events from zero address (ERC20 mint)
        # These represent the same supply operation
        # Match on onBehalfOf (beneficiary) as the target of the transfer
        transfer_events = []
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.from_address != ZERO_ADDRESS:
                continue
            if ev.target_address != on_behalf_of:
                continue
            if ev.event_type not in {
                ScaledTokenEventType.COLLATERAL_TRANSFER,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
            }:
                continue
            if ev.amount is None or ev.amount != collateral_mint.amount:
                continue
            if ev.event["address"] != collateral_mint.event["address"]:
                continue

            transfer_events.append(ev.event)
            break

        assert len(transfer_events) == 1

        return Operation(
            operation_id=operation_id,
            operation_type=OperationType.SUPPLY,
            pool_revision=pool_revision,
            pool_event=supply_event,
            scaled_token_events=[collateral_mint],
            transfer_events=transfer_events,
            balance_transfer_events=[],
        )

    @staticmethod
    def _create_withdraw_operation(
        *,
        operation_id: int,
        withdraw_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """
        Create WITHDRAW operation.

        Event definition:
            event Withdraw(
                address indexed reserve,
                address indexed user,
                address indexed to,
                uint256 amount
            );
        """

        assert withdraw_event["topics"][0] == AaveV3PoolEvent.WITHDRAW.value

        user = decode_address(withdraw_event["topics"][2])
        (withdraw_amount,) = eth_abi.abi.decode(types=["uint256"], data=withdraw_event["data"])

        # Find collateral burn for this operation (most common case)
        collateral_burn: ScaledTokenEvent | None = None
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.event_type != ScaledTokenEventType.COLLATERAL_BURN:
                continue
            if ev.user_address != user:
                continue
            if ev.index is None:
                continue
            if ev.balance_increase is None:
                continue

            # Calculate expected burn amount
            # Pool revision 9 began pre-scaling the amount with flooring ray division.
            # Calculating it exactly requires injecting extra details about the position,
            # so this check will allow up to a TOKEN_AMOUNT_MATCH_TOLERANCE wei deviation
            # on pool revisions 9+
            #
            # see TX: 0x8a4bc3d8f386c0d754d98766caf9033202a65a932f0f3ede035d95f039a56abe
            # for an example of a withdraw event amount=500000000000000000000000, but the
            # associated Burn has a principal amount=500000000000000000000001
            calculated_burn = ev.amount + ev.balance_increase
            if pool_revision >= SCALED_AMOUNT_POOL_REVISION:
                if abs(calculated_burn - withdraw_amount) > TOKEN_AMOUNT_MATCH_TOLERANCE:
                    continue
            elif calculated_burn != withdraw_amount:
                continue

            collateral_burn = ev
            break

        # If no burn found, search for "interest exceeds withdrawal" mint
        # In this case, interest accrued exceeds withdrawal amount, so instead of
        # burning, the contract emits a Mint representing net balance increase.
        interest_mint: ScaledTokenEvent | None = None
        if not collateral_burn:
            for ev in scaled_events:
                if ev.event["logIndex"] in assigned_indices:
                    continue
                if ev.event_type != ScaledTokenEventType.COLLATERAL_MINT:
                    continue
                if ev.user_address != user:
                    continue
                if ev.index is None:
                    continue
                if ev.balance_increase is None:
                    continue

                # Check for "interest exceeds withdrawal" pattern
                # Pattern 1: mint amount < balance_increase (partial interest used)
                # Pattern 2: mint amount ≈ balance_increase (full interest used)
                calculated_withdraw = ev.balance_increase - ev.amount

                if pool_revision >= SCALED_AMOUNT_POOL_REVISION:
                    pattern_1_match = (
                        abs(calculated_withdraw - withdraw_amount) <= TOKEN_AMOUNT_MATCH_TOLERANCE
                    )
                    pattern_2_match = (
                        abs(ev.amount - ev.balance_increase) <= TOKEN_AMOUNT_MATCH_TOLERANCE
                    )
                else:
                    pattern_1_match = calculated_withdraw == withdraw_amount
                    pattern_2_match = ev.amount == ev.balance_increase

                if not (pattern_1_match or pattern_2_match):
                    continue

                interest_mint = ev
                break

        # Every WITHDRAW must have either a collateral burn or interest mint
        if not collateral_burn and not interest_mint:
            msg = (
                f"WITHDRAW at logIndex={withdraw_event['logIndex']} for user={user} "
                f"with amount={withdraw_amount} has no matching burn or mint event. "
                "Every WITHDRAW must have a collateral burn (or mint when "
                "interest exceeds withdrawal)."
            )
            raise AssertionError(msg)

        # Find matching Transfer event
        # A Mint corresponds to a Transfer from ZERO_ADDRESS
        # A Burn corresponds to a Transfer to ZERO_ADDRESS
        transfer_event: LogReceipt | None = None

        if interest_mint:
            for ev in scaled_events:
                if ev.event["logIndex"] in assigned_indices:
                    continue
                if ev.event_type not in {
                    ScaledTokenEventType.COLLATERAL_TRANSFER,
                    ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                }:
                    continue
                if ev.from_address != ZERO_ADDRESS:
                    continue

                transfer_event = ev.event
                break
        else:
            assert collateral_burn is not None
            for ev in scaled_events:
                if ev.event["logIndex"] in assigned_indices:
                    continue
                if ev.event_type not in {
                    ScaledTokenEventType.COLLATERAL_TRANSFER,
                    ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                }:
                    continue
                if ev.target_address != ZERO_ADDRESS:
                    continue

                transfer_event = ev.event
                break

        assert transfer_event is not None, (
            f"WITHDRAW at logIndex={withdraw_event['logIndex']} missing transfer event"
        )

        # Build scaled token events list (exactly one event: either mint or burn)
        scaled_token_events: list[ScaledTokenEvent] = []
        if interest_mint:
            scaled_token_events = [interest_mint]
        else:
            assert collateral_burn is not None
            scaled_token_events = [collateral_burn]

        return Operation(
            operation_id=operation_id,
            operation_type=OperationType.WITHDRAW,
            pool_revision=pool_revision,
            pool_event=withdraw_event,
            scaled_token_events=scaled_token_events,
            transfer_events=[transfer_event],
            balance_transfer_events=[],
        )

    def _create_borrow_operation(
        self,
        operation_id: int,
        borrow_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """
        Create BORROW operation.

        Event definition:
            event Borrow(
                address indexed reserve,
                address user,
                address indexed onBehalfOf,
                uint256 amount,
                uint8 interestRateMode,
                uint256 borrowRate,
                uint16 indexed referralCode
            );
        """

        assert borrow_event["topics"][0] == AaveV3PoolEvent.BORROW.value

        reserve = decode_address(borrow_event["topics"][1])
        on_behalf_of = decode_address(borrow_event["topics"][2])
        _, borrow_amount, _, _ = eth_abi.abi.decode(
            types=["address", "uint256", "uint8", "uint256"],
            data=borrow_event["data"],
        )

        is_gho = reserve == self.gho_token_address

        # Match the borrow to the associated debt mint operation
        debt_mint = None
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.user_address != on_behalf_of:
                continue
            if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_MINT:
                continue
            if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_MINT:
                continue
            if ev.balance_increase is None:
                continue

            reserve_asset = self._get_reserve_for_debt_token(
                get_checksum_address(ev.event["address"])
            )

            if reserve_asset is None or reserve_asset != reserve:
                continue

            # Match borrow amount to debt mint principal
            calculated_borrow = ev.amount - ev.balance_increase
            if abs(calculated_borrow - borrow_amount) > TOKEN_AMOUNT_MATCH_TOLERANCE:
                continue

            debt_mint = ev
            break

        if debt_mint is None:
            msg = (
                f"Could not create BORROW operation for event {borrow_event}, looked for match of "
                f"value {borrow_amount}"
            )
            raise ValueError(msg)

        op_type = OperationType.GHO_BORROW if is_gho else OperationType.BORROW

        # Also look for matching Transfer events from zero address (ERC20 mint)
        # These represent the same borrow operation
        transfer_events = []
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.from_address != ZERO_ADDRESS:
                continue
            if ev.target_address != on_behalf_of:
                continue
            if ev.amount != debt_mint.amount:
                continue
            if ev.event_type not in {
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                ScaledTokenEventType.GHO_DEBT_TRANSFER,
            }:
                continue
            if ev.event["address"] != debt_mint.event["address"]:
                continue

            transfer_events.append(ev.event)
            break  # Only match one transfer per mint

        assert len(transfer_events) == 1

        return Operation(
            operation_id=operation_id,
            operation_type=op_type,
            pool_revision=pool_revision,
            pool_event=borrow_event,
            scaled_token_events=[debt_mint],
            transfer_events=transfer_events,
            balance_transfer_events=[],
        )

    def _create_repay_operation(
        self,
        operation_id: int,
        repay_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """
        Create REPAY operation.

        Event definition:
            event Repay(
                address indexed reserve,
                address indexed user,
                address indexed repayer,
                uint256 amount,
                bool useATokens
            );
        """

        reserve = decode_address(repay_event["topics"][1])
        user = decode_address(repay_event["topics"][2])
        repay_amount, use_a_tokens = eth_abi.abi.decode(
            types=["uint256", "bool"],
            data=repay_event["data"],
        )

        is_gho = reserve == self.gho_token_address
        repay_log_index = repay_event["logIndex"]

        if use_a_tokens:
            assert not is_gho
            return self._create_repay_with_atokens_operation(
                operation_id=operation_id,
                repay_event=repay_event,
                reserve=reserve,
                user=user,
                repay_amount=repay_amount,
                repay_log_index=repay_log_index,
                scaled_events=scaled_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )

        return self._create_standard_repay_operation(
            operation_id=operation_id,
            repay_event=repay_event,
            reserve=reserve,
            user=user,
            repay_amount=repay_amount,
            is_gho=is_gho,
            repay_log_index=repay_log_index,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            pool_revision=pool_revision,
        )

    def _create_standard_repay_operation(
        self,
        *,
        operation_id: int,
        repay_event: LogReceipt,
        reserve: ChecksumAddress,
        user: ChecksumAddress,
        repay_amount: int,
        is_gho: bool,
        repay_log_index: int,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """
        Create standard REPAY or GHO_REPAY operation.

        Attaches the principal debt event (Burn or Mint) to the operation.
        The event contains the repayment data including any accrued interest
        (available via the balance_increase field).
        """

        scaled_token_events: list[ScaledTokenEvent] = []
        local_assigned: set[int] = set()

        # Find principal debt event (Burn or Mint)
        principal_repay_event = self._find_principal_repay_event(
            user=user,
            reserve=reserve,
            repay_amount=repay_amount,
            is_gho=is_gho,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            pool_revision=pool_revision,
        )

        if principal_repay_event is not None:
            scaled_token_events.append(principal_repay_event)
            local_assigned.add(principal_repay_event.event["logIndex"])

        # If no events found, create minimal operation
        if not scaled_token_events:
            logger.debug(
                f"REPAY at logIndex={repay_log_index} has no matching burn events, "
                f"creating minimal operation"
            )
            return Operation(
                operation_id=operation_id,
                operation_type=OperationType.GHO_REPAY if is_gho else OperationType.REPAY,
                pool_revision=pool_revision,
                pool_event=repay_event,
                scaled_token_events=[],
                transfer_events=[],
                balance_transfer_events=[],
            )

        # Find transfer events for the principal burn only
        # Interest burns don't have corresponding transfer events (they're internal)
        transfer_events: list[LogReceipt] = []
        if principal_repay_event is not None:
            transfer_events = self._find_debt_transfer_to_zero(
                user=user,
                amount=principal_repay_event.amount,
                scaled_events=scaled_events,
                assigned_indices=assigned_indices,
            )

        return Operation(
            operation_id=operation_id,
            operation_type=OperationType.GHO_REPAY if is_gho else OperationType.REPAY,
            pool_revision=pool_revision,
            pool_event=repay_event,
            scaled_token_events=scaled_token_events,
            transfer_events=transfer_events,
            balance_transfer_events=[],
        )

    def _create_repay_with_atokens_operation(
        self,
        *,
        operation_id: int,
        repay_event: LogReceipt,
        reserve: ChecksumAddress,
        user: ChecksumAddress,
        repay_amount: int,
        repay_log_index: int,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """Create REPAY_WITH_ATOKENS operation (debt burn + collateral burn + balance transfer)."""

        scaled_token_events: list[ScaledTokenEvent] = []
        balance_transfer_events: list[LogReceipt] = []

        principal_repay_event = self._find_principal_repay_event(
            user=user,
            reserve=reserve,
            repay_amount=repay_amount,
            is_gho=False,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            pool_revision=pool_revision,
        )

        collateral_burn_event = self._find_matching_collateral_burn(
            user=user,
            expected_amount=repay_amount,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            pool_revision=pool_revision,
        )

        if principal_repay_event:
            scaled_token_events.append(principal_repay_event)

            if collateral_burn_event:
                scaled_token_events.append(collateral_burn_event)
                balance_transfer = self._find_balance_transfer_for_repay(
                    user=user,
                    reserve=reserve,
                    scaled_events=scaled_events,
                    assigned_indices=assigned_indices,
                )
                if balance_transfer:
                    balance_transfer_events.append(balance_transfer)
        elif collateral_burn_event:
            scaled_token_events.append(collateral_burn_event)
        else:
            logger.debug(
                f"REPAY_WITH_ATOKENS at logIndex={repay_log_index} has no matching burn event, "
                f"creating minimal operation"
            )

        return Operation(
            operation_id=operation_id,
            operation_type=OperationType.REPAY_WITH_ATOKENS,
            pool_revision=pool_revision,
            pool_event=repay_event,
            scaled_token_events=scaled_token_events,
            transfer_events=[],
            balance_transfer_events=balance_transfer_events,
        )

    def _find_principal_repay_event(
        self,
        *,
        user: ChecksumAddress,
        reserve: ChecksumAddress,
        repay_amount: int,
        is_gho: bool,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> ScaledTokenEvent | None:
        """Find the principal debt event (Burn or Mint) associated with a REPAY operation.

        For REPAY operations, the VariableDebtToken emits either:
        - Burn event: when repayment > interest (net decrease in unscaled debt)
        - Mint event: when interest > repayment (net increase in unscaled debt)

        Both represent the same operation (debt reduction via repayment), just with
        different net effects due to interest accrual.
        """

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue

            # For REPAY operations, match either Burn or Mint events
            # A Mint is emitted when interest > repayment (net unscaled increase)
            # A Burn is emitted when repayment > interest (net unscaled decrease)
            valid_event_types = (
                {
                    ScaledTokenEventType.GHO_DEBT_BURN,
                    ScaledTokenEventType.GHO_DEBT_MINT,
                }
                if is_gho
                else {
                    ScaledTokenEventType.DEBT_BURN,
                    ScaledTokenEventType.DEBT_MINT,
                }
            )
            if ev.event_type not in valid_event_types:
                continue

            if ev.user_address != user:
                continue

            reserve_asset = self._get_reserve_for_debt_token(
                get_checksum_address(ev.event["address"])
            )

            if reserve_asset != reserve:
                continue

            if ev.balance_increase is not None:
                # For DEBT_BURN: amount represents principal burned
                #   (calculated_amount = amount + balance_increase)
                # For DEBT_MINT: amount represents net increase (interest - repayment)
                #   (calculated_amount = balance_increase - amount)
                if ev.event_type in {
                    ScaledTokenEventType.DEBT_BURN,
                    ScaledTokenEventType.GHO_DEBT_BURN,
                }:
                    calculated_amount = ev.amount + ev.balance_increase
                else:  # DEBT_MINT or GHO_DEBT_MINT
                    calculated_amount = ev.balance_increase - ev.amount

                # Pool revision 9 began pre-scaling the amount with flooring ray division.
                # Calculating it exactly requires injecting extra details about the position,
                # so this check will allow up to a TOKEN_AMOUNT_MATCH_TOLERANCE wei deviation
                # on pool revisions 9+
                if pool_revision >= SCALED_AMOUNT_POOL_REVISION:
                    if abs(calculated_amount - repay_amount) > TOKEN_AMOUNT_MATCH_TOLERANCE:
                        continue
                elif calculated_amount != repay_amount:
                    continue

            # For Burn events: target_address should be ZERO_ADDRESS
            # For Mint events: target_address is None (no target in mints)
            if (
                ev.event_type
                in {
                    ScaledTokenEventType.DEBT_BURN,
                    ScaledTokenEventType.GHO_DEBT_BURN,
                }
                and ev.target_address != ZERO_ADDRESS
            ):
                continue

            return ev

        return None

    @staticmethod
    def _find_matching_collateral_burn(
        *,
        user: ChecksumAddress,
        expected_amount: int,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> ScaledTokenEvent | None:
        """
        Find the closest matching collateral burn event.

        Matches based on user address and burn amount (amount + balance_increase).
        For pool revision 9+, allows ±2 wei tolerance due to ray math rounding.
        """

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.event_type != ScaledTokenEventType.COLLATERAL_BURN:
                continue
            if ev.user_address != user:
                continue
            if ev.index is None:
                continue
            if ev.balance_increase is None:
                continue

            # Calculate the total burn amount (principal + interest)
            total_burn = ev.amount + ev.balance_increase

            # Pool revision 9+ uses ray math with flooring, allow ±2 wei tolerance
            # see TX: 0x8a4bc3d8f386c0d754d98766caf9033202a65a932f0f3ede035d95f039a56abe
            if pool_revision >= SCALED_AMOUNT_POOL_REVISION:
                if abs(total_burn - expected_amount) > TOKEN_AMOUNT_MATCH_TOLERANCE:
                    continue
            elif total_burn != expected_amount:
                continue

            return ev

        return None

    @staticmethod
    def _find_debt_transfer_to_zero(
        *,
        user: ChecksumAddress,
        amount: int,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
    ) -> list[LogReceipt]:
        """Find debt transfer event to zero address matching the given amount."""

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.event_type not in {
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                ScaledTokenEventType.GHO_DEBT_TRANSFER,
            }:
                continue
            if ev.from_address != user:
                continue
            if ev.target_address != ZERO_ADDRESS:
                continue
            if ev.amount != amount:
                continue

            return [ev.event]

        return []

    def _find_balance_transfer_for_repay(
        self,
        *,
        user: ChecksumAddress,
        reserve: ChecksumAddress,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
    ) -> LogReceipt | None:
        """Find BalanceTransfer event for aTokens used in repayment."""

        asset = self._get_a_token_asset_by_reserve(get_checksum_address(reserve))
        if asset is None:
            return None
        atoken_address = asset.a_token.address

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.event_type != ScaledTokenEventType.COLLATERAL_TRANSFER:
                continue
            if ev.index is None:
                continue
            if ev.from_address != user:
                continue
            if ev.event["address"] != atoken_address:
                continue

            return ev.event

        return None

    @staticmethod
    def _collect_primary_debt_burns(
        *,
        user: ChecksumAddress,
        debt_v_token_address: ChecksumAddress | None,
        debt_to_cover: int,  # noqa: ARG004
        pool_revision: int,  # noqa: ARG004
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        is_gho: bool,
    ) -> list[ScaledTokenEvent]:
        """
        Collect primary debt burns matching the liquidation's debt asset.

        Uses semantic matching: a debt burn for the same user and debt asset
        in this transaction belongs to this liquidation, regardless of amounts
        or log index ordering. Amount validation happens during processing.
        """

        primary_burns: list[ScaledTokenEvent] = []

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.user_address != user:
                continue
            if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_BURN:
                continue
            if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_BURN:
                continue

            event_token_address = get_checksum_address(ev.event["address"])
            if debt_v_token_address is None or event_token_address != debt_v_token_address:
                continue

            # Semantic matching: the presence of a debt burn for this user and
            # asset in this transaction indicates it belongs to this liquidation.
            # We trust the smart contract event ordering/logic over amount comparisons.
            primary_burns.append(ev)
            assigned_indices.add(ev.event["logIndex"])
            if ev.index is not None and ev.index > 0:
                assigned_indices.add(ev.index)
            break  # Only one primary burn expected per (user, asset) pair

        return primary_burns

    def _collect_secondary_debt_burns(
        self,
        *,
        user: ChecksumAddress,
        debt_v_token_address: ChecksumAddress | None,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        is_gho: bool,  # noqa: ARG002
    ) -> list[ScaledTokenEvent]:
        """
        Collect secondary debt burns for other assets held by the user.

        These are debts that weren't the primary liquidation target but were also
        burned as part of the liquidation (bad debt write-off scenario).

        Note: Secondary debt burns can be any debt type (GHO or non-GHO), not just
        the same type as the primary debt being liquidated.
        """

        secondary_burns: list[ScaledTokenEvent] = []

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.user_address != user:
                continue
            # Collect ALL debt burn types (both GHO and non-GHO) as secondary burns
            if ev.event_type not in {
                ScaledTokenEventType.DEBT_BURN,
                ScaledTokenEventType.GHO_DEBT_BURN,
            }:
                continue

            event_token_address = get_checksum_address(ev.event["address"])
            if debt_v_token_address is not None and event_token_address == debt_v_token_address:
                continue  # Skip primary debt burns

            # Validate this is a real debt token
            asset = self._get_asset_by_v_token(event_token_address)
            if asset is not None:
                secondary_burns.append(ev)

        return secondary_burns

    @staticmethod
    def _collect_collateral_events(
        *,
        user: ChecksumAddress,
        collateral_a_token_address: ChecksumAddress | None,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
    ) -> tuple[ScaledTokenEvent | None, list[ScaledTokenEvent]]:
        """
        Collect collateral events (burns and transfers) for the liquidation.

        During liquidations, borrower may have BOTH collateral burned AND multiple transfers.
        Collateral may be burned OR transferred to treasury (BalanceTransfer).

        Returns:
            Tuple of (collateral_burn, collateral_transfers)
        """

        collateral_transfers: list[ScaledTokenEvent] = []
        collateral_burn: ScaledTokenEvent | None = None

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue

            # Match collateral events only if they belong to this liquidation's collateral asset
            # This prevents incorrect matching when a user is liquidated multiple times
            # with different collateral assets in the same transaction
            event_token_address = get_checksum_address(ev.event["address"])
            if (
                collateral_a_token_address is not None
                and event_token_address != collateral_a_token_address
            ):
                continue

            if ev.event_type == ScaledTokenEventType.COLLATERAL_BURN and ev.user_address == user:
                collateral_burn = ev
            elif (
                ev.event_type
                in {
                    ScaledTokenEventType.COLLATERAL_TRANSFER,
                    ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                }
                and ev.user_address == user
            ):
                collateral_transfers.append(ev)

        return collateral_burn, collateral_transfers

    def _create_liquidation_operation(
        self,
        *,
        operation_id: int,
        liquidation_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """
        Create LIQUIDATION operation.

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

        collateral_asset = decode_address(liquidation_event["topics"][1])
        debt_asset = decode_address(liquidation_event["topics"][2])
        user = decode_address(liquidation_event["topics"][3])
        debt_to_cover, _, _, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "address", "bool"],
            data=liquidation_event["data"],
        )

        is_gho = debt_asset == self.gho_token_address

        # Get token contract addresses for the collateral and debt assets
        # This is needed to properly match events when a user is liquidated
        # multiple times with different collateral assets in the same transaction
        collateral_a_token_address = self._get_a_token_for_asset(collateral_asset)
        debt_v_token_address = self._get_v_token_for_asset(debt_asset)

        # Collect ALL debt burns for the liquidated user
        # A liquidation may burn multiple debt positions (not just the primary debt asset)
        primary_burns = self._collect_primary_debt_burns(
            user=user,
            debt_v_token_address=debt_v_token_address,
            debt_to_cover=debt_to_cover,
            pool_revision=pool_revision,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            is_gho=is_gho,
        )
        secondary_burns = self._collect_secondary_debt_burns(
            user=user,
            debt_v_token_address=debt_v_token_address,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            is_gho=is_gho,
        )
        debt_burns = primary_burns + secondary_burns

        # Collect collateral events (burns and transfers)
        collateral_burn, collateral_transfers = self._collect_collateral_events(
            user=user,
            collateral_a_token_address=collateral_a_token_address,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
        )

        # A liquidation requires at least one collateral event (burn or transfer)
        # Collateral may be transferred to treasury instead of burned when protocol takes fee
        assert collateral_burn is not None or collateral_transfers, (
            f"Expected at least 1 collateral event (burn or transfer) for liquidation. "
            f"User: {user}, scaled_events: {[e.event['logIndex'] for e in scaled_events]}"
        )

        # Find debt mint events that represent net debt increase during liquidation
        # This happens when accrued interest > debt repayment (balance_increase > amount)
        debt_mint: ScaledTokenEvent | None = None
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.user_address != user:
                continue
            if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_MINT:
                continue
            if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_MINT:
                continue

            # Match debt mint events only if they belong to this liquidation's debt asset
            event_token_address = get_checksum_address(ev.event["address"])
            if (
                debt_v_token_address is not None
                and event_token_address == debt_v_token_address
                and ev.balance_increase is not None
                and ev.balance_increase > ev.amount
            ):
                # This Mint event represents net debt increase during liquidation
                debt_mint = ev
                break

        scaled_token_events: list[ScaledTokenEvent] = []
        balance_transfer_events: list[LogReceipt] = []

        # Add the debt burns, debt mint, and collateral burn to scaled_token_events
        # Note: debt_burns may be empty for flash loan liquidations or when interest > repayment
        # Note: debt_mint is set when interest > repayment (net debt increase)
        # Note: collateral_burn may be None when collateral is transferred to treasury
        if collateral_burn is not None:
            scaled_token_events.append(collateral_burn)

        # Add all debt burns (primary and secondary)
        scaled_token_events.extend(debt_burns)
        if debt_mint is not None:
            scaled_token_events.append(debt_mint)

        if collateral_transfers:
            # Add all collateral transfers to scaled_token_events
            # Both ERC20 Transfers (index=0) and BalanceTransfer events (index>0)
            # are collateral events that should be validated together
            for transfer in collateral_transfers:
                scaled_token_events.append(transfer)
                # Track BalanceTransfer events separately so ERC20 Transfers can use
                # them for proper scaling during processing
                if transfer.index is not None and transfer.index > 0:
                    balance_transfer_events.append(transfer.event)

        op_type = OperationType.GHO_LIQUIDATION if is_gho else OperationType.LIQUIDATION

        return Operation(
            operation_id=operation_id,
            operation_type=op_type,
            pool_revision=pool_revision,
            pool_event=liquidation_event,
            scaled_token_events=scaled_token_events,
            transfer_events=[],
            balance_transfer_events=balance_transfer_events,
        )

    def _create_deficit_operation(
        self,
        *,
        operation_id: int,
        deficit_event: LogReceipt,
        scaled_events: list[ScaledTokenEvent],
        all_events: list[LogReceipt],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """
        Create DEFICIT_CREATED operation.

        Event definition:
            event DeficitCreated(
                address indexed user,
                address indexed debtAsset,
                uint256 amountCreated
            );

        DEFICIT_CREATED indicates bad debt write-off. When the asset is GHO,
        it's a GHO flash loan that requires a debt burn. For other assets,
        it's a standalone deficit event with no associated debt burn.

        Note: DEFICIT_CREATED can also be emitted during GHO liquidations as
        part of the bad debt write-off mechanism. In such cases, the GHO debt
        burn should be matched to the LIQUIDATION_CALL operation, not a
        separate flash loan operation.
        """

        user = decode_address(deficit_event["topics"][1])
        asset = decode_address(deficit_event["topics"][2])

        # Check if this is a GHO deficit (flash loan) or non-GHO deficit
        is_gho_deficit = asset == self.gho_token_address

        # Check if there's a LIQUIDATION_CALL for the same user in this transaction
        # If so, this DEFICIT_CREATED is part of the liquidation, not a standalone flash loan
        has_liquidation_for_user = False
        for event in all_events:
            if event["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value:
                liquidation_user = decode_address(event["topics"][3])
                if liquidation_user == user:
                    has_liquidation_for_user = True
                    break

        scaled_token_events: list[ScaledTokenEvent] = []
        if is_gho_deficit and not has_liquidation_for_user:
            # Find GHO debt burn for GHO flash loans only if not part of liquidation
            for ev in scaled_events:
                if ev.event["logIndex"] in assigned_indices:
                    continue

                if ev.event_type == ScaledTokenEventType.GHO_DEBT_BURN and ev.user_address == user:
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
            pool_revision=pool_revision,
            pool_event=deficit_event,
            scaled_token_events=scaled_token_events,
            transfer_events=[],
            balance_transfer_events=[],
        )

    @staticmethod
    def _create_interest_accrual_operations(
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        starting_operation_id: int,
        all_events: list[LogReceipt],
        pool_revision: int,
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
            if ev.event_type in {
                ScaledTokenEventType.DEBT_BURN,
                ScaledTokenEventType.GHO_DEBT_BURN,
            }:
                operations.append(
                    Operation(
                        operation_id=operation_id,
                        operation_type=OperationType.INTEREST_ACCRUAL,
                        pool_revision=pool_revision,
                        pool_event=None,
                        scaled_token_events=[ev],
                        transfer_events=[],
                        balance_transfer_events=[],
                    )
                )
                operation_id += 1
                continue

            # Handle unassigned collateral burn events
            # These can occur in umbrella/staking operations where aTokens are
            # burned without a corresponding WITHDRAW pool event (e.g., stkwaEthUSDC creation)
            if ev.event_type == ScaledTokenEventType.COLLATERAL_BURN:
                operations.append(
                    Operation(
                        operation_id=operation_id,
                        operation_type=OperationType.INTEREST_ACCRUAL,
                        pool_revision=pool_revision,
                        pool_event=None,
                        scaled_token_events=[ev],
                        transfer_events=[],
                        balance_transfer_events=[],
                    )
                )
                operation_id += 1
                continue

            # Only process mint events that represent interest accrual
            if ev.event_type not in {
                ScaledTokenEventType.COLLATERAL_MINT,
                ScaledTokenEventType.DEBT_MINT,
                ScaledTokenEventType.GHO_DEBT_MINT,
            }:
                continue

            # Handle DEBT_MINT events based on type
            # These may be implicit borrows in transactions without BORROW events
            # (e.g., flash loans, internal Pool operations)
            if ev.event_type in {
                ScaledTokenEventType.DEBT_MINT,
                ScaledTokenEventType.GHO_DEBT_MINT,
            }:
                assert ev.balance_increase is not None

                # Interest accrual: balance_increase >= amount
                # - balance_increase > amount: net interest after repayment (in _burnScaled)
                # - balance_increase == amount: pure interest accrual (in _accrueDebtOnAction)
                is_interest_accrual = ev.balance_increase >= ev.amount
                # Pure borrow: balance_increase == 0 (no interest accrued)
                is_pure_borrow = ev.balance_increase == 0

                if not is_interest_accrual:
                    # This is either a pure borrow or borrow with interest
                    # Skip during liquidation/flash loans as those are handled separately
                    if has_liquidation or has_borrow:
                        continue
                    # For pure borrows (balance_increase == 0), create IMPLICIT_BORROW
                    if is_pure_borrow:
                        operations.append(
                            Operation(
                                operation_id=operation_id,
                                operation_type=OperationType.IMPLICIT_BORROW,
                                pool_revision=pool_revision,
                                pool_event=None,
                                scaled_token_events=[ev],
                                transfer_events=[],
                                balance_transfer_events=[],
                            )
                        )
                        operation_id += 1
                        continue
                    # Borrow with interest (0 < balance_increase < amount) falls through
                    # to be processed as INTEREST_ACCRUAL
                # Interest accrual falls through to be processed below

            # Interest accrual: process all unassigned mint events
            # Note: For pure interest, amount == balance_increase
            # But sometimes amount < balance_increase (small deposit + interest)
            # Include dust mints (balance_increase == 0) which still need to update last_index
            # Look for matching Transfer event from ZERO_ADDRESS (interest accrual)
            # For mints from ZERO_ADDRESS, the target_address is the recipient (user)
            # Match by amount if it's a pure interest accrual, or by target_address if there's a
            # deposit
            transfer_events = []
            for transfer_ev in scaled_events:
                if all([
                    transfer_ev.event_type
                    in {
                        ScaledTokenEventType.COLLATERAL_TRANSFER,
                        ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                    },
                    transfer_ev.from_address == ZERO_ADDRESS,
                    transfer_ev.target_address == ev.user_address,
                    transfer_ev.event["address"] == ev.event["address"],
                    transfer_ev.event["logIndex"] not in assigned_indices,
                    transfer_ev.event["logIndex"] not in local_assigned,
                    # For pure interest, Transfer amount matches Mint amount
                    # For deposit + interest, Transfer amount may be less than Mint amount
                    transfer_ev.amount == ev.amount or transfer_ev.amount < ev.amount,
                ]):
                    transfer_events.append(transfer_ev.event)
                    local_assigned.add(transfer_ev.event["logIndex"])
                    break  # Only match one transfer per mint

            operations.append(
                Operation(
                    operation_id=operation_id,
                    operation_type=OperationType.INTEREST_ACCRUAL,
                    pool_revision=pool_revision,
                    pool_event=None,
                    scaled_token_events=[ev],
                    transfer_events=transfer_events,
                    balance_transfer_events=[],
                )
            )
            operation_id += 1

        return operations

    @staticmethod
    def _extract_minted_to_treasury_events(events: list[LogReceipt]) -> list[LogReceipt]:
        """
        Extract MintedToTreasury events from Pool contract.

        These events contain the actual amount minted to treasury (underlying for Rev 8,
        which equals the scaled amount passed to the AToken).
        """
        return [e for e in events if e["topics"][0] == AaveV3PoolEvent.MINTED_TO_TREASURY.value]

    def _create_mint_to_treasury_operations(
        self,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        starting_operation_id: int,
        pool_revision: int,
        minted_to_treasury_events: list[LogReceipt],
    ) -> list[Operation]:
        """Create MINT_TO_TREASURY operations for unassigned scaled token mints to the Pool.

        When the Pool contract calls mintToTreasury(), it emits ScaledTokenMint events
        where the caller_address is the Pool itself. These represent protocol reserves being
        minted to the treasury and should be treated as SUPPLY operations for the Pool.

        During liquidations, a BalanceTransfer event accompanies the Mint event, containing
        the actual scaled amount to add to the treasury.

        Args:
            scaled_events: All scaled token events from the transaction
            assigned_indices: Set of log indices already assigned to operations
            starting_operation_id: The next available operation ID

        Returns:
            List of MINT_TO_TREASURY operations
        """
        operations: list[Operation] = []
        operation_id = starting_operation_id

        for ev in scaled_events:
            # Skip already assigned events
            if ev.event["logIndex"] in assigned_indices:
                continue

            # Only process collateral mints where the caller is the Pool contract
            if ev.event_type != ScaledTokenEventType.COLLATERAL_MINT:
                continue

            # Check if caller is the Pool (mintToTreasury calls have Pool as caller)
            if ev.caller_address != self.pool_address:
                continue

            # This is a mint to treasury - create operation
            logger.debug(
                f"Creating MINT_TO_TREASURY for event at logIndex {ev.event['logIndex']}, "
                f"user={ev.user_address}, amount={ev.amount}"
            )

            # Extract MintedToTreasury amount
            # For Rev 1-8: amountMinted is in underlying units (needs rayDiv to get scaled)
            # For Rev 9+: amountMinted equals the scaled amount directly
            minted_amount: int | None = None
            if minted_to_treasury_events:
                """
                Event definition:
                    event MintedToTreasury(
                        address indexed reserve,
                        uint256 amountMinted
                    );
                """

                # The Mint event's address is the aToken, so we need to find the underlying asset
                asset = self._get_asset_by_a_token(
                    a_token_address=get_checksum_address(ev.event["address"])
                )
                if asset is not None:
                    underlying_addr = get_checksum_address(asset.underlying_token.address)

                    for mt_ev in minted_to_treasury_events:
                        mt_reserve: str
                        (mt_reserve,) = eth_abi.abi.decode(
                            types=["address"], data=mt_ev["topics"][1]
                        )
                        minted_reserve = get_checksum_address(mt_reserve)

                        if minted_reserve == underlying_addr:
                            (minted_amount,) = eth_abi.abi.decode(
                                types=["uint256"],
                                data=mt_ev["data"],
                            )
                            break

            operations.append(
                Operation(
                    operation_id=operation_id,
                    operation_type=OperationType.MINT_TO_TREASURY,
                    pool_revision=pool_revision,
                    pool_event=None,
                    scaled_token_events=[ev],
                    transfer_events=[],
                    balance_transfer_events=[],
                    minted_to_treasury_amount=minted_amount,
                )
            )
            operation_id += 1

        return operations

    @staticmethod
    def _create_transfer_operations(
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        starting_operation_id: int,
        existing_operations: list[Operation],
        pool_revision: int,
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

        for ev in scaled_events:  # noqa:PLR1702
            # Skip already assigned events (both externally and locally)
            if ev.event["logIndex"] in assigned_indices or ev.event["logIndex"] in local_assigned:
                continue

            # Only process transfer events
            if ev.event_type not in {
                ScaledTokenEventType.COLLATERAL_TRANSFER,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                ScaledTokenEventType.GHO_DEBT_TRANSFER,
                ScaledTokenEventType.DISCOUNT_TRANSFER,
            }:
                continue

            # Check if this is an ERC20 Transfer event (index=None means no index from event)
            # BalanceTransfer events have index > 0
            is_erc20_transfer = ev.index is None

            # Skip ERC20 Transfer events to zero address that are part of burns
            # These are handled by the Burn events, not as balance transfers
            if is_erc20_transfer and ev.target_address == ZERO_ADDRESS:
                # Use semantic matching: look for any burn event for the same user and token
                # Log index proximity is not reliable in batch transactions
                is_part_of_burn = False
                ev_token_address = get_checksum_address(ev.event["address"])
                for other_ev in scaled_events:
                    if (
                        other_ev.event_type
                        in {
                            ScaledTokenEventType.DEBT_BURN,
                            ScaledTokenEventType.COLLATERAL_BURN,
                            ScaledTokenEventType.GHO_DEBT_BURN,
                        }
                        and other_ev.user_address == ev.from_address
                        and get_checksum_address(other_ev.event["address"]) == ev_token_address
                    ):
                        # This transfer is part of a burn, skip it
                        is_part_of_burn = True
                        local_assigned.add(ev.event["logIndex"])
                        break
                if is_part_of_burn:
                    continue

            # Skip ERC20 Transfer events from zero address that are part of mints
            # These are handled by the Mint events (SUPPLY, MINT_TO_TREASURY), not as transfers
            if is_erc20_transfer and ev.from_address == ZERO_ADDRESS:
                # Use semantic matching: look for any mint event for the same user and token
                # Log index proximity is not reliable in batch transactions
                is_part_of_mint = False
                ev_token_address = get_checksum_address(ev.event["address"])
                for other_ev in scaled_events:
                    if (
                        other_ev.event_type
                        in {
                            ScaledTokenEventType.COLLATERAL_MINT,
                            ScaledTokenEventType.DEBT_MINT,
                            ScaledTokenEventType.GHO_DEBT_MINT,
                        }
                        and other_ev.user_address == ev.target_address
                        and get_checksum_address(other_ev.event["address"]) == ev_token_address
                    ):
                        # This transfer is part of a mint, skip it
                        is_part_of_mint = True
                        local_assigned.add(ev.event["logIndex"])
                        break
                if is_part_of_mint:
                    continue

            balance_transfer_event: ScaledTokenEvent | None = None

            if is_erc20_transfer:
                # Look for a corresponding BalanceTransfer event
                # BalanceTransfer events are decoded from SCALED_TOKEN_BALANCE_TRANSFER topic
                for bt_ev in scaled_events:
                    if (
                        bt_ev.event["logIndex"] in assigned_indices
                        or bt_ev.event["logIndex"] in local_assigned
                    ):
                        continue

                    # ERC20 Transfer events have index=None, so skip those
                    if bt_ev.index is None:
                        continue

                    # Check if from/to addresses match and it's the same token
                    # Allow matching between ERC20 Transfer and BalanceTransfer events
                    # (e.g., ERC20_COLLATERAL_TRANSFER with COLLATERAL_TRANSFER)
                    event_types_match = (
                        bt_ev.event_type == ev.event_type
                        or {bt_ev.event_type, ev.event_type}
                        == {
                            ScaledTokenEventType.COLLATERAL_TRANSFER,
                            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                        }
                        or {bt_ev.event_type, ev.event_type}
                        == {
                            ScaledTokenEventType.DEBT_TRANSFER,
                            ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                        }
                    )

                    if (
                        bt_ev.from_address == ev.from_address
                        and bt_ev.target_address == ev.target_address
                        and bt_ev.event["address"] == ev.event["address"]
                        and event_types_match
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
                            # ERC20 Transfer events have index=None, so skip those
                            if bt_ev.index is None:
                                continue

                            # Check if from/to addresses match and it's the same token
                            # Allow matching between ERC20 Transfer and BalanceTransfer events
                            event_types_match = (
                                bt_ev.event_type == ev.event_type
                                or {bt_ev.event_type, ev.event_type}
                                == {
                                    ScaledTokenEventType.COLLATERAL_TRANSFER,
                                    ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                                }
                                or {bt_ev.event_type, ev.event_type}
                                == {
                                    ScaledTokenEventType.DEBT_TRANSFER,
                                    ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                                }
                            )

                            if (
                                bt_ev.from_address == ev.from_address
                                and bt_ev.target_address == ev.target_address
                                and bt_ev.event["address"] == ev.event["address"]
                                and event_types_match
                            ):
                                # Found matching BalanceTransfer in existing operation
                                balance_transfer_event = bt_ev
                                local_assigned.add(bt_ev.event["logIndex"])
                                break
                        if balance_transfer_event:
                            break

            # Create TRANSFER operation with both events if found
            balance_transfer_events = []
            if balance_transfer_event:
                balance_transfer_events.append(balance_transfer_event.event)

            # Determine operation type based on event type
            if ev.event_type == ScaledTokenEventType.DISCOUNT_TRANSFER:
                operation_type = OperationType.STKAAVE_TRANSFER
            else:
                operation_type = OperationType.BALANCE_TRANSFER

            operations.append(
                Operation(
                    operation_id=operation_id,
                    operation_type=operation_type,
                    pool_revision=pool_revision,
                    pool_event=None,
                    scaled_token_events=[ev],
                    transfer_events=[],
                    balance_transfer_events=balance_transfer_events,
                )
            )
            operation_id += 1

        # Process standalone BalanceTransfer events (no paired ERC20 Transfer)
        # These can occur when rewards are distributed directly via BalanceTransfer
        for ev in scaled_events:
            # Skip already assigned events
            if ev.event["logIndex"] in assigned_indices or ev.event["logIndex"] in local_assigned:
                continue

            # Only process BalanceTransfer events (index > 0 indicates BalanceTransfer)
            if ev.index is None or ev.index == 0:
                continue

            # Only process transfer event types
            if ev.event_type not in {
                ScaledTokenEventType.COLLATERAL_TRANSFER,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                ScaledTokenEventType.GHO_DEBT_TRANSFER,
                ScaledTokenEventType.DISCOUNT_TRANSFER,
            }:
                continue

            # Determine operation type based on event type
            if ev.event_type == ScaledTokenEventType.DISCOUNT_TRANSFER:
                operation_type = OperationType.STKAAVE_TRANSFER
            else:
                operation_type = OperationType.BALANCE_TRANSFER

            # Create operation for standalone BalanceTransfer
            operations.append(
                Operation(
                    operation_id=operation_id,
                    operation_type=operation_type,
                    pool_revision=pool_revision,
                    pool_event=None,
                    scaled_token_events=[ev],
                    transfer_events=[],
                    balance_transfer_events=[],
                )
            )
            local_assigned.add(ev.event["logIndex"])
            operation_id += 1

        # Update the assigned_indices set with locally assigned events
        assigned_indices.update(local_assigned)

        return operations

    def _validate_operation(self, op: Operation, tx_hash: HexBytes) -> None:
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
            OperationType.STKAAVE_TRANSFER: self._validate_stkaave_transfer,
        }

        validator = validators.get(op.operation_type)
        if validator:
            errors.extend(validator(op))
        elif op.operation_type == OperationType.UNKNOWN:
            # UNKNOWN operations are intentionally unprocessed placeholders
            # (e.g., DEFICIT_CREATED events that are part of liquidations)
            pass
        else:
            msg = f"No validator found for {op.operation_type}!"
            raise ValueError(msg)

        if errors:
            raise TransactionValidationError(
                message=(
                    f"Operation {op.operation_id} ({op.operation_type.name}) validation failed:\n"
                    + "\n".join(errors)
                ),
                tx_hash=tx_hash,
                events=op.get_all_events(),
                operations=[op],
            )

    @staticmethod
    def _validate_supply(op: Operation) -> list[str]:
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

    @staticmethod
    def _validate_withdraw(op: Operation) -> list[str]:
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

    @staticmethod
    def _validate_borrow(op: Operation) -> list[str]:
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
        gho_mints = [
            e for e in op.scaled_token_events if e.event_type == ScaledTokenEventType.GHO_DEBT_MINT
        ]
        if len(gho_mints) != 1:
            errors.append(f"Expected 1 GHO debt mint for GHO_BORROW, got {len(gho_mints)}")

        return errors

    @staticmethod
    def _validate_repay(op: Operation) -> list[str]:
        """Validate REPAY operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing REPAY pool event")
            return errors

        # Can have 0, 1, or 2 debt burns:
        # 0 = interest-only repayment
        # 1 = principal repayment only
        # 2 = principal repayment + interest accrual during transaction
        debt_burns = [e for e in op.scaled_token_events if e.is_debt]
        if len(debt_burns) > 2:  # noqa:PLR2004
            errors.append(f"Expected 0, 1, or 2 debt burns for REPAY, got {len(debt_burns)}")

        return errors

    @staticmethod
    def _validate_repay_with_atokens(op: Operation) -> list[str]:
        """Validate REPAY_WITH_ATOKENS operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing REPAY pool event")
            return errors

        # Should have 0 or 1 debt events (burn or mint) and 0 or 1 collateral burn
        # Note: When interest exceeds repayment, debt mints instead of burns
        # Note: In some edge cases, debt burn may not be emitted if debt is fully covered by
        # interest
        # Edge case: Collateral burn may be absent if user has no aToken balance or when
        # repayment is handled via flash loan / adapter contract
        # See TX 0x1a7d205b9831cc63c545ba5ddf21c2fc29c00973ac680fc6371e3aa999f60f19
        debt_events = [e for e in op.scaled_token_events if e.is_debt]
        collateral_burns = [e for e in op.scaled_token_events if e.is_collateral and e.is_burn]

        if len(debt_events) > 1:
            errors.append(
                f"Expected 0 or 1 debt events for REPAY_WITH_ATOKENS, got {len(debt_events)}"
            )
        if len(collateral_burns) > 1:
            errors.append(
                f"Expected at most 1 collateral burn for REPAY_WITH_ATOKENS, "
                f"got {len(collateral_burns)}"
            )

        return errors

    def _validate_gho_repay(self, op: Operation) -> list[str]:
        """Validate GHO REPAY operation."""
        errors = self._validate_repay(op)

        # GHO repay can emit either BURN (debt reduction) or MINT (interest > repayment)
        # When interest accrued exceeds repayment amount, the debt token mints instead of burns
        gho_events = [
            e
            for e in op.scaled_token_events
            if e.event_type
            in {ScaledTokenEventType.GHO_DEBT_BURN, ScaledTokenEventType.GHO_DEBT_MINT}
        ]
        if len(gho_events) > 1:
            errors.append(f"Expected 0 or 1 GHO debt event for GHO_REPAY, got {len(gho_events)}")

        return errors

    @staticmethod
    def _validate_liquidation(op: Operation) -> list[str]:
        """Validate LIQUIDATION operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing LIQUIDATION_CALL pool event")
            return errors

        # Should have 1 collateral event (burn or transfer) and 0 or more debt burns
        # Flash loan liquidations have 0 debt burns (debt repaid via flash loan)
        # Standard liquidations have 1 debt burn (primary debt asset)
        # Multi-asset liquidations may have multiple debt burns (primary + secondary debts)
        # Collateral may be burned OR transferred to treasury (BalanceTransfer)
        debt_burns = [e for e in op.scaled_token_events if e.is_debt]
        collateral_events = [e for e in op.scaled_token_events if e.is_collateral]

        # Allow multiple debt burns for multi-asset liquidations
        # Each debt burn should be for a different debt asset (verified by token address)
        if len(debt_burns) > 0:
            # Check that all debt burns are for different assets
            debt_token_addresses = {e.event["address"] for e in debt_burns}
            if len(debt_token_addresses) != len(debt_burns):
                errors.append(
                    f"Multiple debt burns for same asset in LIQUIDATION. "
                    f"Debt burns: {[e.event['logIndex'] for e in debt_burns]}. "
                    f"Token addresses: {list(debt_token_addresses)}"
                )

        if len(collateral_events) < 1:
            errors.append(
                f"Expected at least 1 collateral event (burn or transfer) for LIQUIDATION, "
                f"got {len(collateral_events)}. "
                f"DEBUG NOTE: Check collateral asset matching and user address consistency. "
                f"Current collateral events: {[e.event['logIndex'] for e in collateral_events]}. "
                f"User in LIQUIDATION_CALL: {decode_address(op.pool_event['topics'][3])}"
            )

        return errors

    def _validate_gho_liquidation(self, op: Operation) -> list[str]:
        """Validate GHO LIQUIDATION operation."""
        errors = self._validate_liquidation(op)

        # Additional GHO-specific validation
        gho_burns = [
            e for e in op.scaled_token_events if e.event_type == ScaledTokenEventType.GHO_DEBT_BURN
        ]
        if len(gho_burns) > 1:
            errors.append(
                f"Expected 0 or 1 GHO debt burn for GHO_LIQUIDATION, got {len(gho_burns)}. "
                f"DEBUG NOTE: Dust liquidations may have 0 burns (zero debt to cover)."
            )

        return errors

    @staticmethod
    def _validate_flash_loan(op: Operation) -> list[str]:
        """Validate FLASH_LOAN (DEFICIT_CREATED) operation."""
        errors = []

        if not op.pool_event:
            errors.append("Missing DEFICIT_CREATED pool event")
            return errors

        # Should have exactly 1 GHO debt burn
        gho_burns = [
            e for e in op.scaled_token_events if e.event_type == ScaledTokenEventType.GHO_DEBT_BURN
        ]
        if len(gho_burns) != 1:
            errors.append(
                f"Expected 1 GHO debt burn for FLASH_LOAN, got {len(gho_burns)}. "
                f"DEBUG NOTE: Flash loans should have exactly one debt burn."
            )

        return errors

    @staticmethod
    def _validate_interest_accrual(op: Operation) -> list[str]:
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
                f"Expected 1 scaled token event for INTEREST_ACCRUAL, "
                f"got {len(op.scaled_token_events)}"
            )

        # Allow both interest accrual (balance_increase > 0) and dust mints (balance_increase == 0)
        # Dust mints occur during discount updates and still need to update last_index

        return errors

    @staticmethod
    def _validate_balance_transfer(op: Operation) -> list[str]:
        """Validate BALANCE_TRANSFER operation."""
        errors = []

        # Should have no pool event (transfers are standalone)
        if op.pool_event is not None:
            errors.append("BALANCE_TRANSFER should not have a pool event")

        # Should have exactly 1 scaled token event (the transfer)
        if len(op.scaled_token_events) != 1:
            errors.append(
                f"Expected 1 scaled token event for BALANCE_TRANSFER, "
                f"got {len(op.scaled_token_events)}"
            )

        # The event should be a transfer event
        if op.scaled_token_events:
            ev = op.scaled_token_events[0]
            if ev.event_type not in {
                ScaledTokenEventType.COLLATERAL_TRANSFER,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                ScaledTokenEventType.GHO_DEBT_TRANSFER,
            }:
                errors.append(
                    f"BALANCE_TRANSFER event should be a transfer type, got {ev.event_type}"
                )

        return errors

    @staticmethod
    def _validate_mint_to_treasury(op: Operation) -> list[str]:
        """Validate MINT_TO_TREASURY operation."""
        errors = []

        # Should have no pool event (treasury mints are standalone)
        if op.pool_event is not None:
            errors.append("MINT_TO_TREASURY should not have a pool event")

        # Should have exactly 1 scaled token event (the mint)
        if len(op.scaled_token_events) != 1:
            errors.append(
                f"Expected 1 scaled token event for MINT_TO_TREASURY, "
                f"got {len(op.scaled_token_events)}"
            )

        # The event should be a collateral mint
        if op.scaled_token_events:
            ev = op.scaled_token_events[0]
            if ev.event_type != ScaledTokenEventType.COLLATERAL_MINT:
                errors.append(
                    f"MINT_TO_TREASURY event should be COLLATERAL_MINT, got {ev.event_type}"
                )

        return errors

    @staticmethod
    def _validate_stkaave_transfer(op: Operation) -> list[str]:
        """Validate STKAAVE_TRANSFER operation."""
        errors = []

        # Should have no pool event (transfers are standalone ERC20 events)
        if op.pool_event is not None:
            errors.append("STKAAVE_TRANSFER should not have a pool event")

        # Should have exactly 1 scaled token event (the transfer)
        if len(op.scaled_token_events) != 1:
            errors.append(
                f"Expected 1 scaled token event for STKAAVE_TRANSFER, "
                f"got {len(op.scaled_token_events)}"
            )

        # The event should be a discount transfer
        if op.scaled_token_events:
            ev = op.scaled_token_events[0]
            if ev.event_type != ScaledTokenEventType.DISCOUNT_TRANSFER:
                errors.append(
                    f"STKAAVE_TRANSFER event should be DISCOUNT_TRANSFER, got {ev.event_type}"
                )

        return errors
