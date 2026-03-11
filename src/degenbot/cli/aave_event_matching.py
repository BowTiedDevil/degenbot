"""
Aave V3 event matching framework.
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
from degenbot.cli.aave_transaction_operations import Operation, OperationType, ScaledTokenEventType
from degenbot.exceptions import DegenbotValueError


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
    """

    CONSUMABLE = auto()
    REUSABLE = auto()
    CONDITIONAL = auto()


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


class EventMatchResult(TypedDict):
    """Result of a successful event match."""

    pool_event: LogReceipt | None
    should_consume: bool
    extraction_data: dict[str, int]


def _should_consume_collateral_mint_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a collateral mint's pool event should be consumed.

    Consumption rules:
    - SUPPLY: Always consumed (single-purpose event)
    - WITHDRAW: Always consumed (single-purpose event)
    - REPAY: Never consumed (shared with debt burns for repay-with-aTokens)
    - LIQUIDATION_CALL: Never consumed (shared across liquidation operations)
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


def _should_consume_collateral_burn_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a collateral burn's pool event should be consumed.

    Consumption rules:
    - WITHDRAW: Always consumed (single-purpose event)
    - REPAY: Consumed only if useATokens=False
      (when useATokens=True, REPAY is shared with debt burn)
    - LIQUIDATION_CALL: Never consumed (shared across operations)
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


def _should_consume_debt_mint_pool_event(pool_event: LogReceipt) -> bool:
    """Determine if a debt mint's pool event should be consumed.

    Consumption rules:
    - BORROW: Always consumed (single-purpose event)
    - REPAY: Never consumed (shared with collateral burns for repay-with-aTokens)
    - LIQUIDATION_CALL: Never consumed (shared across liquidation operations)
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
    """

    event_topic = pool_event["topics"][0]

    assert event_topic in {
        AaveV3PoolEvent.REPAY.value,
        AaveV3PoolEvent.LIQUIDATION_CALL.value,
        AaveV3PoolEvent.DEFICIT_CREATED.value,
    }

    return event_topic not in {
        AaveV3PoolEvent.LIQUIDATION_CALL.value,
        AaveV3PoolEvent.DEFICIT_CREATED.value,
    }


class EventMatcher:
    """Centralized pool event matching for scaled token events.

    This class provides a declarative approach to matching Aave V3 Pool events
    with scaled token events (Mint/Burn), handling complex edge cases like:
    - Shared LIQUIDATION_CALL events in liquidations
    - Conditional REPAY consumption for repay-with-aTokens
    - Debt burns without matching Pool events (flash loans)
    """

    # Single source of truth for all matching configurations
    CONFIGS: ClassVar[dict[ScaledTokenEventType, MatchConfig]] = {
        # Collateral Mint: Can match SUPPLY (deposit), WITHDRAW (interest before withdraw),
        # LIQUIDATION_CALL (liquidator receiving collateral), or REPAY (excess aTokens returned
        # during repayWithATokens)
        # SUPPLY/WITHDRAW: consumed (single-purpose)
        # LIQUIDATION_CALL: never consumed (shared across liquidation operations)
        # REPAY: never consumed (may be shared with debt burns for repay-with-aTokens)
        ScaledTokenEventType.COLLATERAL_MINT: MatchConfig(
            target_event=ScaledTokenEventType.COLLATERAL_MINT,
            pool_event_types=[
                AaveV3PoolEvent.SUPPLY,
                AaveV3PoolEvent.WITHDRAW,
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=_should_consume_collateral_mint_pool_event,
        ),
        # Collateral Burn: Can match WITHDRAW (withdrawal), REPAY (repay with aTokens),
        # or LIQUIDATION_CALL (collateral seized)
        # REPAY only consumed if useATokens=False
        # LIQUIDATION_CALL never consumed
        ScaledTokenEventType.COLLATERAL_BURN: MatchConfig(
            target_event=ScaledTokenEventType.COLLATERAL_BURN,
            pool_event_types=[
                AaveV3PoolEvent.WITHDRAW,
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=_should_consume_collateral_burn_pool_event,
        ),
        # Debt Mint: Can match BORROW (borrow), REPAY (interest before repayment),
        # or LIQUIDATION_CALL (liquidator borrowing to fund liquidation)
        # REPAY and LIQUIDATION_CALL never consumed (shared across operations)
        ScaledTokenEventType.DEBT_MINT: MatchConfig(
            target_event=ScaledTokenEventType.DEBT_MINT,
            pool_event_types=[
                AaveV3PoolEvent.BORROW,
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=_should_consume_debt_mint_pool_event,
        ),
        # Debt Burn: Can match REPAY (repayment), LIQUIDATION_CALL (debt repaid during liquidation),
        # or DEFICIT_CREATED (bad debt write-off)
        # LIQUIDATION_CALL and DEFICIT_CREATED never consumed
        # REPAY only consumed if useATokens=False
        ScaledTokenEventType.DEBT_BURN: MatchConfig(
            target_event=ScaledTokenEventType.DEBT_BURN,
            pool_event_types=[
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
                AaveV3PoolEvent.DEFICIT_CREATED,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=_should_consume_debt_burn_pool_event,
        ),
        # GHO Debt Mint: Can match BORROW or REPAY
        # REPAY never consumed (shared with collateral burns)
        ScaledTokenEventType.GHO_DEBT_MINT: MatchConfig(
            target_event=ScaledTokenEventType.GHO_DEBT_MINT,
            pool_event_types=[AaveV3PoolEvent.BORROW, AaveV3PoolEvent.REPAY],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=_should_consume_gho_debt_mint_pool_event,
        ),
        # GHO Debt Burn: Can match REPAY, LIQUIDATION_CALL, or DEFICIT_CREATED
        # LIQUIDATION_CALL and DEFICIT_CREATED never consumed (shared across operations)
        # DEFICIT_CREATED is used for GHO liquidations (bad debt write-off mechanism)
        ScaledTokenEventType.GHO_DEBT_BURN: MatchConfig(
            target_event=ScaledTokenEventType.GHO_DEBT_BURN,
            pool_event_types=[
                AaveV3PoolEvent.REPAY,
                AaveV3PoolEvent.LIQUIDATION_CALL,
                AaveV3PoolEvent.DEFICIT_CREATED,
            ],
            consumption_policy=EventConsumptionPolicy.CONDITIONAL,
            consumption_condition=_should_consume_gho_debt_burn_pool_event,
        ),
    }

    def __init__(self, tx_context: TransactionContext) -> None:
        """Initialize matcher with transaction context.

        Args:
            tx_context: Transaction context containing pool events and consumption state
        """
        self.tx_context = tx_context
        self._event_index: dict[HexBytes, list[LogReceipt]] | None = None


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

    def find_match(self) -> EventMatchResult | None:
        """
        Find pool event match within operation context.

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
        return matcher()

    def _match_supply(self) -> EventMatchResult:
        """Match supply operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # SUPPLY is single-purpose
            extraction_data=self._extract_supply_data(),
        )

    def _match_withdraw(self) -> EventMatchResult:
        """Match withdraw operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # WITHDRAW is single-purpose
            extraction_data=self._extract_withdraw_data(),
        )

    def _match_borrow(self) -> EventMatchResult:
        """Match borrow operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # BORROW is single-purpose
            extraction_data=self._extract_borrow_data(),
        )

    def _match_gho_borrow(self) -> EventMatchResult:
        """Match GHO borrow operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # GHO BORROW is single-purpose
            extraction_data=self._extract_borrow_data(),
        )

    def _match_repay(self) -> EventMatchResult:
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

    def _match_repay_with_atokens(self) -> EventMatchResult:
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

    def _match_gho_repay(self) -> EventMatchResult:
        """Match GHO repay operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # GHO REPAY is single-purpose (no useATokens)
            extraction_data=self._extract_repay_data(),
        )

    def _match_liquidation(self) -> EventMatchResult:
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

    def _match_gho_liquidation(self) -> EventMatchResult:
        """Match GHO liquidation operation.

        Same as standard liquidation - LIQUIDATION_CALL is shared.
        """
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,  # Shared across burns
            extraction_data=self._extract_liquidation_data(),
        )

    def _match_flash_loan(self) -> EventMatchResult:
        """Match flash loan (DEFICIT_CREATED) operation."""
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,  # DEFICIT_CREATED is reusable
            extraction_data=self._extract_deficit_data(),
        )

    def _match_interest_accrual(self) -> EventMatchResult:
        """Match interest accrual operation.

        Interest accrual operations have no pool event. The scaled token event
        represents pure interest accrual where amount == balance_increase.
        """
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,
            extraction_data={},
        )

    def _match_balance_transfer(self) -> EventMatchResult:
        """Match balance transfer operation.

        Balance transfer operations have no pool event. The scaled token event
        represents an ERC20 Transfer of aTokens or vTokens between users.
        """
        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,
            extraction_data={},
        )

    def _default_match(self) -> EventMatchResult | None:
        """Default matching for unknown operation types."""
        if self.operation.pool_event is None:
            return None

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # Default to consumable
            extraction_data={},
        )

    def _extract_supply_data(self) -> dict[str, int]:
        """
        Extract data from Supply event.

        Event definition:
            event Supply(
                address indexed reserve,
                address user,
                address indexed onBehalfOf,
                uint256 amount,
                uint16 indexed referralCode
            );
        """

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
        """
        Extract data from Withdraw event.

        Event definition:
            event Withdraw(
                address indexed reserve,
                address indexed user,
                address indexed to,
                uint256 amount
            );
        """

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
        """
        Extract data from BORROW event.

        Event definition:
            event Borrow(
                address indexed reserve,
                address user,
                address indexed onBehalfOf,
                uint256 amount,
                DataTypes.InterestRateMode interestRateMode,
                uint256 borrowRate,
                uint16 indexed referralCode
            );
        """

        if self.operation.pool_event is None:
            return {"raw_amount": 0}
        # Skip the first 32 bytes (address caller) and decode the amount
        _, raw_amount, _, _ = decode(
            types=["address", "uint256", "uint8", "uint256"],
            data=self.operation.pool_event["data"],
        )
        return {
            "raw_amount": raw_amount,
        }

    def _extract_repay_data(self) -> dict[str, int | bool]:
        """
        Extract data from Repay event.

        Event definition:
            event Repay(
                address indexed reserve,
                address indexed user,
                address indexed repayer,
                uint256 amount,
                bool useATokens
            );
        """

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
        """
        Extract data from LiquidationCall event.

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
        """
        Extract data from DeficitCreated event.

        Event definition:
            event DeficitCreated(
                address indexed user,
                address indexed debtAsset,
                uint256 amountCreated
            );
        """

        if self.operation.pool_event is None:
            return {"amount_created": 0}
        (amount_created,) = decode(
            types=["uint256"],
            data=self.operation.pool_event["data"],
        )
        return {
            "amount_created": amount_created,
        }
