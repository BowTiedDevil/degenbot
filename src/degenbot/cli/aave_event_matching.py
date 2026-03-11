"""
Aave V3 event matching framework.
"""

from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum, auto
from typing import Protocol, TypedDict

from eth_abi.abi import decode
from web3.types import LogReceipt

from degenbot.aave.events import AaveV3PoolEvent
from degenbot.cli.aave_transaction_operations import Operation, OperationType, ScaledTokenEventType


class TransactionContext(Protocol):
    """
    Protocol for transaction context.

    Defines the interface needed by EventMatcher without importing
    the actual TransactionContext class from aave.py (avoiding circular imports).
    """

    pool_events: list[LogReceipt]
    matched_pool_events: dict[int, bool]


class EventConsumptionPolicy(Enum):
    """
    Policy for consuming pool events after matching.

    - CONSUMABLE: Mark event as consumed after first match (e.g., SUPPLY, WITHDRAW)
    - REUSABLE: Never mark as consumed (e.g., LIQUIDATION_CALL, DEFICIT_CREATED)
    - CONDITIONAL: Consumed based on event data (e.g., REPAY with useATokens flag)
    """

    CONSUMABLE = auto()
    REUSABLE = auto()
    CONDITIONAL = auto()


@dataclass(frozen=True)
class MatchConfig:
    """
    Configuration for matching a scaled token event to pool events.

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
        """
        Match supply operation.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # SUPPLY is single-purpose
            extraction_data=self._extract_supply_data(),
        )

    def _match_withdraw(self) -> EventMatchResult:
        """
        Match withdraw operation.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # WITHDRAW is single-purpose
            extraction_data=self._extract_withdraw_data(),
        )

    def _match_borrow(self) -> EventMatchResult:
        """
        Match borrow operation.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # BORROW is single-purpose
            extraction_data=self._extract_borrow_data(),
        )

    def _match_gho_borrow(self) -> EventMatchResult:
        """
        Match GHO borrow operation.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # GHO BORROW is single-purpose
            extraction_data=self._extract_borrow_data(),
        )

    def _match_repay(self) -> EventMatchResult:
        """
        Match repay operation.
        """

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
        """
        Match repay with aTokens operation.

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
        """
        Match GHO repay operation.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=True,  # GHO REPAY is single-purpose (no useATokens)
            extraction_data=self._extract_repay_data(),
        )

    def _match_liquidation(self) -> EventMatchResult:
        """
        Match liquidation operation.

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
        """
        Match GHO liquidation operation.

        Same as standard liquidation - LIQUIDATION_CALL is shared.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,  # Shared across burns
            extraction_data=self._extract_liquidation_data(),
        )

    def _match_flash_loan(self) -> EventMatchResult:
        """
        Match flash loan (DEFICIT_CREATED) operation.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,  # DEFICIT_CREATED is reusable
            extraction_data=self._extract_deficit_data(),
        )

    def _match_interest_accrual(self) -> EventMatchResult:
        """
        Match interest accrual operation.

        Interest accrual operations have no pool event. The scaled token event
        represents pure interest accrual where amount == balance_increase.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,
            extraction_data={},
        )

    def _match_balance_transfer(self) -> EventMatchResult:
        """
        Match balance transfer operation.

        Balance transfer operations have no pool event. The scaled token event
        represents an ERC20 Transfer of aTokens or vTokens between users.
        """

        return EventMatchResult(
            pool_event=self.operation.pool_event,
            should_consume=False,
            extraction_data={},
        )

    def _default_match(self) -> EventMatchResult | None:
        """
        Default matching for unknown operation types.
        """

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

        (amount_created,) = decode(
            types=["uint256"],
            data=self.operation.pool_event["data"],
        )
        return {
            "amount_created": amount_created,
        }
