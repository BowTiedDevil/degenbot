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

import eth_abi.abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
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


class AaveV3Event(Enum):
    """Aave V3 Pool event types."""

    SUPPLY = HexBytes("0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61")
    WITHDRAW = HexBytes("0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7")
    BORROW = HexBytes("0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0")
    REPAY = HexBytes("0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051")
    LIQUIDATION_CALL = HexBytes(
        "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"
    )
    DEFICIT_CREATED = HexBytes("0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699")


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
    pool_event_types: list[AaveV3Event] = field(default_factory=list)
    consumption_policy: EventConsumptionPolicy = EventConsumptionPolicy.CONSUMABLE
    consumption_condition: Callable[[LogReceipt], bool] | None = None
    match_order_priority: bool = True


class EventMatchResult(TypedDict):
    """Result of a successful event match."""

    pool_event: LogReceipt
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
                AaveV3Event.SUPPLY,
                AaveV3Event.WITHDRAW,
                AaveV3Event.REPAY,
                AaveV3Event.LIQUIDATION_CALL,
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
                AaveV3Event.WITHDRAW,
                AaveV3Event.REPAY,
                AaveV3Event.LIQUIDATION_CALL,
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
                AaveV3Event.BORROW,
                AaveV3Event.REPAY,
                AaveV3Event.LIQUIDATION_CALL,
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
                AaveV3Event.REPAY,
                AaveV3Event.LIQUIDATION_CALL,
                AaveV3Event.DEFICIT_CREATED,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=lambda e: _should_consume_debt_burn_pool_event(e),
        ),
        # GHO Debt Mint: Can match BORROW or REPAY
        # REPAY never consumed (shared with collateral burns)
        # See debug/aave/0012b
        ScaledTokenEventType.GHO_DEBT_MINT: MatchConfig(
            target_event=ScaledTokenEventType.GHO_DEBT_MINT,
            pool_event_types=[AaveV3Event.BORROW, AaveV3Event.REPAY],
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
                AaveV3Event.REPAY,
                AaveV3Event.LIQUIDATION_CALL,
                AaveV3Event.DEFICIT_CREATED,
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
        elif config.consumption_policy == EventConsumptionPolicy.REUSABLE:
            return False
        elif config.consumption_policy == EventConsumptionPolicy.CONDITIONAL:
            if config.consumption_condition is not None:
                return config.consumption_condition(pool_event)
            return True
        return True

    @staticmethod
    def _matches_pool_event(
        pool_event: LogReceipt,
        expected_type: AaveV3Event,
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

        # Allow LIQUIDATION_CALL when expecting REPAY or WITHDRAW
        # Allow DEFICIT_CREATED when expecting REPAY (bad debt write-off)
        if (
            event_topic != expected_type.value
            and not (
                event_topic == AaveV3Event.LIQUIDATION_CALL.value
                and expected_type in {AaveV3Event.REPAY, AaveV3Event.WITHDRAW}
            )
            and not (
                event_topic == AaveV3Event.DEFICIT_CREATED.value
                and expected_type == AaveV3Event.REPAY
            )
        ):
            return False

        if expected_type == AaveV3Event.BORROW:
            # BORROW: topics[1]=reserve, topics[2]=onBehalfOf, data=(amount, interestRateMode)
            event_reserve = _decode_address(pool_event["topics"][1])
            event_on_behalf_of = _decode_address(pool_event["topics"][2])
            (_, _, interest_rate_mode, _) = eth_abi.abi.decode(
                types=["address", "uint256", "uint8", "uint256"],
                data=pool_event["data"],
            )
            return (
                event_on_behalf_of == user_address
                and event_reserve == reserve_address
                and interest_rate_mode == 2  # Variable rate
            )

        elif expected_type == AaveV3Event.REPAY:
            if event_topic == AaveV3Event.REPAY.value:
                # REPAY: topics[1]=reserve, topics[2]=user
                event_reserve = _decode_address(pool_event["topics"][1])
                event_user = _decode_address(pool_event["topics"][2])
                return event_user == user_address and event_reserve == reserve_address
            elif event_topic == AaveV3Event.LIQUIDATION_CALL.value:
                # Liquidation matching - match on debtAsset
                event_debt_asset = _decode_address(pool_event["topics"][2])
                event_user = _decode_address(pool_event["topics"][3])
                return event_user == user_address and event_debt_asset == reserve_address
            elif event_topic == AaveV3Event.DEFICIT_CREATED.value:
                # DeficitCreated matching - debt written off
                event_user = _decode_address(pool_event["topics"][1])
                event_reserve = _decode_address(pool_event["topics"][2])
                return event_user == user_address and event_reserve == reserve_address

        elif expected_type == AaveV3Event.SUPPLY:
            # SUPPLY: topics[1]=reserve, topics[2]=onBehalfOf, topics[3]=referralCode
            # Aave V3 Supply event format (4 topics):
            #   Supply(address indexed reserve, address indexed onBehalfOf,
            #          uint16 indexed referralCode, address caller, uint256 amount)
            # topics[3] is referralCode (uint16), NOT an address - do not decode as address!
            event_reserve = _decode_address(pool_event["topics"][1])
            event_on_behalf_of = _decode_address(pool_event["topics"][2])
            return event_on_behalf_of == user_address and event_reserve == reserve_address

        elif expected_type == AaveV3Event.WITHDRAW:
            if event_topic == AaveV3Event.WITHDRAW.value:
                # WITHDRAW: topics[1]=reserve, topics[2]=user
                event_reserve = _decode_address(pool_event["topics"][1])
                event_user = _decode_address(pool_event["topics"][2])
                return event_user == user_address and event_reserve == reserve_address
            elif event_topic == AaveV3Event.LIQUIDATION_CALL.value:
                # Liquidation matching - match on collateralAsset
                event_collateral_asset = _decode_address(pool_event["topics"][1])
                event_user = _decode_address(pool_event["topics"][3])
                return event_user == user_address and event_collateral_asset == reserve_address

        elif expected_type == AaveV3Event.LIQUIDATION_CALL:
            # LIQUIDATION_CALL: topics[1]=collateralAsset, topics[2]=debtAsset, topics[3]=user
            event_collateral_asset = _decode_address(pool_event["topics"][1])
            event_debt_asset = _decode_address(pool_event["topics"][2])
            event_user = _decode_address(pool_event["topics"][3])
            if event_user == user_address:
                return reserve_address in {event_debt_asset, event_collateral_asset}

        elif expected_type == AaveV3Event.DEFICIT_CREATED:
            # DEFICIT_CREATED: topics[1]=user, topics[2]=asset, data=(uint256 amountCreated)
            # Used for GHO liquidations and bad debt write-offs
            event_user = _decode_address(pool_event["topics"][1])
            event_asset = _decode_address(pool_event["topics"][2])
            return event_user == user_address and event_asset == reserve_address

        return False

    def find_matching_pool_event(
        self,
        event_type: ScaledTokenEventType,
        user_address: ChecksumAddress,
        reserve_address: ChecksumAddress,
        *,
        check_users: list[ChecksumAddress] | None = None,
        max_log_index: int | None = None,
    ) -> EventMatchResult | None:
        """Find a matching pool event for a scaled token event.

        This is the main entry point for event matching. It searches through
        available pool events in the transaction context, trying each configured
        event type in order until a match is found.

        Args:
            event_type: Type of scaled token event being matched
            user_address: Primary user address to match
            reserve_address: Reserve/token address to match
            check_users: Optional additional user addresses to try (e.g., caller_address)
            max_log_index: Optional maximum logIndex for pool events. Pool events with
                logIndex > max_log_index will be skipped. This prevents matching a
                scaled token event to a pool event that occurs later in the transaction.

        Returns:
            EventMatchResult with pool_event, should_consume flag, and extraction_data,
            or None if no match found

        Raises:
            EventMatchError: If matching fails and strict mode is enabled
        """
        config = self.CONFIGS.get(event_type)
        if config is None:
            raise EventMatchError(
                f"Unknown scaled token event type: {event_type}",
                user_address=user_address,
                reserve_address=reserve_address,
            )

        # Build list of user addresses to check
        users_to_check = [user_address]
        if check_users:
            users_to_check.extend(check_users)

        # Try each pool event type in order
        for expected_type in config.pool_event_types:
            for user in users_to_check:
                # Use index for O(1) lookup if available, otherwise iterate
                event_index = self._build_event_index()
                events_of_type = event_index.get(expected_type.value, [])

                for pool_event in events_of_type:
                    # Skip events that occur after the scaled token event
                    if max_log_index is not None and pool_event["logIndex"] > max_log_index:
                        continue

                    if self._is_consumed(pool_event):
                        logger.debug(
                            f"Skipping consumed event at logIndex {pool_event['logIndex']}"
                        )
                        continue

                    if self._is_consumed(pool_event):
                        print(
                            f"DEBUG: Skipping - event already consumed at logIndex {pool_event['logIndex']}"
                        )
                        continue

                    matches = self._matches_pool_event(
                        pool_event, expected_type, user, reserve_address
                    )
                    if matches:
                        logger.debug(
                            f"Matched {expected_type.name} event at logIndex {pool_event['logIndex']} "
                            f"for user {user}"
                        )
                        should_consume = self._should_consume(pool_event, config)
                        if should_consume:
                            self._mark_consumed(pool_event)

                        extraction_data = self._extract_event_data(pool_event, event_type)

                        return EventMatchResult(
                            pool_event=pool_event,
                            should_consume=should_consume,
                            extraction_data=extraction_data,
                        )

        # No match found
        logger.debug(
            f"No matching event found for {event_type.name}. "
            f"Tried users: {users_to_check}, reserve: {reserve_address}. "
            f"Available pool events: {len(self.tx_context.pool_events)}"
        )
        return None

    def _extract_event_data(
        self, pool_event: LogReceipt, event_type: ScaledTokenEventType
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

        if event_topic == AaveV3Event.SUPPLY.value:
            # SUPPLY: data=(address caller, uint256 amount)
            (_, raw_amount) = eth_abi.abi.decode(
                types=["address", "uint256"],
                data=pool_event["data"],
            )
            result["raw_amount"] = raw_amount

        elif event_topic == AaveV3Event.WITHDRAW.value:
            # WITHDRAW: data=(uint256 amount)
            (raw_amount,) = eth_abi.abi.decode(
                types=["uint256"],
                data=pool_event["data"],
            )
            result["raw_amount"] = raw_amount

        elif event_topic == AaveV3Event.BORROW.value:
            # BORROW: data=(address caller, uint256 amount, uint8 interestRateMode, uint256 borrowRate)
            (_, raw_amount, _, _) = eth_abi.abi.decode(
                types=["address", "uint256", "uint8", "uint256"],
                data=pool_event["data"],
            )
            result["raw_amount"] = raw_amount

        elif event_topic == AaveV3Event.REPAY.value:
            # REPAY: data=(uint256 amount, bool useATokens)
            raw_amount, use_a_tokens = eth_abi.abi.decode(
                types=["uint256", "bool"],
                data=pool_event["data"],
            )
            result["raw_amount"] = raw_amount
            result["use_a_tokens"] = int(use_a_tokens)  # Store as int for TypedDict

        elif event_topic == AaveV3Event.LIQUIDATION_CALL.value:
            # LIQUIDATION_CALL: data=(uint256 debtToCover, uint256 liquidatedCollateralAmount,
            #                          address liquidator, bool receiveAToken)
            debt_to_cover, liquidated_collateral, _, _ = eth_abi.abi.decode(
                types=["uint256", "uint256", "address", "bool"],
                data=pool_event["data"],
            )
            result["debt_to_cover"] = debt_to_cover
            result["liquidated_collateral"] = liquidated_collateral

        elif event_topic == AaveV3Event.DEFICIT_CREATED.value:
            # DEFICIT_CREATED: data=(uint256 amountCreated)
            (amount_created,) = eth_abi.abi.decode(
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

    if event_topic == AaveV3Event.LIQUIDATION_CALL.value:
        return False

    if event_topic == AaveV3Event.REPAY.value:
        # REPAY: data=(uint256 amount, bool useATokens)
        _, use_a_tokens = eth_abi.abi.decode(
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

    # LIQUIDATION_CALL and REPAY are never consumed because they must be available
    # to match multiple operations (liquidations or repay-with-aTokens transactions)
    if event_topic in {
        AaveV3Event.LIQUIDATION_CALL.value,
        AaveV3Event.REPAY.value,
    }:
        return False

    # SUPPLY and WITHDRAW are consumable
    return True


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

    # REPAY and LIQUIDATION_CALL are never consumed by debt mints
    # because they need to be available for other operations
    if event_topic in {
        AaveV3Event.REPAY.value,
        AaveV3Event.LIQUIDATION_CALL.value,
    }:
        return False

    return True


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

    if event_topic == AaveV3Event.LIQUIDATION_CALL.value:
        return False

    if event_topic == AaveV3Event.DEFICIT_CREATED.value:
        return False

    if event_topic == AaveV3Event.REPAY.value:
        # REPAY: data=(uint256 amount, bool useATokens)
        _, use_a_tokens = eth_abi.abi.decode(
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

    # REPAY is never consumed by GHO debt mints
    if event_topic == AaveV3Event.REPAY.value:
        return False

    return True


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

    if event_topic in {
        AaveV3Event.LIQUIDATION_CALL.value,
        AaveV3Event.DEFICIT_CREATED.value,
    }:
        return False

    return True


def _decode_address(topic: HexBytes) -> ChecksumAddress:
    """Decode a 32-byte topic to a checksummed address."""
    return get_checksum_address("0x" + topic.hex()[-40:])
