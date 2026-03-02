"""Aave V3 event matching framework.

Centralizes pool event matching logic for scaled token events (Mint/Burn).
This module provides a declarative, testable approach to matching Aave V3
Pool events with scaled token events, handling complex edge cases like:
- Liquidation transactions (LIQUIDATION_CALL shared across multiple operations)
- Repay with aTokens (REPAY event shared across debt and collateral burns)
- Flash loan liquidations (debt burns without matching Pool events)

See debug/aave/ for detailed transaction examples and bug reports.

References:
- Bug #0002: Mint Events Incorrectly Match SUPPLY Instead of WITHDRAW
- Bug #0008: Repay with aTokens - Duplicate Burn Events Match Same Repay Event
- Bug #0009: Collateral Burn Events Miss LiquidationCall Matching
- Bug #0010: GHO Debt Burn Consumes LiquidationCall Event Blocking Collateral Burn
- Bug #0011: Collateral Mint Events Miss LiquidationCall Matching
- Bug #0012a: Collateral Burn Fails When LIQUIDATION_CALL Already Consumed
- Bug #0012b: Collateral Operations Consume LIQUIDATION_CALL Events
- Bug #0013: Debt Burn Without Matching Pool Event
- Bug #0014: BalanceTransfer Skipped After Pure Interest Mint
- Bug #0015: Collateral Mint Events Miss REPAY Matching for repayWithATokens
"""

from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum, auto
from typing import ClassVar, Protocol, TypedDict

from eth_abi.abi import decode
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.events import AaveV3PoolEvent
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave_transaction_operations import Operation, OperationType, ScaledTokenEvent
from degenbot.exceptions import DegenbotValueError
from degenbot.logging import logger


class TransactionContext(Protocol):
    """Protocol for transaction context.

    Defines the interface needed by EventMatcher without importing
    the actual TransactionContext class from aave.py (avoiding circular imports).
    """

    pool_events: list[LogReceipt]
    matched_pool_events: dict[int, bool]


class EventMatchError(DegenbotValueError):
    """Raised when event matching fails.

    Provides detailed error messages including available pool events
    for debugging purposes.
    """

    def __init__(
        self,
        message: str,
        *,
        tx_hash: HexBytes | None = None,
        user_address: ChecksumAddress | None = None,
        reserve_address: ChecksumAddress | None = None,
        available_events: list[str] | None = None,
    ) -> None:
        self.tx_hash = tx_hash
        self.user_address = user_address
        self.reserve_address = reserve_address
        self.available_events = available_events or []
        super().__init__(message)


class EventConsumptionPolicy(Enum):
    """Policy for consuming pool events after matching.

    - CONSUMABLE: Mark event as consumed after first match (e.g., SUPPLY, WITHDRAW)
    - REUSABLE: Never mark as consumed (e.g., LIQUIDATION_CALL, DEFICIT_CREATED)
    - CONDITIONAL: Consumed based on event data (e.g., REPAY with useATokens flag)

    See debug/aave/0010, 0011, 0012 for LIQUIDATION_CALL reuse patterns.
    See debug/aave/0008, 0012b for REPAY conditional consumption patterns.
    """

    CONSUMABLE = auto()
    REUSABLE = auto()
    CONDITIONAL = auto()


class ScaledTokenEventType(Enum):
    """Types of scaled token events that require pool event matching."""

    COLLATERAL_MINT = auto()
    COLLATERAL_BURN = auto()
    DEBT_MINT = auto()
    DEBT_BURN = auto()
    GHO_DEBT_MINT = auto()
    GHO_DEBT_BURN = auto()


@dataclass(frozen=True)
class MatchConfig:
    """Configuration for matching a scaled token event to pool events.

    Attributes:
        target_event: The scaled token event type being matched
        pool_event_types: Ordered list of pool event types to try matching
        consumption_policy: How to handle event consumption
        consumption_condition: Optional function to determine if event should be consumed
        match_order_priority: Whether to respect the order in pool_event_types
    """

    target_event: ScaledTokenEventType
    pool_event_types: list[AaveV3PoolEvent] = field(default_factory=list)
    consumption_policy: EventConsumptionPolicy = EventConsumptionPolicy.CONSUMABLE
    consumption_condition: Callable[[LogReceipt], bool] | None = None
    match_order_priority: bool = True


class EventMatchResult(TypedDict):
    """Result of a successful event match."""

    pool_event: LogReceipt | None
    should_consume: bool
    extraction_data: dict[str, int]


class EventMatcher:
    """Centralized pool event matching for scaled token events.

    This class provides a declarative approach to matching Aave V3 Pool events
    with scaled token events (Mint/Burn), handling complex edge cases like:
    - Shared LIQUIDATION_CALL events in liquidations
    - Conditional REPAY consumption for repay-with-aTokens
    - Debt burns without matching Pool events (flash loans)

    Usage:
        matcher = EventMatcher(tx_context)
        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            user_address=user.address,
            reserve_address=reserve_address,
        )
        if result is None:
            raise EventMatchError("No matching event found")

        pool_event = result["pool_event"]
        should_consume = result["should_consume"]
        scaled_amount = result["extraction_data"].get("scaled_amount")
    """

    # Single source of truth for all matching configurations
    # See debug/aave/ for bug reports justifying each configuration
    CONFIGS: ClassVar[dict[ScaledTokenEventType, MatchConfig]] = {
        # Collateral Mint: Can match SUPPLY (deposit), WITHDRAW (interest before withdraw),
        # LIQUIDATION_CALL (liquidator receiving collateral), or REPAY (excess aTokens returned
        # during repayWithATokens)
        # SUPPLY/WITHDRAW: consumed (single-purpose)
        # LIQUIDATION_CALL: never consumed (shared across liquidation operations)
        # REPAY: never consumed (may be shared with debt burns for repay-with-aTokens)
        # See debug/aave/0011, 0015
        ScaledTokenEventType.COLLATERAL_MINT: MatchConfig(
            target_event=ScaledTokenEventType.COLLATERAL_MINT,
            pool_event_types=[
                AaveV3PoolEvent.SUPPLY,
                AaveV3PoolEvent.WITHDRAW,
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=lambda e: _should_consume_collateral_mint_pool_event(e),
        ),
        # Collateral Burn: Can match WITHDRAW (withdrawal), REPAY (repay with aTokens),
        # or LIQUIDATION_CALL (collateral seized)
        # REPAY only consumed if useATokens=False
        # LIQUIDATION_CALL never consumed
        # See debug/aave/0008, 0009
        ScaledTokenEventType.COLLATERAL_BURN: MatchConfig(
            target_event=ScaledTokenEventType.COLLATERAL_BURN,
            pool_event_types=[
                AaveV3PoolEvent.WITHDRAW,
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=lambda e: _should_consume_collateral_burn_pool_event(e),
        ),
        # Debt Mint: Can match BORROW (borrow), REPAY (interest before repayment),
        # or LIQUIDATION_CALL (liquidator borrowing to fund liquidation)
        # REPAY and LIQUIDATION_CALL never consumed (shared across operations)
        # See debug/aave/0011, 0012b
        ScaledTokenEventType.DEBT_MINT: MatchConfig(
            target_event=ScaledTokenEventType.DEBT_MINT,
            pool_event_types=[
                AaveV3PoolEvent.BORROW,
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=lambda e: _should_consume_debt_mint_pool_event(e),
        ),
        # Debt Burn: Can match REPAY (repayment), LIQUIDATION_CALL (debt repaid during liquidation),
        # or DEFICIT_CREATED (bad debt write-off)
        # LIQUIDATION_CALL and DEFICIT_CREATED never consumed
        # REPAY only consumed if useATokens=False
        # See debug/aave/0008, 0010, 0012a, 0013
        ScaledTokenEventType.DEBT_BURN: MatchConfig(
            target_event=ScaledTokenEventType.DEBT_BURN,
            pool_event_types=[
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
                AaveV3PoolEvent.DEFICIT_CREATED,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=lambda e: _should_consume_debt_burn_pool_event(e),
        ),
        # GHO Debt Mint: Can match BORROW or REPAY
        # REPAY never consumed (shared with collateral burns)
        # See debug/aave/0012b
        ScaledTokenEventType.GHO_DEBT_MINT: MatchConfig(
            target_event=ScaledTokenEventType.GHO_DEBT_MINT,
            pool_event_types=[AaveV3PoolEvent.BORROW, AaveV3PoolEvent.REPAY],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=lambda e: _should_consume_gho_debt_mint_pool_event(e),
        ),
        # GHO Debt Burn: Can match REPAY, LIQUIDATION_CALL, or DEFICIT_CREATED
        # LIQUIDATION_CALL and DEFICIT_CREATED never consumed (shared across operations)
        # DEFICIT_CREATED is used for GHO liquidations (bad debt write-off mechanism)
        # See debug/aave/0010, 0016
        ScaledTokenEventType.GHO_DEBT_BURN: MatchConfig(
            target_event=ScaledTokenEventType.GHO_DEBT_BURN,
            pool_event_types=[
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
                AaveV3PoolEvent.DEFICIT_CREATED,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=lambda e: _should_consume_gho_debt_burn_pool_event(e),
        ),
    }

    def __init__(self, tx_context: TransactionContext) -> None:
        """Initialize matcher with transaction context.

        Args:
            tx_context: Transaction context containing pool events and consumption state
        """
        self.tx_context = tx_context
        self._event_index: dict[HexBytes, list[LogReceipt]] | None = None

    def _build_event_index(self) -> dict[HexBytes, list[LogReceipt]]:
        """Build index of pool events by topic for O(1) lookup.

        This is lazily built on first access to avoid overhead for simple cases.
        """
        if self._event_index is None:
            self._event_index = {}
            for event in self.tx_context.pool_events:
                topic = event["topics"][0]
                if topic not in self._event_index:
                    self._event_index[topic] = []
                self._event_index[topic].append(event)
        return self._event_index

    def _is_consumed(self, pool_event: LogReceipt) -> bool:
        """Check if a pool event has already been consumed."""
        return self.tx_context.matched_pool_events.get(pool_event["logIndex"], False)

    def _mark_consumed(self, pool_event: LogReceipt) -> None:
        """Mark a pool event as consumed."""
        self.tx_context.matched_pool_events[pool_event["logIndex"]] = True

    @staticmethod
    def _should_consume(pool_event: LogReceipt, config: MatchConfig) -> bool:
        """Determine if a matched pool event should be consumed.

        Args:
            pool_event: The matched pool event
            config: Match configuration for the scaled token event

        Returns:
            True if the event should be marked as consumed, False otherwise
        """
        if config.consumption_policy == EventConsumptionPolicy.CONSUMABLE:
            return True
        if config.consumption_policy == EventConsumptionPolicy.REUSABLE:
            return False
        if config.consumption_policy == EventConsumptionPolicy.CONDITIONAL:
            if config.consumption_condition is not None:
                return config.consumption_condition(pool_event)
            return True
        return True

    @staticmethod
    def _matches_pool_event(
        pool_event: LogReceipt,
        expected_type: AaveV3PoolEvent,
        user_address: ChecksumAddress,
        reserve_address: ChecksumAddress,
    ) -> bool:
        """Check if a pool event matches expected criteria.

        This implements the matching logic previously in _matches_pool_event()
        in aave.py, but centralized and documented.

        Args:
            pool_event: Pool event to check
            expected_type: Expected event type
            user_address: User address to match
            reserve_address: Reserve/token address to match

        Returns:
            True if the event matches, False otherwise
        """
        event_topic = pool_event["topics"][0]
        event_log_index = pool_event["logIndex"]

        # DEBUG: Log matching attempt
        logger.debug(
            f"_matches_pool_event: checking event at logIndex {event_log_index} "
            f"(topic={event_topic.hex()[:10]}) against expected_type={expected_type.name}, "
            f"user={user_address}, reserve={reserve_address}"
        )

        # Allow LIQUIDATION_CALL when expecting REPAY or WITHDRAW
        # Allow DEFICIT_CREATED when expecting REPAY (bad debt write-off)
        if (
            event_topic != expected_type.value
            and not (
                event_topic == AaveV3PoolEvent.LIQUIDATION_CALL.value
                and expected_type in {AaveV3PoolEvent.REPAY, AaveV3PoolEvent.WITHDRAW}
            )
            and not (
                event_topic == AaveV3PoolEvent.DEFICIT_CREATED.value
                and expected_type == AaveV3PoolEvent.REPAY
            )
        ):
            logger.debug(
                f"  -> NO MATCH: event topic {event_topic.hex()[:10]} != expected {expected_type.value.hex()[:10]}"
            )
            return False

        if expected_type == AaveV3PoolEvent.BORROW:
            # BORROW: topics[1]=reserve, topics[2]=onBehalfOf, data=(amount, interestRateMode)
            event_reserve = _decode_address(pool_event["topics"][1])
            event_on_behalf_of = _decode_address(pool_event["topics"][2])
            (_, _, interest_rate_mode, _) = decode(
                types=["address", "uint256", "uint8", "uint256"],
                data=pool_event["data"],
            )
            matches = (
                event_on_behalf_of == user_address
                and event_reserve == reserve_address
                and interest_rate_mode == 2  # Variable rate
            )
            logger.debug(
                f"  -> BORROW check: event_reserve={event_reserve}, "
                f"event_on_behalf_of={event_on_behalf_of}, interest_rate_mode={interest_rate_mode}, "
                f"matches={matches}"
            )
            return matches

        if expected_type == AaveV3PoolEvent.REPAY:
            if event_topic == AaveV3PoolEvent.REPAY.value:
                # REPAY: topics[1]=reserve, topics[2]=user, data=(amount, useATokens)
                event_reserve = _decode_address(pool_event["topics"][1])
                event_user = _decode_address(pool_event["topics"][2])

                # Decode amount from REPAY data
                (payback_amount, _) = decode(
                    types=["uint256", "bool"],
                    data=pool_event["data"],
                )

                # Skip REPAY events with amount=0 (flash loan repayment via direct transfer)
                # The actual debt reduction is captured in the Burn event
                if payback_amount == 0:
                    logger.warning("  -> REPAY check: SKIPPING (amount=0, flash loan repayment)")
                    return False

                # Compare addresses case-insensitively (both should be checksummed but may differ in casing)
                matches = event_user == user_address and event_reserve == reserve_address
                logger.debug(
                    f"  -> REPAY check: event_user={event_user}, event_reserve={event_reserve}, "
                    f"amount={payback_amount}, matches={matches}"
                )
                return matches
            if event_topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
                # Liquidation matching - match on debtAsset
                event_debt_asset = _decode_address(pool_event["topics"][2])
                event_user = _decode_address(pool_event["topics"][3])
                matches = event_user == user_address and event_debt_asset == reserve_address
                logger.debug(
                    f"  -> LIQUIDATION_CALL(as REPAY) check: event_user={event_user}, "
                    f"event_debt_asset={event_debt_asset}, matches={matches}"
                )
                return matches
            if event_topic == AaveV3PoolEvent.DEFICIT_CREATED.value:
                # DeficitCreated matching - debt written off
                event_user = _decode_address(pool_event["topics"][1])
                event_reserve = _decode_address(pool_event["topics"][2])
                matches = event_user == user_address and event_reserve == reserve_address
                logger.debug(
                    f"  -> DEFICIT_CREATED(as REPAY) check: event_user={event_user}, "
                    f"event_reserve={event_reserve}, matches={matches}"
                )
                return matches

        elif expected_type == AaveV3PoolEvent.SUPPLY:
            # SUPPLY: topics[1]=reserve, topics[2]=onBehalfOf, topics[3]=referralCode
            # Aave V3 Supply event format (4 topics):
            #   Supply(address indexed reserve, address indexed onBehalfOf,
            #          uint16 indexed referralCode, address caller, uint256 amount)
            # topics[3] is referralCode (uint16), NOT an address - do not decode as address!
            event_reserve = _decode_address(pool_event["topics"][1])
            event_on_behalf_of = _decode_address(pool_event["topics"][2])
            matches = event_on_behalf_of == user_address and event_reserve == reserve_address
            logger.debug(
                f"  -> SUPPLY check: event_reserve={event_reserve}, "
                f"event_on_behalf_of={event_on_behalf_of}, matches={matches}"
            )
            return matches

        elif expected_type == AaveV3PoolEvent.WITHDRAW:
            if event_topic == AaveV3PoolEvent.WITHDRAW.value:
                # WITHDRAW: topics[1]=reserve, topics[2]=user
                event_reserve = _decode_address(pool_event["topics"][1])
                event_user = _decode_address(pool_event["topics"][2])
                matches = event_user == user_address and event_reserve == reserve_address
                logger.debug(
                    f"  -> WITHDRAW check: event_user={event_user}, event_reserve={event_reserve}, "
                    f"matches={matches}"
                )
                return matches
            if event_topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
                # Liquidation matching - match on collateralAsset
                event_collateral_asset = _decode_address(pool_event["topics"][1])
                event_user = _decode_address(pool_event["topics"][3])
                matches = event_user == user_address and event_collateral_asset == reserve_address
                logger.debug(
                    f"  -> LIQUIDATION_CALL(as WITHDRAW) check: event_user={event_user}, "
                    f"event_collateral_asset={event_collateral_asset}, matches={matches}"
                )
                return matches

        elif expected_type == AaveV3PoolEvent.LIQUIDATION_CALL:
            # LIQUIDATION_CALL: topics[1]=collateralAsset, topics[2]=debtAsset, topics[3]=user
            event_collateral_asset = _decode_address(pool_event["topics"][1])
            event_debt_asset = _decode_address(pool_event["topics"][2])
            event_user = _decode_address(pool_event["topics"][3])
            if event_user == user_address:
                matches = reserve_address in {event_debt_asset, event_collateral_asset}
                logger.debug(
                    f"  -> LIQUIDATION_CALL check: event_user={event_user}, "
                    f"event_collateral_asset={event_collateral_asset}, event_debt_asset={event_debt_asset}, "
                    f"matches={matches}"
                )
                return matches

        elif expected_type == AaveV3PoolEvent.DEFICIT_CREATED:
            # DEFICIT_CREATED: topics[1]=user, topics[2]=asset, data=(uint256 amountCreated)
            # Used for GHO liquidations and bad debt write-offs
            event_user = _decode_address(pool_event["topics"][1])
            event_asset = _decode_address(pool_event["topics"][2])
            matches = event_user == user_address and event_asset == reserve_address
            logger.debug(
                f"  -> DEFICIT_CREATED check: event_user={event_user}, "
                f"event_asset={event_asset}, matches={matches}"
            )
            return matches

        logger.debug(f"  -> NO MATCH: unexpected expected_type={expected_type.name}")
        return False

    def find_matching_pool_event(
        self,
        event_type: ScaledTokenEventType,
        user_address: ChecksumAddress,
        reserve_address: ChecksumAddress,
        *,
        check_users: list[ChecksumAddress] | None = None,
        try_event_type_first: AaveV3PoolEvent | None = None,
    ) -> EventMatchResult | None:
        """Find a matching pool event for a scaled token event.

        This is the main entry point for event matching. It searches through
        available pool events in the transaction context, trying each configured
        event type in order until a match is found.

        Matching is not constrained by event ordering (logIndex). Pool events can
        appear before or after their corresponding token events in complex transactions.
        Event consumption tracking prevents double-matching.

        Args:
            event_type: Type of scaled token event being matched
            user_address: Primary user address to match
            reserve_address: Reserve/token address to match
            check_users: Optional additional user addresses to try (e.g., caller_address)
            try_event_type_first: Optional event type to try first before the config order.
                Used for interest accrual mints to try WITHDRAW before SUPPLY.

        Returns:
            EventMatchResult with pool_event, should_consume flag, and extraction_data,
            or None if no match found

        Raises:
            EventMatchError: If matching fails and strict mode is enabled
        """
        config = self.CONFIGS.get(event_type)
        if config is None:
            msg = f"Unknown scaled token event type: {event_type}"
            raise EventMatchError(msg, user_address=user_address, reserve_address=reserve_address)

        # DEBUG: Log entry into event matching
        logger.debug(
            f"EventMatcher.find_matching_pool_event called: "
            f"event_type={event_type.name}, user={user_address}, reserve={reserve_address}"
        )

        # Build list of user addresses to check
        users_to_check = [user_address]
        if check_users:
            users_to_check.extend(check_users)

        # Build the list of event types to try
        event_types_to_try = config.pool_event_types.copy()
        if try_event_type_first is not None:
            # Find the matching event type by value since direct enum comparison may fail
            matching_type = None
            for et in event_types_to_try:
                if et.value == try_event_type_first.value:
                    matching_type = et
                    break
            if matching_type is not None:
                # Move the specified event type to the front
                event_types_to_try.remove(matching_type)
                event_types_to_try.insert(0, matching_type)

        # Try each pool event type in order
        for expected_type in event_types_to_try:
            logger.debug(
                f"Trying pool event type: {expected_type.name} (looking for {event_type.name})"
            )
            for user in users_to_check:
                # Use index for O(1) lookup if available, otherwise iterate
                event_index = self._build_event_index()
                events_of_type = event_index.get(expected_type.value, [])

                logger.debug(
                    f"Found {len(events_of_type)} events of type {expected_type.name} "
                    f"for user {user}"
                )

                for pool_event in events_of_type:
                    event_log_index = pool_event["logIndex"]
                    event_topic = pool_event["topics"][0].hex()[:10]

                    if self._is_consumed(pool_event):
                        logger.debug(
                            f"Skipping consumed event at logIndex {event_log_index} "
                            f"(topic={event_topic})"
                        )
                        continue

                    matches = self._matches_pool_event(
                        pool_event, expected_type, user, reserve_address
                    )
                    if matches:
                        logger.debug(
                            f"✓ MATCHED {expected_type.name} event at logIndex {event_log_index} "
                            f"for user {user} (reserve: {reserve_address})"
                        )
                        should_consume = self._should_consume(pool_event, config)
                        logger.debug(f"Consumption: should_consume={should_consume}")
                        if should_consume:
                            self._mark_consumed(pool_event)

                        extraction_data = self._extract_event_data(pool_event, event_type)
                        logger.debug(f"Extracted data: {extraction_data}")

                        return EventMatchResult(
                            pool_event=pool_event,
                            should_consume=should_consume,
                            extraction_data=extraction_data,
                        )
                    logger.debug(
                        f"✗ NO MATCH for {expected_type.name} event at logIndex {event_log_index} "
                        f"(topic={event_topic}, user={user}, reserve={reserve_address})"
                    )

        # No match found
        logger.debug(
            f"NO MATCH FOUND for {event_type.name}. "
            f"Tried users: {users_to_check}, reserve: {reserve_address}. "
            f"Available pool events: {len(self.tx_context.pool_events)}"
        )
        return None

    @staticmethod
    def _extract_event_data(
        pool_event: LogReceipt,
        event_type: ScaledTokenEventType,
    ) -> dict[str, int]:
        """Extract relevant data from a matched pool event.

        Returns a dictionary with extracted values depending on event type:
        - For SUPPLY: {'raw_amount': amount}
        - For WITHDRAW: {'raw_amount': amount}
        - For BORROW: {'raw_amount': amount}
        - For REPAY: {'raw_amount': amount, 'use_a_tokens': bool}
        - For LIQUIDATION_CALL: {'debt_to_cover': amount, 'liquidated_collateral': amount}
        - For DEFICIT_CREATED: {'amount_created': amount}
        """
        event_topic = pool_event["topics"][0]
        result: dict[str, int] = {}

        if event_topic == AaveV3PoolEvent.SUPPLY.value:
            # SUPPLY: data=(address caller, uint256 amount)
            (_, raw_amount) = decode(
                types=["address", "uint256"],
                data=pool_event["data"],
            )
            result["raw_amount"] = raw_amount

        elif event_topic == AaveV3PoolEvent.WITHDRAW.value:
            # WITHDRAW: data=(uint256 amount)
            (raw_amount,) = decode(
                types=["uint256"],
                data=pool_event["data"],
            )
            result["raw_amount"] = raw_amount

        elif event_topic == AaveV3PoolEvent.BORROW.value:
            # BORROW: data=(address caller, uint256 amount, uint8 interestRateMode, uint256 borrowRate)
            (_, raw_amount, _, _) = decode(
                types=["address", "uint256", "uint8", "uint256"],
                data=pool_event["data"],
            )
            result["raw_amount"] = raw_amount

        elif event_topic == AaveV3PoolEvent.REPAY.value:
            # REPAY: data=(uint256 amount, bool useATokens)
            raw_amount, use_a_tokens = decode(
                types=["uint256", "bool"],
                data=pool_event["data"],
            )
            result["raw_amount"] = raw_amount
            result["use_a_tokens"] = int(use_a_tokens)  # Store as int for TypedDict

        elif event_topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
            # LIQUIDATION_CALL: data=(uint256 debtToCover, uint256 liquidatedCollateralAmount,
            #                          address liquidator, bool receiveAToken)
            debt_to_cover, liquidated_collateral, _, _ = decode(
                types=["uint256", "uint256", "address", "bool"],
                data=pool_event["data"],
            )
            result["debt_to_cover"] = debt_to_cover
            result["liquidated_collateral"] = liquidated_collateral

        elif event_topic == AaveV3PoolEvent.DEFICIT_CREATED.value:
            # DEFICIT_CREATED: data=(uint256 amountCreated)
            (amount_created,) = decode(
                types=["uint256"],
                data=pool_event["data"],
            )
            result["amount_created"] = amount_created

        return result


# Consumption condition functions
# These determine whether a matched pool event should be marked as consumed


def _should_consume_collateral_burn_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a collateral burn's pool event should be consumed.

    Consumption rules:
    - WITHDRAW: Always consumed (single-purpose event)
    - REPAY: Consumed only if useATokens=False
      (when useATokens=True, REPAY is shared with debt burn)
    - LIQUIDATION_CALL: Never consumed (shared across operations)

    See debug/aave/0008 for repay-with-aTokens pattern.
    See debug/aave/0009 for LIQUIDATION_CALL reuse pattern.
    """
    event_topic = pool_event["topics"][0]

    if event_topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
        return False

    if event_topic == AaveV3PoolEvent.REPAY.value:
        # REPAY: data=(uint256 amount, bool useATokens)
        _, use_a_tokens = decode(
            types=["uint256", "bool"],
            data=pool_event["data"],
        )
        return not use_a_tokens

    # WITHDRAW and other events are consumable
    return True


def _should_consume_collateral_mint_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a collateral mint's pool event should be consumed.

    Consumption rules:
    - SUPPLY: Always consumed (single-purpose event)
    - WITHDRAW: Always consumed (single-purpose event)
    - REPAY: Never consumed (shared with debt burns for repay-with-aTokens)
    - LIQUIDATION_CALL: Never consumed (shared across liquidation operations)

    See debug/aave/0011 for LIQUIDATION_CALL matching in collateral mints.
    See debug/aave/0015 for REPAY matching in repayWithATokens.
    """
    event_topic = pool_event["topics"][0]

    assert event_topic in {
        AaveV3PoolEvent.LIQUIDATION_CALL.value,
        AaveV3PoolEvent.REPAY.value,
        AaveV3PoolEvent.SUPPLY.value,
        AaveV3PoolEvent.WITHDRAW.value,
    }

    return event_topic not in {
        AaveV3PoolEvent.LIQUIDATION_CALL.value,
        AaveV3PoolEvent.REPAY.value,
    }


def _should_consume_debt_mint_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a debt mint's pool event should be consumed.

    Consumption rules:
    - BORROW: Always consumed (single-purpose event)
    - REPAY: Never consumed (shared with collateral burns for repay-with-aTokens)
    - LIQUIDATION_CALL: Never consumed (shared across liquidation operations)

    See debug/aave/0011 for LIQUIDATION_CALL matching in debt mints.
    See debug/aave/0012b for REPAY preservation pattern.
    """
    event_topic = pool_event["topics"][0]

    assert event_topic in {
        AaveV3PoolEvent.BORROW.value,
        AaveV3PoolEvent.REPAY.value,
        AaveV3PoolEvent.LIQUIDATION_CALL.value,
    }

    return event_topic not in {
        AaveV3PoolEvent.REPAY.value,
        AaveV3PoolEvent.LIQUIDATION_CALL.value,
    }


def _should_consume_debt_burn_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a debt burn's pool event should be consumed.

    Consumption rules:
    - REPAY: Consumed only if useATokens=False
      (when useATokens=True, REPAY is shared with collateral burn)
    - LIQUIDATION_CALL: Never consumed (shared across liquidation operations)
    - DEFICIT_CREATED: Never consumed (bad debt write-off may affect multiple positions)

    See debug/aave/0008 for repay-with-aTokens pattern.
    See debug/aave/0010, 0012a for LIQUIDATION_CALL preservation pattern.
    See debug/aave/0013 for DEFICIT_CREATED handling.
    """
    event_topic = pool_event["topics"][0]

    if event_topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
        return False

    if event_topic == AaveV3PoolEvent.DEFICIT_CREATED.value:
        return False

    if event_topic == AaveV3PoolEvent.REPAY.value:
        # REPAY: data=(uint256 amount, bool useATokens)
        _, use_a_tokens = decode(
            types=["uint256", "bool"],
            data=pool_event["data"],
        )
        return not use_a_tokens

    return True


def _should_consume_gho_debt_mint_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a GHO debt mint's pool event should be consumed.

    Consumption rules:
    - BORROW: Always consumed (single-purpose event)
    - REPAY: Never consumed (shared with collateral burns)

    See debug/aave/0012b for REPAY preservation in GHO operations.
    """
    event_topic = pool_event["topics"][0]

    assert event_topic in {
        AaveV3PoolEvent.BORROW.value,
        AaveV3PoolEvent.REPAY.value,
    }

    return event_topic is not AaveV3PoolEvent.REPAY.value


def _should_consume_gho_debt_burn_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a GHO debt burn's pool event should be consumed.

    Consumption rules:
    - REPAY: Consumed (GHO has no useATokens flag, always single-purpose)
    - LIQUIDATION_CALL: Never consumed (shared across liquidation operations)
    - DEFICIT_CREATED: Never consumed (bad debt write-off may affect multiple positions)

    See debug/aave/0010 for LIQUIDATION_CALL preservation in GHO burns.
    See debug/aave/0016 for DEFICIT_CREATED handling in GHO liquidations.
    """
    event_topic = pool_event["topics"][0]

    assert event_topic in {
        AaveV3PoolEvent.BORROW.value,
        AaveV3PoolEvent.LIQUIDATION_CALL.value,
        AaveV3PoolEvent.DEFICIT_CREATED.value,
    }

    return event_topic not in {
        AaveV3PoolEvent.LIQUIDATION_CALL.value,
        AaveV3PoolEvent.DEFICIT_CREATED.value,
    }


def _decode_address(topic: HexBytes) -> ChecksumAddress:
    """Decode a 32-byte topic to a checksummed address."""
    return get_checksum_address("0x" + topic.hex()[-40:])


# ============================================================================
# OPERATION-AWARE EVENT MATCHER
# ============================================================================


class OperationAwareEventMatcher:
    """Event matcher that works within operation context.

    This matcher uses pre-parsed operation context to determine matches,
    eliminating the need for max_log_index and temporal ordering checks.
    """

    def __init__(self, operation: Operation) -> None:
        """Initialize matcher with operation context.

        Args:
            operation: The operation containing the pool event and scaled events.
        """
        self.operation = operation

    def find_match(
        self,
        scaled_event: ScaledTokenEvent,
    ) -> EventMatchResult | None:
        """Find pool event match within operation context.

        Unlike the legacy EventMatcher, this doesn't need max_log_index
        because the operation already groups related events together.

        Args:
            scaled_event: The scaled token event to match.

        Returns:
            EventMatchResult with pool_event, should_consume flag, and extraction_data,
            or None if no match found.
        """
        # Pattern-aware matching based on operation type
        matchers = {
            OperationType.SUPPLY: self._match_supply,
            OperationType.WITHDRAW: self._match_withdraw,
            OperationType.BORROW: self._match_borrow,
            OperationType.GHO_BORROW: self._match_gho_borrow,
            OperationType.REPAY: self._match_repay,
            OperationType.REPAY_WITH_ATOKENS: self._match_repay_with_atokens,
            OperationType.GHO_REPAY: self._match_gho_repay,
            OperationType.LIQUIDATION: self._match_liquidation,
            OperationType.GHO_LIQUIDATION: self._match_gho_liquidation,
            OperationType.GHO_FLASH_LOAN: self._match_flash_loan,
            OperationType.INTEREST_ACCRUAL: self._match_interest_accrual,
            OperationType.BALANCE_TRANSFER: self._match_balance_transfer,
        }

        matcher = matchers.get(
            self.operation.operation_type,
            # Default matching for unknown operation types
            self._default_match,
        )
        return matcher(scaled_event)

    def _match_supply(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match supply operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # SUPPLY is single-purpose
            extraction_data=self._extract_supply_data(),
        )

    def _match_withdraw(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match withdraw operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # WITHDRAW is single-purpose
            extraction_data=self._extract_withdraw_data(),
        )

    def _match_borrow(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match borrow operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # BORROW is single-purpose
            extraction_data=self._extract_borrow_data(),
        )

    def _match_gho_borrow(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match GHO borrow operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # GHO BORROW is single-purpose
            extraction_data=self._extract_borrow_data(),
        )

    def _match_repay(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match repay operation."""
        # Extract useATokens to determine consumption
        extraction_data = self._extract_repay_data()
        use_a_tokens = extraction_data.get("use_a_tokens", False)

        # REPAY is consumed only if useATokens=False
        should_consume = not use_a_tokens

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=should_consume,
            extraction_data=extraction_data,
        )

    def _match_repay_with_atokens(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match repay with aTokens operation.

        In this operation, the REPAY event is shared between:
        - Debt burn (vToken burn)
        - Collateral burn (aToken burn)

        The REPAY event should NOT be consumed.
        """
        extraction_data = self._extract_repay_data()

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,  # Shared across debt and collateral burns
            extraction_data=extraction_data,
        )

    def _match_gho_repay(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match GHO repay operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # GHO REPAY is single-purpose (no useATokens)
            extraction_data=self._extract_repay_data(),
        )

    def _match_liquidation(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match liquidation operation.

        In liquidation, the LIQUIDATION_CALL event is shared between:
        - Debt burn (debt repayment)
        - Collateral burn (collateral seized)

        The LIQUIDATION_CALL event should NOT be consumed.
        """
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,  # Shared across debt and collateral burns
            extraction_data=self._extract_liquidation_data(),
        )

    def _match_gho_liquidation(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match GHO liquidation operation.

        Same as standard liquidation - LIQUIDATION_CALL is shared.
        """
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,  # Shared across burns
            extraction_data=self._extract_liquidation_data(),
        )

    def _match_flash_loan(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match flash loan (DEFICIT_CREATED) operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,  # DEFICIT_CREATED is reusable
            extraction_data=self._extract_deficit_data(),
        )

    def _match_interest_accrual(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match interest accrual operation.

        Interest accrual operations have no pool event. The scaled token event
        represents pure interest accrual where amount == balance_increase.
        """
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,
            extraction_data={},
        )

    def _match_balance_transfer(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """Match balance transfer operation.

        Balance transfer operations have no pool event. The scaled token event
        represents an ERC20 Transfer of aTokens or vTokens between users.
        """
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,
            extraction_data={},
        )

    def _default_match(self, scaled_event: ScaledTokenEvent) -> EventMatchResult | None:
        """Default matching for unknown operation types."""
        if self.operation.pool_event is None:
            return None

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # Default to consumable
            extraction_data={},
        )

    def _extract_supply_data(self) -> dict[str, int]:
        """Extract data from SUPPLY event."""
        # SUPPLY: indexed reserve, indexed onBehalfOf, indexed referralCode
        # data=(address user, uint256 amount)
        if self.operation.pool_event is None:
            return {"raw_amount": 0}
        _, raw_amount = decode(
            types=["address", "uint256"],
            data=self.operation.pool_event["data"],
        )
        return {
            "raw_amount": raw_amount,
        }

    def _extract_withdraw_data(self) -> dict[str, int]:
        """Extract data from WITHDRAW event."""
        # WITHDRAW: data=(uint256 amount)
        if self.operation.pool_event is None:
            return {"raw_amount": 0}
        (raw_amount,) = decode(
            types=["uint256"],
            data=self.operation.pool_event["data"],
        )
        return {
            "raw_amount": raw_amount,
        }

    def _extract_borrow_data(self) -> dict[str, int]:
        """Extract data from BORROW event."""
        # BORROW: data=(address caller, uint256 amount, uint8 interestRateMode, uint256 borrowRate, uint16 referralCode)
        # Skip the first 32 bytes (address caller) and decode the amount
        if self.operation.pool_event is None:
            return {"raw_amount": 0}
        _, raw_amount, _, _ = decode(
            types=["address", "uint256", "uint8", "uint256"],
            data=self.operation.pool_event["data"],
        )
        return {
            "raw_amount": raw_amount,
        }

    def _extract_repay_data(self) -> dict[str, int | bool]:
        """Extract data from REPAY event."""
        # REPAY: data=(uint256 amount, bool useATokens)
        if self.operation.pool_event is None:
            return {"raw_amount": 0, "use_a_tokens": False}
        raw_amount, use_a_tokens = decode(
            types=["uint256", "bool"],
            data=self.operation.pool_event["data"],
        )
        return {
            "raw_amount": raw_amount,
            "use_a_tokens": use_a_tokens,
        }

    def _extract_liquidation_data(self) -> dict[str, int]:
        """Extract data from LIQUIDATION_CALL event."""
        # LIQUIDATION_CALL: data=(uint256 debtToCover, uint256 liquidatedCollateralAmount,
        #                          address liquidator, bool receiveAToken)
        if self.operation.pool_event is None:
            return {"debt_to_cover": 0, "liquidated_collateral": 0}
        debt_to_cover, liquidated_collateral = decode(
            types=["uint256", "uint256"],
            data=self.operation.pool_event["data"],
        )[:2]
        return {
            "debt_to_cover": debt_to_cover,
            "liquidated_collateral": liquidated_collateral,
        }

    def _extract_deficit_data(self) -> dict[str, int]:
        """Extract data from DEFICIT_CREATED event."""
        # DEFICIT_CREATED: data=(uint256 amountCreated)
        if self.operation.pool_event is None:
            return {"amount_created": 0}
        (amount_created,) = decode(
            types=["uint256"],
            data=self.operation.pool_event["data"],
        )
        return {
            "amount_created": amount_created,
        }
