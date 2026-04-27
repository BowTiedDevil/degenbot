"""
Parses transaction events into logical operations based on asset flows.
Provides strict validation with detailed plain-text error reporting.
"""

import operator
from collections import Counter
from dataclasses import dataclass, field
from typing import Literal, assert_never

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
from degenbot.aave.operation_types import OperationType
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
        """Check if operation passed validation."""
        return len(self.validation_errors) == 0

    def get_all_events(self) -> list[LogReceipt]:
        """Get all events involved in this operation."""
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
        """Get all log indices involved in this operation."""
        return [e["logIndex"] for e in self.get_all_events()]


class TransactionValidationError(Exception):  # pragma: no cover
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
            lines.append("  Status: Valid")

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
        for op in self.operations:  # pragma: no cover
            if not op.is_valid():
                all_errors.extend([
                    f"Operation {op.operation_id} ({op.operation_type.name}): {err}"
                    for err in op.validation_errors
                ])

        # Check for unassigned required events
        required_unassigned = [e for e in self.unassigned_events if self._is_required_pool_event(e)]
        assert not required_unassigned

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
        assert not unassigned_scaled

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

        if all_errors:  # pragma: no cover
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

        # TODO: add to deployments or store in DB
        known_treasuries: dict[int, ChecksumAddress] = {
            1: get_checksum_address("0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c"),  # Ethereum
        }

        assert self.market.chain_id in known_treasuries
        return known_treasuries[self.market.chain_id]

    def _get_gho_asset(self) -> AaveGhoToken:
        """Get GHO token asset for the current market."""

        gho_asset = self.session.scalar(
            select(AaveGhoToken)
            .join(AaveGhoToken.token)
            .where(Erc20TokenTable.chain == self.market.chain_id)
        )
        assert gho_asset is not None

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
        """
        Get the underlying reserve address for a debt token.
        """

        asset = self.session.scalar(
            select(AaveV3Asset)
            .join(AaveV3Asset.v_token)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == debt_token_address,
            )
        )
        assert asset is not None

        return get_checksum_address(asset.underlying_token.address)

    def _get_a_token_for_asset(self, underlying_asset: ChecksumAddress) -> ChecksumAddress | None:
        """
        Get the aToken address for an underlying asset.
        """

        asset = self.session.scalar(
            select(AaveV3Asset)
            .join(AaveV3Asset.underlying_token)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == underlying_asset,
            )
        )
        assert asset is not None

        return get_checksum_address(asset.a_token.address)

    def _get_v_token_for_asset(self, underlying_asset: ChecksumAddress) -> ChecksumAddress | None:
        """
        Get the vToken address for an underlying asset.
        """

        # Query database directly to avoid stale ORM cache
        asset = self.session.scalar(
            select(AaveV3Asset)
            .join(AaveV3Asset.underlying_token)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == underlying_asset,
            )
        )
        assert asset is not None

        return get_checksum_address(asset.v_token.address)

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

    def _get_asset_by_token(
        self,
        token_address: ChecksumAddress,
        token_type: TokenType,
    ) -> AaveV3Asset | None:
        """
        Get the asset for a given token address.
        """

        # Select the appropriate relationship based on token type
        if token_type == TokenType.A_TOKEN:
            relationship = AaveV3Asset.a_token
        elif token_type == TokenType.V_TOKEN:
            relationship = AaveV3Asset.v_token
        else:
            assert_never(token_type)

        return self.session.scalar(
            select(AaveV3Asset)
            .join(relationship)
            .where(
                AaveV3Asset.market_id == self.market.id,
                Erc20TokenTable.address == token_address,
            )
        )

    def _get_asset_by_a_token(self, a_token_address: ChecksumAddress) -> AaveV3Asset | None:
        """
        Get the asset for a given aToken address.
        """

        return self._get_asset_by_token(a_token_address, TokenType.A_TOKEN)

    def _get_event_type_for_token(
        self,
        token_address: ChecksumAddress,
        event_category: Literal["mint", "burn", "transfer"],
    ) -> ScaledTokenEventType:
        """
        Determine the event type based on token type and event category.

        Uses GHO token check first (special case), then falls back to token type lookup.
        """

        if event_category == "mint":
            if token_address == self.gho_vtoken_address:
                return ScaledTokenEventType.GHO_DEBT_MINT
            token_type = self._get_token_type(token_address)
            if token_type == TokenType.A_TOKEN:
                return ScaledTokenEventType.COLLATERAL_MINT
            if token_type == TokenType.V_TOKEN:
                return ScaledTokenEventType.DEBT_MINT
            assert_never(token_type)

        if event_category == "burn":
            if token_address == self.gho_vtoken_address:
                return ScaledTokenEventType.GHO_DEBT_BURN
            token_type = self._get_token_type(token_address)
            if token_type == TokenType.A_TOKEN:
                return ScaledTokenEventType.COLLATERAL_BURN
            if token_type == TokenType.V_TOKEN:
                return ScaledTokenEventType.DEBT_BURN
            assert_never(token_type)

        # Fall through to transfer handling
        token_type = self._get_token_type(token_address)
        if token_type == TokenType.A_TOKEN:
            return ScaledTokenEventType.COLLATERAL_TRANSFER
        if token_type == TokenType.V_TOKEN:
            return ScaledTokenEventType.DEBT_TRANSFER
        assert_never(token_type)

    @staticmethod
    def _amounts_match(
        calculated: int,
        expected: int,
        pool_revision: int,
        tolerance: int = TOKEN_AMOUNT_MATCH_TOLERANCE,
    ) -> bool:
        """
        Check if calculated amount matches expected, accounting for pool revision tolerance.

        Pool revision 9+ uses ray math with flooring which can cause ±2 wei deviations.
        """
        if pool_revision >= SCALED_AMOUNT_POOL_REVISION:
            return abs(calculated - expected) <= tolerance
        return calculated == expected

    @staticmethod
    def _are_compatible_transfer_types(
        ev1: ScaledTokenEvent,
        ev2: ScaledTokenEvent,
    ) -> bool:
        """
        Check if two transfer events are compatible (ERC20 Transfer + BalanceTransfer).

        Allows matching between ERC20 Transfer events and BalanceTransfer events
        of the same token type (collateral or debt).
        """

        collateral_pair = {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }
        debt_pair = {
            ScaledTokenEventType.DEBT_TRANSFER,
            ScaledTokenEventType.ERC20_DEBT_TRANSFER,
        }

        event_types = {ev1.event_type, ev2.event_type}
        return event_types in (collateral_pair, debt_pair)

    def parse(self, events: list[LogReceipt], tx_hash: HexBytes) -> TransactionOperations:
        """
        Parse events into operations.
        """

        assert events

        block_number = events[0]["blockNumber"]

        # Step 1: Identify pool events (anchors for operations)
        pool_events = self._extract_pool_events(events)

        # Step 2: Identify and decode scaled token events
        scaled_events = self._extract_scaled_token_events(events)

        # Step 3: Group into operations
        operations: list[Operation] = []
        assigned_log_indices: set[int] = set()
        pool_revision = self._get_pool_revision()

        for i, pool_event in enumerate(pool_events):
            operation = self._create_operation_from_pool_event(
                operation_id=i,
                pool_event=pool_event,
                scaled_events=scaled_events,
                all_events=events,
                assigned_indices=assigned_log_indices,
                pool_revision=pool_revision,
            )
            assert operation is not None

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

        # Step 4c: Create DEFICIT_COVERAGE operations for paired BalanceTransfer + Burn events
        # These occur during Umbrella protocol's deficit coverage operations where aTokens
        # are transferred to a user and then immediately burned to cover deficits
        deficit_coverage_ops = self._create_deficit_coverage_operations(
            scaled_events=scaled_events,
            assigned_indices=assigned_log_indices,
            starting_operation_id=len(operations),
            pool_revision=pool_revision,
        )
        operations.extend(deficit_coverage_ops)
        assigned_log_indices.update(
            ev.event["logIndex"] for op in deficit_coverage_ops for ev in op.scaled_token_events
        )

        # Step 4d: Create INTEREST_ACCRUAL operations for unassigned scaled token events
        # that represent interest accrual (amount == balance_increase)
        # Skip DEBT_MINT extraction if there's a LIQUIDATION_CALL (flash loan pattern)
        interest_accrual_ops = self._create_interest_accrual_operations(
            scaled_events=scaled_events,
            assigned_indices=assigned_log_indices,
            starting_operation_id=len(operations),
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

        # Step 4e: Create TRANSFER operations for unassigned transfer events
        transfer_ops = self._create_transfer_operations(
            scaled_events=scaled_events,
            assigned_indices=assigned_log_indices,
            starting_operation_id=len(operations),
            pool_revision=pool_revision,
        )
        operations.extend(transfer_ops)

        # Step 4f: Handle unassigned events
        unassigned_events = [
            e
            for e in events
            if e["logIndex"] not in assigned_log_indices
            and e["topics"][0] != ERC20Event.TRANSFER.value
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
                result.append(
                    self._decode_mint_event(event),
                )

            elif topic == AaveV3ScaledTokenEvent.BURN.value:
                result.append(
                    self._decode_burn_event(event),
                )

            elif topic == AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value:
                result.append(
                    self._decode_balance_transfer_event(event),
                )

            elif topic == ERC20Event.TRANSFER.value:
                # Handle ERC20 Transfer events for aTokens, vTokens, and the GHO discount token if
                # that mechanism is active.
                ev = self._decode_transfer_event(event)
                if ev:
                    result.append(ev)

        return sorted(result, key=lambda e: e.event["logIndex"])

    def _decode_mint_event(self, event: LogReceipt) -> ScaledTokenEvent:
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

        token_address = event["address"]
        event_type = self._get_event_type_for_token(token_address, "mint")

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

    def _decode_burn_event(self, event: LogReceipt) -> ScaledTokenEvent:
        """Decode a Burn event."""

        from_addr = decode_address(event["topics"][1])
        target = decode_address(event["topics"][2])
        amount, balance_increase, index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=event["data"],
        )

        token_address = event["address"]
        event_type = self._get_event_type_for_token(token_address, "burn")

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

    def _decode_balance_transfer_event(self, event: LogReceipt) -> ScaledTokenEvent:
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

        token_address = event["address"]
        event_type = self._get_event_type_for_token(token_address, "transfer")

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
        token_address = event["address"]
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
                all_events=all_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )
        if topic == AaveV3PoolEvent.DEFICIT_CREATED.value:
            return self._create_deficit_operation(
                operation_id=operation_id,
                deficit_event=pool_event,
                all_events=all_events,
                pool_revision=pool_revision,
            )

        assert_never(topic)

    def _create_supply_operation(
        self,
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
        _, supply_amount = eth_abi.abi.decode(
            types=["address", "uint256"], data=supply_event["data"]
        )

        # Get the reserve (underlying asset) from the Supply event
        supply_reserve = decode_address(supply_event["topics"][1])

        # Get the aToken address for this reserve
        expected_a_token = self._get_a_token_for_asset(supply_reserve)
        assert expected_a_token is not None, (
            f"Could not find aToken for reserve {supply_reserve} in market {self.market.id}"
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
            # Verify event is from the correct aToken contract
            if ev.event["address"] != expected_a_token:
                continue
            if ev.user_address != on_behalf_of:
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
            if not self._amounts_match(calculated_principal, supply_amount, pool_revision):
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

    def _create_withdraw_operation(
        self,
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

        # Get the reserve (underlying asset) from the Withdraw event
        withdraw_reserve = decode_address(withdraw_event["topics"][1])

        # Get the aToken address for this reserve
        expected_a_token = self._get_a_token_for_asset(withdraw_reserve)
        assert expected_a_token is not None, (
            f"Could not find aToken for reserve {withdraw_reserve} in market {self.market.id}"
        )

        # Find collateral burn for this operation (most common case)
        collateral_burn: ScaledTokenEvent | None = None
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.event_type != ScaledTokenEventType.COLLATERAL_BURN:
                continue
            # Verify event is from the correct aToken contract
            if ev.event["address"] != expected_a_token:
                continue
            if ev.user_address != user:
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
            if not self._amounts_match(calculated_burn, withdraw_amount, pool_revision):
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
                # Verify event is from the correct aToken contract
                if ev.event["address"] != expected_a_token:
                    continue

                interest_mint = ev
                break

        assert collateral_burn or interest_mint

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
                # Verify event is from the correct aToken contract
                if ev.event["address"] != expected_a_token:
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
                # Verify event is from the correct aToken contract
                if ev.event["address"] != expected_a_token:
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

            reserve_asset = self._get_reserve_for_debt_token(ev.event["address"])
            assert reserve_asset is not None

            # Match borrow amount to debt mint principal
            calculated_borrow = ev.amount - ev.balance_increase
            if not self._amounts_match(calculated_borrow, borrow_amount, pool_revision):
                continue

            debt_mint = ev
            break

        assert debt_mint is not None

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

            assert ev.event_type in {
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                ScaledTokenEventType.GHO_DEBT_TRANSFER,
            }

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

        if use_a_tokens:
            assert not is_gho
            return self._create_repay_with_atokens_operation(
                operation_id=operation_id,
                repay_event=repay_event,
                reserve=reserve,
                user=user,
                repay_amount=repay_amount,
                scaled_events=scaled_events,
                assigned_indices=assigned_indices,
                pool_revision=pool_revision,
            )

        return self._create_standard_repay_operation(
            operation_id=operation_id,
            repay_event=repay_event,
            user=user,
            repay_amount=repay_amount,
            is_gho=is_gho,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            pool_revision=pool_revision,
        )

    def _create_standard_repay_operation(
        self,
        *,
        operation_id: int,
        repay_event: LogReceipt,
        user: ChecksumAddress,
        repay_amount: int,
        is_gho: bool,
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
            repay_amount=repay_amount,
            is_gho=is_gho,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            pool_revision=pool_revision,
        )
        assert principal_repay_event is not None

        scaled_token_events.append(principal_repay_event)
        local_assigned.add(principal_repay_event.event["logIndex"])

        # Find transfer events for the principal burn only
        # Interest burns don't have corresponding transfer events (they're internal)
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
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> Operation:
        """Create REPAY_WITH_ATOKENS operation (debt burn + collateral burn + balance transfer)."""

        scaled_token_events: list[ScaledTokenEvent] = []
        balance_transfer_events: list[LogReceipt] = []

        principal_repay_event = self._find_principal_repay_event(
            repay_amount=repay_amount,
            is_gho=False,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            pool_revision=pool_revision,
        )
        assert principal_repay_event is not None
        scaled_token_events.append(principal_repay_event)

        collateral_adjustment_event = self._find_collateral_adjustment_event(
            user=user,
            reserve=reserve,
            expected_amount=repay_amount,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            pool_revision=pool_revision,
        )
        assert collateral_adjustment_event is not None
        scaled_token_events.append(collateral_adjustment_event)

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
        repay_amount: int,
        is_gho: bool,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> ScaledTokenEvent:
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

            reserve_asset = self._get_reserve_for_debt_token(ev.event["address"])
            assert reserve_asset is not None

            assert ev.balance_increase is not None

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

            if not self._amounts_match(calculated_amount, repay_amount, pool_revision):
                continue

            return ev

        assert_never()

    def _find_collateral_adjustment_event(
        self,
        *,
        user: ChecksumAddress,
        reserve: ChecksumAddress,
        expected_amount: int,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        pool_revision: int,
    ) -> ScaledTokenEvent:
        """
        Find the collateral adjustment event for a REPAY_WITH_ATOKENS operation.

        In a REPAY_WITH_ATOKENS operation, the user burns aTokens to repay debt.
        The contract emits either:
        - COLLATERAL_BURN: when burned amount > accrued interest
        - COLLATERAL_MINT: when accrued interest > burned amount (user receives aTokens)

        Both represent the same conceptual operation: adjusting collateral to repay debt.
        The net adjustment amount is calculated from the event fields and matched against
        the expected repayment amount from the pool event.

        For pool revision 9+, allows ±2 wei tolerance due to ray math rounding.
        """

        # Get the aToken address for this reserve
        expected_a_token = self._get_a_token_for_asset(reserve)
        assert expected_a_token is not None, (
            f"Could not find aToken for reserve {reserve} in market {self.market.id}"
        )

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices:
                continue
            if ev.event_type not in {
                ScaledTokenEventType.COLLATERAL_BURN,
                ScaledTokenEventType.COLLATERAL_MINT,
            }:
                continue
            if ev.user_address != user:
                continue

            # Calculate the net collateral adjustment amount
            # BURN: user burns (amount + balance_increase) aTokens
            #   - amount = principal portion
            #   - balance_increase = interest accrued
            #   - total burned = amount + balance_increase
            # MINT: user receives net interest when interest > burn amount
            #   - balance_increase = gross interest accrued
            #   - amount = net interest (balance_increase - burn_amount)
            #   - burn_amount = balance_increase - amount
            if ev.event_type == ScaledTokenEventType.COLLATERAL_MINT:
                # For mints in REPAY_WITH_ATOKENS, the amount field is net interest
                # The actual burn amount = balance_increase - amount
                adjustment = ev.balance_increase - ev.amount
            else:
                # For burns, total adjustment = principal + interest
                adjustment = ev.amount + ev.balance_increase

            if not self._amounts_match(adjustment, expected_amount, pool_revision):
                continue

            return ev

        assert_never()

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
            if ev.amount != amount:
                continue

            return [ev.event]

        return []

    def _analyze_liquidation_scenarios(
        self,
        all_events: list[LogReceipt],
    ) -> dict[tuple[ChecksumAddress, ChecksumAddress], int]:
        """
        Pre-analyze liquidations to detect multi-liquidation scenarios.

        Returns a mapping of (user, debt_v_token_address) -> liquidation_count.
        This allows proper disambiguation when the same user is liquidated
        multiple times with the same debt asset in one transaction.

        See debug/aave/0051 for architectural details.
        """
        liquidation_counts: Counter[tuple[ChecksumAddress, ChecksumAddress]] = Counter()

        for ev in all_events:
            if ev["topics"][0] != AaveV3PoolEvent.LIQUIDATION_CALL.value:
                continue

            user = decode_address(ev["topics"][3])
            debt_asset = decode_address(ev["topics"][2])  # Underlying asset address

            # Convert underlying to vToken address for consistent lookups
            v_token_address = self._get_v_token_for_asset(debt_asset)
            assert v_token_address is not None
            liquidation_counts[user, v_token_address] += 1

        return dict(liquidation_counts)

    @staticmethod
    def _analyze_user_liquidation_count(
        all_events: list[LogReceipt],
    ) -> dict[ChecksumAddress, int]:
        """
        Count total liquidations per user (not per user+asset pair).

        When a user has exactly 1 liquidation, ALL debt burns for that user belong
        to that single liquidation. This handles bad debt liquidations where the
        protocol burns multiple debt positions via _burnBadDebt().

        When a user has multiple liquidations, use asset-specific matching to
        disambiguate which burns belong to which liquidation.

        See debug/aave/0055 for user-level liquidation count approach.
        """
        counts: dict[ChecksumAddress, int] = {}
        for ev in all_events:
            if ev["topics"][0] != AaveV3PoolEvent.LIQUIDATION_CALL.value:
                continue
            user = decode_address(ev["topics"][3])
            counts[user] = counts.get(user, 0) + 1
        return counts

    @staticmethod
    def _collect_debt_burns(
        *,
        user: ChecksumAddress,
        debt_v_token_address: ChecksumAddress | None,
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        liquidation_analysis: dict[tuple[ChecksumAddress, ChecksumAddress], int],
        user_liquidation_count: int = 1,
        liquidation_position: int = 0,
    ) -> list[ScaledTokenEvent]:
        """
        Collect debt burns for the liquidated user.

        Collection strategy:
        - Single liquidation per user: Collect ALL debt burns (no asset filter)
          This handles bad debt liquidations where _burnBadDebt() burns all debt positions.
        - Multiple liquidations per user: Use asset filter + sequential matching
          to disambiguate which burns belong to which liquidation.

        See debug/aave/0054 for sequential matching approach.
        See debug/aave/0051 for original refactoring.
        See debug/aave/0052 for removal of is_gho-based filtering.
        See debug/aave/0055 for user-level liquidation count approach.
        """
        burns: list[ScaledTokenEvent] = []

        if user_liquidation_count == 1:
            candidate_burns = sorted(
                [
                    ev
                    for ev in scaled_events
                    if ev.event["logIndex"] not in assigned_indices
                    and ev.user_address == user
                    and ev.event_type
                    in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}
                ],
                key=lambda e: e.event["logIndex"],
            )
            for ev in candidate_burns:
                burns.append(ev)
                assigned_indices.add(ev.event["logIndex"])
                assert ev.index is not None
                assert ev.index > 0
                assigned_indices.add(ev.index)
        else:
            is_multi_liquidation = False
            liquidation_count_for_asset = 1
            assert debt_v_token_address is not None
            liquidation_count_for_asset = liquidation_analysis.get(
                (user, debt_v_token_address),
                1,
            )
            is_multi_liquidation = liquidation_count_for_asset > 1

            # Get ALL burns for this (user, debt_asset) to determine pattern
            # Don't filter by assigned_indices yet - we need total count for pattern detection
            all_burns_for_asset = sorted(
                [
                    ev
                    for ev in scaled_events
                    if ev.user_address == user
                    and ev.event_type
                    in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}
                    and ev.event["address"] == debt_v_token_address
                ],
                key=lambda e: e.event["logIndex"],
            )

            # Now get only unassigned burns for assignment
            candidate_burns = [
                ev for ev in all_burns_for_asset if ev.event["logIndex"] not in assigned_indices
            ]

            # Determine pattern: COMBINED_BURN vs SEPARATE_BURNS
            # COMBINED_BURN: N liquidations share M burns where M < N (Issue 0056)
            # SEPARATE_BURNS: N liquidations have N burns, one per liquidation (Issue 0065)
            total_burn_count = len(all_burns_for_asset)

            if is_multi_liquidation and len(candidate_burns) > 0:
                assert liquidation_count_for_asset >= total_burn_count
                if liquidation_count_for_asset == total_burn_count:
                    # SEPARATE_BURNS pattern: Each liquidation gets exactly one burn
                    # Get burn at position liquidation_position from ALL burns (not just unassigned)
                    assert liquidation_position < total_burn_count
                    target_burn = all_burns_for_asset[liquidation_position]

                    assert target_burn.event["logIndex"] not in assigned_indices
                    burns.append(target_burn)
                    assigned_indices.add(target_burn.event["logIndex"])

                    assert target_burn.index is not None
                    assert target_burn.index > 0
                    assigned_indices.add(target_burn.index)

                elif liquidation_position == 0:
                    # COMBINED_BURN pattern: More liquidations than burns
                    # All burns go to the first liquidation
                    for ev in candidate_burns:
                        burns.append(ev)
                        assigned_indices.add(ev.event["logIndex"])

                        assert ev.index is not None
                        assert ev.index > 0
                        assigned_indices.add(ev.index)

            else:
                # Single liquidation or no burns: collect all available burns
                for ev in candidate_burns:
                    burns.append(ev)
                    assigned_indices.add(ev.event["logIndex"])
                    assert ev.index is not None
                    assert ev.index > 0
                    assigned_indices.add(ev.index)

        return burns

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
            event_token_address = ev.event["address"]
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
        all_events: list[LogReceipt],
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

        # Extract debtToCover from LiquidationCall event data
        # Event data: [debtToCover, liquidatedCollateralAmount, liquidator, receiveAToken]
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

        assert debt_v_token_address is not None

        # Pre-analyze liquidations to detect multi-liquidation scenarios
        # This allows proper disambiguation without the primary/secondary split
        # See debug/aave/0051 for architectural details
        liquidation_analysis = self._analyze_liquidation_scenarios(all_events)

        # Count total liquidations per user to determine collection strategy
        # Single liquidation = collect ALL debt burns (handles bad debt multi-asset)
        # Multiple liquidations = use asset filter + sequential matching
        # See debug/aave/0055 for user-level liquidation count approach
        user_liquidation_analysis = self._analyze_user_liquidation_count(all_events)
        user_liquidation_count = user_liquidation_analysis.get(user, 1)

        # Calculate this liquidation's position among all liquidations for this (user, debt_asset)
        # Sequential matching: burn[i] belongs to liquidation[i]
        # See debug/aave/0054 for detailed explanation
        liquidation_position = 0
        for ev in all_events:
            if ev["topics"][0] != AaveV3PoolEvent.LIQUIDATION_CALL.value:
                continue
            if decode_address(ev["topics"][3]) != user:
                continue
            if decode_address(ev["topics"][2]) != debt_asset:
                continue
            if ev["logIndex"] < liquidation_event["logIndex"]:
                liquidation_position += 1

        # Collect debt burns for the liquidated user
        # Single liquidation: collect ALL debt burns (handles bad debt multi-asset)
        # Multiple liquidations: use asset filter + sequential matching
        # See debug/aave/0054 for sequential matching approach
        # See debug/aave/0055 for user-level liquidation count approach
        debt_burns = self._collect_debt_burns(
            user=user,
            debt_v_token_address=debt_v_token_address,
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            liquidation_analysis=liquidation_analysis,
            user_liquidation_count=user_liquidation_count,
            liquidation_position=liquidation_position,
        )

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
        for scaled_event in scaled_events:
            if scaled_event.event["logIndex"] in assigned_indices:
                continue
            if scaled_event.user_address != user:
                continue
            if is_gho and scaled_event.event_type != ScaledTokenEventType.GHO_DEBT_MINT:
                continue
            if not is_gho and scaled_event.event_type != ScaledTokenEventType.DEBT_MINT:
                continue

            # Match debt mint events only if they belong to this liquidation's debt asset
            event_token_address = scaled_event.event["address"]
            assert event_token_address == debt_v_token_address
            assert scaled_event.balance_increase is not None
            assert scaled_event.balance_increase > scaled_event.amount

            # This Mint event represents net debt increase during liquidation
            debt_mint = scaled_event
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
            debt_to_cover=debt_to_cover,  # Use actual debtToCover from LiquidationCall
        )

    def _create_deficit_operation(
        self,
        *,
        operation_id: int,
        deficit_event: LogReceipt,
        all_events: list[LogReceipt],
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

        # If this DEFICIT_CREATED is part of a liquidation, mark it as UNKNOWN
        # so it doesn't interfere with liquidation processing
        # TODO: fix this block, it seems like tech debt
        if is_gho_deficit and has_liquidation_for_user:
            operation_type = OperationType.UNKNOWN
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
    def _create_deficit_coverage_operations(
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        starting_operation_id: int,
        pool_revision: int,
    ) -> list[Operation]:
        """Create DEFICIT_COVERAGE operations for paired BalanceTransfer + Burn events.

        Umbrella protocol's executeCoverReserveDeficits transfers aTokens to a user
        (via BalanceTransfer) and then burns them to cover reserve deficits. These
        paired events must be processed atomically to maintain correct balances.

        Pattern:
            1. BalanceTransfer to user (credits user's collateral position)
            2. Burn from user (debits user's collateral position + interest)

        The net effect is zero (transfer amount == burn amount - interest), but the
        intermediate state must be tracked correctly.

        Args:
            scaled_events: All scaled token events from the transaction
            assigned_indices: Set of log indices already assigned to operations
            starting_operation_id: The next available operation ID
            pool_revision: Pool revision for this transaction

        Returns:
            List of DEFICIT_COVERAGE operations
        """
        operations: list[Operation] = []
        operation_id = starting_operation_id
        local_assigned: set[int] = set()

        # Find all BalanceTransfer events that are not yet assigned
        balance_transfers: list[ScaledTokenEvent] = []
        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices or ev.event["logIndex"] in local_assigned:
                continue
            if ev.event_type in {
                ScaledTokenEventType.COLLATERAL_TRANSFER,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
            }:
                balance_transfers.append(ev)

        # For each BalanceTransfer, look for a paired Burn event
        for bt_ev in balance_transfers:
            bt_token_address = bt_ev.event["address"]
            bt_target_user = bt_ev.target_address

            assert bt_target_user is not None

            # Look for a matching Burn event
            paired_burn: ScaledTokenEvent | None = None
            for burn_ev in scaled_events:
                if (
                    burn_ev.event["logIndex"] in assigned_indices
                    or burn_ev.event["logIndex"] in local_assigned
                ):
                    continue
                if burn_ev.event_type != ScaledTokenEventType.COLLATERAL_BURN:
                    continue
                if burn_ev.user_address != bt_target_user:
                    continue
                burn_token_address = burn_ev.event["address"]
                if burn_token_address != bt_token_address:
                    continue

                # Found a paired burn
                paired_burn = burn_ev
                break

            if paired_burn is not None:
                # Create DEFICIT_COVERAGE operation with paired events
                # Include both ERC20 Transfer and BalanceTransfer if both exist
                paired_events = [bt_ev, paired_burn]

                # Check for a matching BalanceTransfer event for the same transfer
                # ERC20 Transfer and BalanceTransfer events represent the same transfer
                # but have different event types and amounts (underlying vs scaled)
                assert bt_ev.event_type == ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER
                for other_ev in scaled_events:
                    if (
                        other_ev.event["logIndex"] in assigned_indices
                        or other_ev.event["logIndex"] in local_assigned
                    ):
                        continue
                    if other_ev.event_type != ScaledTokenEventType.COLLATERAL_TRANSFER:
                        continue
                    if other_ev.from_address != bt_ev.from_address:
                        continue

                    # Found matching BalanceTransfer - include it
                    paired_events.insert(1, other_ev)  # Insert between transfer and burn
                    local_assigned.add(other_ev.event["logIndex"])
                    break

                # Collect BalanceTransfer events for the balance_transfer_events field
                # This allows _should_skip_collateral_transfer to properly skip paired events
                bt_events = [
                    ev.event
                    for ev in paired_events
                    if ev.event_type == ScaledTokenEventType.COLLATERAL_TRANSFER
                ]

                operations.append(
                    Operation(
                        operation_id=operation_id,
                        operation_type=OperationType.DEFICIT_COVERAGE,
                        pool_revision=pool_revision,
                        pool_event=None,
                        scaled_token_events=paired_events,
                        transfer_events=[],
                        balance_transfer_events=bt_events,
                    )
                )
                operation_id += 1
                local_assigned.add(bt_ev.event["logIndex"])
                local_assigned.add(paired_burn.event["logIndex"])

        # Update the passed-in assigned_indices set with locally assigned events
        assigned_indices.update(local_assigned)

        return operations

    @staticmethod
    def _create_interest_accrual_operations(
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        starting_operation_id: int,
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

        operations: list[Operation] = []
        operation_id = starting_operation_id
        local_assigned: set[int] = set()

        for ev in scaled_events:
            # Skip already assigned events
            if ev.event["logIndex"] in assigned_indices or ev.event["logIndex"] in local_assigned:
                continue

            # Only process mint events that represent interest accrual
            if ev.event_type not in {
                ScaledTokenEventType.COLLATERAL_MINT,
                ScaledTokenEventType.DEBT_MINT,
                ScaledTokenEventType.GHO_DEBT_MINT,
            }:
                continue

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

            # Skip interest accrual events (amount == balance_increase means no new tokens)
            # Emitted during transfers/liquidations for tracking, not actual MINT_TO_TREASURY
            if ev.amount == ev.balance_increase:
                logger.debug(
                    f"Skipping interest accrual Mint event at logIndex {ev.event['logIndex']} - "
                    f"amount ({ev.amount}) equals balance_increase ({ev.balance_increase})"
                )
                assigned_indices.add(ev.event["logIndex"])
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

            """
            Event definition:
                event MintedToTreasury(
                    address indexed reserve,
                    uint256 amountMinted
                );
            """

            # The Mint event's address is the aToken, so we need to find the underlying asset
            asset = self._get_asset_by_a_token(a_token_address=ev.event["address"])
            assert asset is not None
            underlying_addr = get_checksum_address(asset.underlying_token.address)

            for mt_ev in minted_to_treasury_events:
                mt_reserve: str
                (mt_reserve,) = eth_abi.abi.decode(types=["address"], data=mt_ev["topics"][1])
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

        # Phase 1: Process ERC20 Transfer events and pair with BalanceTransfer events
        transfer_operations, operation_id = TransactionOperationsParser._process_erc20_transfers(
            scaled_events=scaled_events,
            assigned_indices=assigned_indices,
            local_assigned=local_assigned,
            starting_operation_id=operation_id,
            pool_revision=pool_revision,
        )
        operations.extend(transfer_operations)

        # Update the assigned_indices set with locally assigned events
        assigned_indices.update(local_assigned)

        return operations

    @staticmethod
    def _is_part_of_burn(
        ev: ScaledTokenEvent,
        scaled_events: list[ScaledTokenEvent],
        local_assigned: set[int],
    ) -> bool:
        """
        Check if an ERC20 Transfer to zero address is part of a burn operation.
        """

        ev_token_address = ev.event["address"]
        for other_ev in scaled_events:
            if (
                other_ev.event_type
                in {
                    ScaledTokenEventType.DEBT_BURN,
                    ScaledTokenEventType.COLLATERAL_BURN,
                    ScaledTokenEventType.GHO_DEBT_BURN,
                }
                and other_ev.user_address == ev.from_address
                and other_ev.event["address"] == ev_token_address
            ):
                local_assigned.add(ev.event["logIndex"])
                return True
        return False

    @staticmethod
    def _is_part_of_mint(
        ev: ScaledTokenEvent,
        scaled_events: list[ScaledTokenEvent],
        local_assigned: set[int],
    ) -> bool:
        """
        Check if an ERC20 Transfer from zero address is part of a mint operation.
        """

        ev_token_address = ev.event["address"]
        for other_ev in scaled_events:
            if (
                other_ev.event_type
                in {
                    ScaledTokenEventType.COLLATERAL_MINT,
                    ScaledTokenEventType.DEBT_MINT,
                    ScaledTokenEventType.GHO_DEBT_MINT,
                }
                and other_ev.user_address == ev.target_address
                and other_ev.event["address"] == ev_token_address
            ):
                local_assigned.add(ev.event["logIndex"])
                return True
        return False

    @staticmethod
    def _find_matching_balance_transfer(
        scaled_token_event: ScaledTokenEvent,
        all_scaled_token_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        local_assigned: set[int],
    ) -> ScaledTokenEvent | None:
        """
        Find a matching BalanceTransfer event for an ERC20 Transfer.
        """

        # Look in unassigned events first
        for bt_ev in all_scaled_token_events:
            if (
                bt_ev.event["logIndex"] in assigned_indices
                or bt_ev.event["logIndex"] in local_assigned
            ):
                continue
            if bt_ev.index is None:  # Skip ERC20 Transfers
                continue
            if (
                bt_ev.from_address == scaled_token_event.from_address
                and bt_ev.target_address == scaled_token_event.target_address
                and bt_ev.event["address"] == scaled_token_event.event["address"]
                and TransactionOperationsParser._are_compatible_transfer_types(
                    bt_ev, scaled_token_event
                )
            ):
                local_assigned.add(bt_ev.event["logIndex"])
                return bt_ev

        return None

    @staticmethod
    def _process_erc20_transfers(
        scaled_events: list[ScaledTokenEvent],
        assigned_indices: set[int],
        local_assigned: set[int],
        starting_operation_id: int,
        pool_revision: int,
    ) -> tuple[list[Operation], int]:
        """Create operations for ERC20 Transfer events, pairing with BalanceTransfer when found."""
        operations: list[Operation] = []
        operation_id = starting_operation_id

        for ev in scaled_events:
            if ev.event["logIndex"] in assigned_indices or ev.event["logIndex"] in local_assigned:
                continue

            assert ev.event_type in {
                ScaledTokenEventType.COLLATERAL_TRANSFER,
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.DISCOUNT_TRANSFER,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                ScaledTokenEventType.GHO_DEBT_TRANSFER,
            }

            # Only ERC20 Transfer events should make it here (index=None means no index from event)
            assert ev.index is None

            # Skip transfers to/from zero address that are part of mints/burns
            if ev.target_address == ZERO_ADDRESS and TransactionOperationsParser._is_part_of_burn(
                ev, scaled_events, local_assigned
            ):
                continue
            if ev.from_address == ZERO_ADDRESS and TransactionOperationsParser._is_part_of_mint(
                ev, scaled_events, local_assigned
            ):
                continue

            balance_transfer_events = []

            # Find matching BalanceTransfer event
            balance_transfer_event = TransactionOperationsParser._find_matching_balance_transfer(
                scaled_token_event=ev,
                all_scaled_token_events=scaled_events,
                assigned_indices=assigned_indices,
                local_assigned=local_assigned,
            )
            if balance_transfer_event:
                balance_transfer_events.append(balance_transfer_event.event)

            # Create operation
            operation_type = (
                OperationType.STKAAVE_TRANSFER
                if ev.event_type == ScaledTokenEventType.DISCOUNT_TRANSFER
                else OperationType.BALANCE_TRANSFER
            )
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

        return operations, operation_id

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
            OperationType.INTEREST_ACCRUAL: self._validate_interest_accrual,
            OperationType.DEFICIT_COVERAGE: self._validate_deficit_coverage,
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
            assert_never()

    @staticmethod
    def _validate_supply(op: Operation) -> list[str]:
        """Validate SUPPLY operation."""
        errors = []

        assert op.pool_event is not None, "Missing SUPPLY pool event"

        # Should have exactly 1 collateral mint
        collateral_mints = [e for e in op.scaled_token_events if e.is_collateral]
        assert len(collateral_mints) == 1, (
            f"Expected 1 collateral mint for SUPPLY, got {len(collateral_mints)}"
        )

        return errors

    @staticmethod
    def _validate_withdraw(op: Operation) -> list[str]:
        """Validate WITHDRAW operation."""
        errors = []

        assert op.pool_event is not None, "Missing WITHDRAW pool event"

        # Should have at most 1 collateral burn
        collateral_burns = [e for e in op.scaled_token_events if e.is_collateral and e.is_burn]
        assert len(collateral_burns) <= 1, (
            f"Expected at most 1 collateral burn for WITHDRAW, got {len(collateral_burns)}"
        )

        return errors

    @staticmethod
    def _validate_borrow(op: Operation) -> list[str]:
        """Validate BORROW operation."""
        errors = []

        assert op.pool_event is not None, "Missing BORROW pool event"

        # Should have exactly 1 debt mint
        debt_mints = [e for e in op.scaled_token_events if e.is_debt]
        assert len(debt_mints) == 1, f"Expected 1 debt mint for BORROW, got {len(debt_mints)}"

        return errors

    def _validate_gho_borrow(self, op: Operation) -> list[str]:
        """Validate GHO BORROW operation."""
        errors = self._validate_borrow(op)

        gho_mints = [
            e for e in op.scaled_token_events if e.event_type == ScaledTokenEventType.GHO_DEBT_MINT
        ]
        assert len(gho_mints) == 1, f"Expected 1 GHO debt mint for GHO_BORROW, got {len(gho_mints)}"

        return errors

    @staticmethod
    def _validate_repay(op: Operation) -> list[str]:
        """Validate REPAY operation."""
        errors = []

        assert op.pool_event is not None, "Missing REPAY pool event"

        debt_burns = [e for e in op.scaled_token_events if e.is_debt]
        assert len(debt_burns) == 1

        return errors

    @staticmethod
    def _validate_repay_with_atokens(op: Operation) -> list[str]:
        """Validate REPAY_WITH_ATOKENS operation."""
        errors = []

        assert op.pool_event is not None, "Missing REPAY pool event"

        # Should have 0 or 1 debt events (burn or mint) and 0 or 1 collateral burn
        # Note: When interest exceeds repayment, debt mints instead of burns
        # Note: In some edge cases, debt burn may not be emitted if debt is fully covered by
        # interest
        # Edge case: Collateral burn may be absent if user has no aToken balance or when
        # repayment is handled via flash loan / adapter contract
        # See TX 0x1a7d205b9831cc63c545ba5ddf21c2fc29c00973ac680fc6371e3aa999f60f19
        debt_events = [e for e in op.scaled_token_events if e.is_debt]
        assert len(debt_events) == 1

        collateral_burns = [e for e in op.scaled_token_events if e.is_collateral and e.is_burn]
        assert len(collateral_burns) <= 1

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
            in {
                ScaledTokenEventType.GHO_DEBT_BURN,
                ScaledTokenEventType.GHO_DEBT_MINT,
            }
        ]
        assert len(gho_events) == 1

        return errors

    @staticmethod
    def _validate_liquidation(op: Operation) -> list[str]:
        """Validate LIQUIDATION operation."""
        errors = []

        assert op.pool_event is not None, "Missing LIQUIDATION_CALL pool event"

        # Should have 1 collateral event (burn or transfer) and any number of debt burns
        # Flash loan liquidations have 0 debt burns (debt repaid via flash loan)
        # Standard liquidations have 1 debt burn (primary debt asset)
        # Multi-asset liquidations may have multiple debt burns (primary + secondary debts)
        # Collateral may be burned OR transferred to treasury (BalanceTransfer)

        collateral_events = [e for e in op.scaled_token_events if e.is_collateral]
        assert len(collateral_events) > 0

        return errors

    def _validate_gho_liquidation(self, op: Operation) -> list[str]:
        """Validate GHO LIQUIDATION operation."""
        return self._validate_liquidation(op)

    @staticmethod
    def _validate_interest_accrual(op: Operation) -> list[str]:
        """Validate INTEREST_ACCRUAL operation.

        Interest accrual operations have no pool event. The scaled token event
        represents pure interest accrual where amount == balance_increase.
        Also includes dust mints (balance_increase == 0) from discount updates
        that still need to update the user's last_index.
        """
        errors = []

        assert op.pool_event is None, "INTEREST_ACCRUAL should not have a pool event"
        assert len(op.scaled_token_events) == 1, (
            f"Expected 1 scaled token event for INTEREST_ACCRUAL, got {len(op.scaled_token_events)}"
        )

        return errors

    @staticmethod
    def _validate_balance_transfer(op: Operation) -> list[str]:
        """Validate BALANCE_TRANSFER operation."""
        errors = []

        assert op.pool_event is None, "BALANCE_TRANSFER should not have a pool event"
        assert len(op.scaled_token_events) == 1, (
            f"Expected 1 scaled token event for BALANCE_TRANSFER, got {len(op.scaled_token_events)}"
        )

        assert op.scaled_token_events[0].event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
            ScaledTokenEventType.DEBT_TRANSFER,
            ScaledTokenEventType.ERC20_DEBT_TRANSFER,
            ScaledTokenEventType.GHO_DEBT_TRANSFER,
        }, (
            f"BALANCE_TRANSFER event should be a transfer type, "
            f"got {op.scaled_token_events[0].event_type}"
        )

        return errors

    @staticmethod
    def _validate_deficit_coverage(op: Operation) -> list[str]:
        """Validate DEFICIT_COVERAGE operation.

        DEFICIT_COVERAGE operations group paired BalanceTransfer + Burn events
        that occur during Umbrella protocol's deficit coverage operations.
        May include both ERC20 Transfer and BalanceTransfer events for the same transfer.
        """
        errors = []

        assert op.pool_event is None, "DEFICIT_COVERAGE should not have a pool event"

        # Validate the events are transfer(s) and burn for the same user/asset
        # First event should be a transfer
        first_ev = op.scaled_token_events[0]
        assert first_ev.event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }, f"DEFICIT_COVERAGE first event should be a transfer, got {first_ev.event_type}"

        # Last event should be a burn
        last_ev = op.scaled_token_events[-1]
        assert last_ev.event_type == ScaledTokenEventType.COLLATERAL_BURN, (
            f"DEFICIT_COVERAGE last event should be COLLATERAL_BURN, got {last_ev.event_type}"
        )

        # All events should be for the same token
        first_token = first_ev.event["address"]
        last_token = last_ev.event["address"]
        assert first_token == last_token, (
            f"DEFICIT_COVERAGE events should be for the same token: {first_token} != {last_token}"
        )

        # Burn should be from the transfer recipient
        assert last_ev.user_address == first_ev.target_address, (
            f"DEFICIT_COVERAGE burn user ({last_ev.user_address}) should match "
            f"transfer recipient ({first_ev.target_address})"
        )

        return errors

    @staticmethod
    def _validate_mint_to_treasury(op: Operation) -> list[str]:
        """Validate MINT_TO_TREASURY operation."""
        errors = []

        # Should have no pool event (treasury mints are standalone)
        assert op.pool_event is None, "MINT_TO_TREASURY should not have a pool event"

        # Should have exactly 1 scaled token event (the mint)
        assert len(op.scaled_token_events) == 1, (
            f"Expected 1 scaled token event for MINT_TO_TREASURY, got {len(op.scaled_token_events)}"
        )

        # The event should be a collateral mint
        assert op.scaled_token_events[0].event_type == ScaledTokenEventType.COLLATERAL_MINT, (
            f"MINT_TO_TREASURY event should be COLLATERAL_MINT, "
            f"got {op.scaled_token_events[0].event_type}"
        )

        return errors

    @staticmethod
    def _validate_stkaave_transfer(op: Operation) -> list[str]:
        """Validate STKAAVE_TRANSFER operation."""
        errors = []

        # Should have no pool event (transfers are standalone ERC20 events)
        assert op.pool_event is None, "STKAAVE_TRANSFER should not have a pool event"

        # Should have exactly 1 scaled token event (the transfer)
        assert len(op.scaled_token_events) == 1, (
            f"Expected 1 scaled token event for STKAAVE_TRANSFER, got {len(op.scaled_token_events)}"
        )

        # The event should be a discount transfer
        assert op.scaled_token_events[0].event_type == ScaledTokenEventType.DISCOUNT_TRANSFER, (
            f"STKAAVE_TRANSFER event should be DISCOUNT_TRANSFER, "
            f"got {op.scaled_token_events[0].event_type}"
        )

        return errors
