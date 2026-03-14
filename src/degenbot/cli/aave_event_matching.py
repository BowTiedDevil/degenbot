"""
Aave V3 event matching framework.
"""

from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum, auto
from typing import TYPE_CHECKING

from eth_abi.abi import decode
from web3.types import LogReceipt

from degenbot.aave.events import AaveV3PoolEvent, ScaledTokenEventType
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.cli.aave_transaction_operations import Operation, OperationType, ScaledTokenEvent

if TYPE_CHECKING:
    from degenbot.aave.enrichment import ScaledEventEnricher


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


@dataclass(frozen=True)
class EventMatchResult:
    """Result of a successful event match with enriched scaled amounts."""

    pool_event: LogReceipt | None
    should_consume: bool
    enriched_event: EnrichedScaledTokenEvent


class OperationAwareEventMatcher:
    """Event matcher that works within operation context.

    This matcher uses pre-parsed operation context to determine matches,
    eliminating the need for max_log_index and temporal ordering checks.
    """

    def __init__(
        self,
        operation: Operation,
        enricher: "ScaledEventEnricher",
    ) -> None:
        """Initialize matcher with operation context.

        Args:
            operation: The operation containing the pool event and scaled events.
            enricher: The enricher to use for calculating scaled amounts.
        """
        self.operation = operation
        self.enricher = enricher

    def find_match(self, scaled_event: ScaledTokenEvent) -> EventMatchResult:
        """
        Find pool event match within operation context and enrich with scaled amounts.

        Args:
            scaled_event: The scaled token event to match.

        Returns:
            EventMatchResult with pool_event, should_consume flag, and enriched_event
            containing pre-calculated scaled amounts.
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

        # Get match result (pool_event and should_consume)
        pool_event, should_consume = matcher(scaled_event=scaled_event)

        # Enrich the scaled event with calculated amounts
        enriched_event = self.enricher.enrich(
            scaled_event=scaled_event,
            operation=self.operation,
        )

        return EventMatchResult(
            pool_event=pool_event,
            should_consume=should_consume,
            enriched_event=enriched_event,
        )

    def _match_supply(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match supply operation.
        """

        return (self.operation.pool_event, True)

    def _match_withdraw(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match withdraw operation.
        """

        return (self.operation.pool_event, True)

    def _match_borrow(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match borrow operation.
        """

        return (self.operation.pool_event, True)

    def _match_gho_borrow(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match GHO borrow operation.
        """

        return (self.operation.pool_event, True)

    def _match_repay(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match repay operation.
        """

        # Extract useATokens to determine consumption
        extraction_data = self._extract_repay_data()
        use_a_tokens = extraction_data.get("use_a_tokens", False)

        # REPAY is consumed only if useATokens=False
        should_consume = not use_a_tokens

        return (self.operation.pool_event, should_consume)

    def _match_repay_with_atokens(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match repay with aTokens operation.

        In this operation, the REPAY event is shared between:
        - Debt burn (vToken burn)
        - Collateral burn (aToken burn)

        The REPAY event should NOT be consumed.
        """

        return (self.operation.pool_event, False)

    def _match_gho_repay(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match GHO repay operation.
        """

        return (self.operation.pool_event, True)

    def _match_liquidation(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match liquidation operation.

        In liquidation, the LIQUIDATION_CALL event is shared between:
        - Debt burn (debt repayment)
        - Collateral burn (collateral seized)

        The LIQUIDATION_CALL event should NOT be consumed.
        """

        return (self.operation.pool_event, False)

    def _match_gho_liquidation(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match GHO liquidation operation.

        Same as standard liquidation - LIQUIDATION_CALL is shared.
        """

        return (self.operation.pool_event, False)

    def _match_flash_loan(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match flash loan (DEFICIT_CREATED) operation.
        """

        return (self.operation.pool_event, False)

    def _match_interest_accrual(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match interest accrual operation.

        Interest accrual operations have no pool event. The scaled token event
        represents pure interest accrual where amount == balance_increase.
        For debt burns during interest accrual, provide raw_amount to enable
        correct scaled amount calculation using TokenMath.
        """

        return (self.operation.pool_event, False)

    def _match_balance_transfer(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Match balance transfer operation.

        Balance transfer operations have no pool event. The scaled token event
        represents an ERC20 Transfer of aTokens or vTokens between users.
        """

        return (self.operation.pool_event, False)

    def _default_match(
        self,
        scaled_event: ScaledTokenEvent,  # noqa: ARG002
    ) -> tuple[LogReceipt | None, bool]:
        """
        Default matching for unknown operation types.
        """

        return (self.operation.pool_event, True)

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

        assert self.operation.pool_event is not None

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

        assert self.operation.pool_event is not None

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

        assert self.operation.pool_event is not None

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

        assert self.operation.pool_event is not None

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

        assert self.operation.pool_event is not None

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

        assert self.operation.pool_event is not None

        (amount_created,) = decode(
            types=["uint256"],
            data=self.operation.pool_event["data"],
        )
        return {
            "amount_created": amount_created,
        }
