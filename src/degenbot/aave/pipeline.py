"""
Event processing pipeline for Aave V3.

Deep module that owns the full computation path from raw event to position delta:
extraction → enrichment → processor dispatch → delta.

Callers construct a PositionContext from DB state, call process(), and apply the
returned PositionDelta to DB models. The pipeline has no DB dependency.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

import eth_abi.abi

from degenbot.aave.events import AaveV3PoolEvent, ScaledTokenEventType
from degenbot.aave.libraries.pool_math import PoolMath
from degenbot.aave.libraries.token_math import TokenMathFactory
from degenbot.aave.models import EnrichmentError
from degenbot.aave.operation_types import OperationType
from degenbot.aave.processors import (
    CollateralBurnEvent,
    CollateralMintEvent,
    DebtBurnEvent,
    DebtMintEvent,
    TokenProcessorFactory,
)
from degenbot.aave.types import Operation, ScaledTokenEvent, UserOperation

if TYPE_CHECKING:
    from degenbot.aave.processors.base import (
        GhoScaledTokenBurnResult,
        GhoScaledTokenMintResult,
        ScaledTokenBurnResult,
        ScaledTokenMintResult,
    )


@dataclass(frozen=True)
class PositionContext:
    """
    Input to the pipeline: current position state.

    Constructed from DB models by the CLI layer. No SQLAlchemy dependency.
    """

    previous_balance: int
    previous_index: int
    previous_discount: int = 0
    token_revision: int = 0
    pool_revision: int = 0
    is_gho: bool = False
    is_bad_debt: bool = False


@dataclass(frozen=True)
class PositionDelta:
    """
    Output of the pipeline: computed balance changes.

    The CLI layer applies these to DB position models.
    """

    balance_delta: int
    new_index: int
    user_operation: UserOperation
    discount_scaled: int = 0
    should_refresh_discount: bool = False
    set_balance_to_zero: bool = False


_LIQUIDATION_TOPIC = AaveV3PoolEvent.LIQUIDATION_CALL.value


class EventPipeline:
    """
    Deep pipeline for Aave event processing.

    Owns extraction, enrichment, and processor dispatch.
    Returns PositionDelta for the CLI layer to apply.
    """

    def __init__(
        self,
        pool_revision: int,
        token_revisions: dict[str, int] | None = None,
    ) -> None:
        self.pool_revision = pool_revision
        self.token_revisions: dict[str, int] = token_revisions or {}

    def process(
        self,
        scaled_event: ScaledTokenEvent,
        operation: Operation,
        position: PositionContext,
    ) -> PositionDelta:
        """
        Process a single scaled token event through the full pipeline.

        Steps:
        1. Handle special cases (INTEREST_ACCRUAL, MINT_TO_TREASURY, transfers)
        2. Extract raw amount from pool event
        3. Calculate scaled amount (enrichment)
        4. Dispatch to the correct processor
        5. Return PositionDelta
        """
        event_type = scaled_event.event_type
        op_type = operation.operation_type

        if op_type == OperationType.INTEREST_ACCRUAL:
            return self._process_interest_accrual(scaled_event)

        if op_type == OperationType.MINT_TO_TREASURY:
            return self._process_mint_to_treasury(scaled_event, operation, position)

        if event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
            ScaledTokenEventType.DEBT_TRANSFER,
            ScaledTokenEventType.ERC20_DEBT_TRANSFER,
            ScaledTokenEventType.GHO_DEBT_TRANSFER,
        }:
            return self._process_transfer(scaled_event, position)

        _raw_amount, scaled_amount, calc_type = self._enrich(
            scaled_event=scaled_event,
            operation=operation,
        )

        return self._dispatch(
            scaled_event=scaled_event,
            operation=operation,
            position=position,
            scaled_amount=scaled_amount,
            calculation_event_type=calc_type,
        )

    @staticmethod
    def _process_interest_accrual(
        scaled_event: ScaledTokenEvent,
    ) -> PositionDelta:
        """Interest accrual does not change scaled balance."""
        assert scaled_event.index is not None
        return PositionDelta(
            balance_delta=0,
            new_index=scaled_event.index,
            user_operation=UserOperation.DEPOSIT,
        )

    @staticmethod
    def _process_mint_to_treasury(
        scaled_event: ScaledTokenEvent,
        operation: Operation,
        position: PositionContext,
    ) -> PositionDelta:
        """MINT_TO_TREASURY uses PoolMath for scaled amount calculation."""
        assert operation.minted_to_treasury_amount is not None
        assert scaled_event.index is not None
        assert scaled_event.balance_increase is not None

        scaled_amount = PoolMath.underlying_to_scaled_collateral(
            underlying_amount=operation.minted_to_treasury_amount,
            liquidity_index=scaled_event.index,
            pool_revision=operation.pool_revision,
        )

        event_data = CollateralMintEvent(
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        )

        processor = TokenProcessorFactory.get_collateral_processor(position.token_revision)
        result: ScaledTokenMintResult = processor.process_mint_event(
            event_data=event_data,
            previous_balance=position.previous_balance,
            previous_index=position.previous_index,
            scaled_delta=scaled_amount,
        )

        user_op = UserOperation.WITHDRAW if result.is_repay else UserOperation.DEPOSIT
        return PositionDelta(
            balance_delta=result.balance_delta,
            new_index=result.new_index,
            user_operation=user_op,
        )

    @staticmethod
    def _process_transfer(
        scaled_event: ScaledTokenEvent,
        position: PositionContext,
    ) -> PositionDelta:
        """Transfer events: raw_amount == scaled_amount, no index calculation."""
        raw_amount = scaled_event.amount
        scaled_amount = raw_amount
        is_collateral = scaled_event.event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }

        if is_collateral:
            assert scaled_event.index is not None
            assert scaled_event.balance_increase is not None
            event_data = CollateralMintEvent(
                value=raw_amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            )
            processor = TokenProcessorFactory.get_collateral_processor(
                position.token_revision,
            )
            result = processor.process_mint_event(
                event_data=event_data,
                previous_balance=position.previous_balance,
                previous_index=position.previous_index,
                scaled_delta=scaled_amount,
            )
            user_op = UserOperation.WITHDRAW if result.is_repay else UserOperation.DEPOSIT
            return PositionDelta(
                balance_delta=result.balance_delta,
                new_index=result.new_index,
                user_operation=user_op,
            )

        return PositionDelta(
            balance_delta=scaled_amount,
            new_index=position.previous_index,
            user_operation=UserOperation.BORROW,
        )

    def _enrich(
        self,
        scaled_event: ScaledTokenEvent,
        operation: Operation,
    ) -> tuple[int, int | None, ScaledTokenEventType]:
        """
        Extract raw amount and calculate scaled amount.

        Returns (raw_amount, scaled_amount, calculation_event_type).

        The calculation_event_type may differ from scaled_event.event_type
        when enrichment overrides the rounding mode.
        """
        op_type = operation.operation_type
        event_type = scaled_event.event_type

        if operation.pool_event is None:
            if op_type == OperationType.MINT_TO_TREASURY:
                return scaled_event.amount, None, event_type
            return scaled_event.amount, scaled_event.amount, event_type

        if event_type == ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER:
            return scaled_event.amount, scaled_event.amount, event_type

        raw_amount = self._extract_raw_amount(scaled_event, operation)
        calculation_event_type = event_type

        # Pool rev 9+ liquidation with debt burn
        if (
            op_type in {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}
            and operation.pool_event["topics"][0] == _LIQUIDATION_TOPIC
            and event_type
            in {
                ScaledTokenEventType.DEBT_BURN,
                ScaledTokenEventType.GHO_DEBT_BURN,
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
            }
            and self.pool_revision >= 9  # noqa: PLR2004
        ):
            assert scaled_event.index is not None
            token_revision = self.token_revisions.get(scaled_event.event["address"], 0)
            token_math = TokenMathFactory.get_token_math_for_token_revision(
                token_revision,
            )
            scaled = token_math.get_debt_burn_scaled_amount(raw_amount, scaled_event.index)
            return raw_amount, scaled, ScaledTokenEventType.DEBT_BURN

        # Interest exceeds withdrawal
        if (
            op_type == OperationType.WITHDRAW
            and event_type == ScaledTokenEventType.COLLATERAL_MINT
            and scaled_event.balance_increase is not None
            and scaled_event.amount < scaled_event.balance_increase
        ):
            calculation_event_type = ScaledTokenEventType.COLLATERAL_BURN

        # Interest exceeds repayment
        if (
            op_type in {OperationType.REPAY, OperationType.GHO_REPAY}
            and event_type == ScaledTokenEventType.DEBT_MINT
            and scaled_event.balance_increase is not None
        ):
            calculation_event_type = ScaledTokenEventType.DEBT_BURN

        # REPAY_WITH_ATOKENS interest exceeds repayment
        if (
            op_type == OperationType.REPAY_WITH_ATOKENS
            and event_type == ScaledTokenEventType.COLLATERAL_MINT
            and scaled_event.balance_increase is not None
            and scaled_event.amount < scaled_event.balance_increase
        ):
            calculation_event_type = ScaledTokenEventType.COLLATERAL_BURN

        assert scaled_event.index is not None
        token_revision = self.token_revisions.get(scaled_event.event["address"], 0)
        token_math = TokenMathFactory.get_token_math_for_token_revision(
            token_revision,
        )

        method_map = {
            ScaledTokenEventType.COLLATERAL_MINT: token_math.get_collateral_mint_scaled_amount,
            ScaledTokenEventType.COLLATERAL_BURN: token_math.get_collateral_burn_scaled_amount,
            ScaledTokenEventType.COLLATERAL_TRANSFER: (
                token_math.get_collateral_transfer_scaled_amount
            ),
            ScaledTokenEventType.DEBT_MINT: token_math.get_debt_mint_scaled_amount,
            ScaledTokenEventType.DEBT_BURN: token_math.get_debt_burn_scaled_amount,
            ScaledTokenEventType.GHO_DEBT_MINT: token_math.get_debt_mint_scaled_amount,
            ScaledTokenEventType.GHO_DEBT_BURN: token_math.get_debt_burn_scaled_amount,
        }

        method = method_map.get(calculation_event_type)
        if method is None:
            msg = f"No TokenMath method for event type: {calculation_event_type}"
            raise EnrichmentError(msg)

        scaled_amount = method(raw_amount, scaled_event.index)

        is_repay_debt_mint = op_type in {
            OperationType.REPAY,
            OperationType.GHO_REPAY,
        } and event_type in {
            ScaledTokenEventType.DEBT_MINT,
            ScaledTokenEventType.GHO_DEBT_MINT,
        }

        if calculation_event_type != event_type and not is_repay_debt_mint:
            scaled_amount = None

        return raw_amount, scaled_amount, calculation_event_type

    @staticmethod
    def _extract_raw_amount(
        scaled_event: ScaledTokenEvent,
        operation: Operation,
    ) -> int:
        """
        Extract raw amount from the pool event.

        Absorbs the logic from RawAmountExtractor.
        """
        assert operation.pool_event is not None
        op_type = operation.operation_type
        event_type = scaled_event.event_type
        pool_event = operation.pool_event

        # Liquidation: use specific extractors
        if (
            op_type in {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}
            and pool_event["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value
        ):
            if event_type in {
                ScaledTokenEventType.DEBT_BURN,
                ScaledTokenEventType.GHO_DEBT_BURN,
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
            }:
                debt_to_cover, _, _, _ = eth_abi.abi.decode(
                    types=["uint256", "uint256", "address", "bool"],
                    data=pool_event["data"],
                )
                return debt_to_cover
            if event_type in {
                ScaledTokenEventType.COLLATERAL_BURN,
                ScaledTokenEventType.COLLATERAL_TRANSFER,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
            }:
                _, liquidated_collateral, _, _ = eth_abi.abi.decode(
                    types=["uint256", "uint256", "address", "bool"],
                    data=pool_event["data"],
                )
                return liquidated_collateral
            debt_to_cover, _, _, _ = eth_abi.abi.decode(
                types=["uint256", "uint256", "address", "bool"],
                data=pool_event["data"],
            )
            return debt_to_cover

        # Interest exceeds withdrawal: extract from Withdraw event
        if (
            op_type == OperationType.WITHDRAW
            and event_type == ScaledTokenEventType.COLLATERAL_MINT
            and scaled_event.balance_increase is not None
            and scaled_event.amount < scaled_event.balance_increase
        ):
            (withdraw_amount,) = eth_abi.abi.decode(
                types=["uint256"],
                data=pool_event["data"],
            )
            return withdraw_amount

        # Interest exceeds repayment: extract from Repay event
        if (
            op_type in {OperationType.REPAY, OperationType.GHO_REPAY}
            and event_type == ScaledTokenEventType.DEBT_MINT
            and scaled_event.balance_increase is not None
        ):
            repay_amount, _ = eth_abi.abi.decode(
                types=["uint256", "bool"],
                data=pool_event["data"],
            )
            return repay_amount

        # Standard extraction: decode from pool event based on event topic
        topic = pool_event["topics"][0]

        if topic == AaveV3PoolEvent.SUPPLY.value:
            (_, supply_amount) = eth_abi.abi.decode(
                types=["address", "uint256"],
                data=pool_event["data"],
            )
            return supply_amount

        if topic == AaveV3PoolEvent.WITHDRAW.value:
            (withdraw_amount,) = eth_abi.abi.decode(
                types=["uint256"],
                data=pool_event["data"],
            )
            return withdraw_amount

        if topic == AaveV3PoolEvent.BORROW.value:
            (_, borrow_amount, _, _) = eth_abi.abi.decode(
                types=["address", "uint256", "uint8", "uint256"],
                data=pool_event["data"],
            )
            return borrow_amount

        if topic == AaveV3PoolEvent.REPAY.value:
            (repay_amount, _) = eth_abi.abi.decode(
                types=["uint256", "bool"],
                data=pool_event["data"],
            )
            return repay_amount

        msg = f"No extractor for pool event topic: {topic.hex()}"
        raise EnrichmentError(msg)

    def _dispatch(
        self,
        scaled_event: ScaledTokenEvent,
        operation: Operation,
        position: PositionContext,
        scaled_amount: int | None,
        calculation_event_type: ScaledTokenEventType,
    ) -> PositionDelta:
        """Dispatch to the correct processor and return a PositionDelta."""
        op_type = operation.operation_type
        event_type = scaled_event.event_type

        if position.is_bad_debt and op_type in {
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
        }:
            assert scaled_event.index is not None
            return PositionDelta(
                balance_delta=-position.previous_balance,
                new_index=scaled_event.index,
                user_operation=UserOperation.REPAY,
                set_balance_to_zero=True,
            )

        if position.is_gho:
            return self._process_gho_event(
                scaled_event=scaled_event,
                operation=operation,
                position=position,
                scaled_amount=scaled_amount,
            )

        if event_type in {
            ScaledTokenEventType.COLLATERAL_MINT,
            ScaledTokenEventType.COLLATERAL_BURN,
        }:
            return self._process_collateral_event(
                scaled_event=scaled_event,
                position=position,
                scaled_amount=scaled_amount,
                calculation_event_type=calculation_event_type,
            )

        if event_type in {
            ScaledTokenEventType.DEBT_MINT,
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_MINT,
            ScaledTokenEventType.GHO_DEBT_BURN,
        }:
            return self._process_debt_event(
                scaled_event=scaled_event,
                operation=operation,
                position=position,
                scaled_amount=scaled_amount,
            )

        msg = f"Unhandled event type: {event_type}"
        raise EnrichmentError(msg)

    @staticmethod
    def _process_collateral_event(
        scaled_event: ScaledTokenEvent,
        position: PositionContext,
        scaled_amount: int | None,
        calculation_event_type: ScaledTokenEventType,
    ) -> PositionDelta:
        assert scaled_event.index is not None
        assert scaled_event.balance_increase is not None

        is_effective_burn = (
            calculation_event_type == ScaledTokenEventType.COLLATERAL_BURN
            and scaled_event.event_type == ScaledTokenEventType.COLLATERAL_MINT
        )

        if is_effective_burn or scaled_event.event_type == ScaledTokenEventType.COLLATERAL_BURN:
            event_data = CollateralBurnEvent(
                value=scaled_event.amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            )
            processor = TokenProcessorFactory.get_collateral_processor(
                position.token_revision,
            )
            result: ScaledTokenBurnResult = processor.process_burn_event(
                event_data=event_data,
                previous_balance=position.previous_balance,
                previous_index=position.previous_index,
                scaled_delta=scaled_amount,
            )
            return PositionDelta(
                balance_delta=result.balance_delta,
                new_index=result.new_index,
                user_operation=UserOperation.WITHDRAW,
            )

        event_data = CollateralMintEvent(
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        )
        processor = TokenProcessorFactory.get_collateral_processor(
            position.token_revision,
        )
        result: ScaledTokenMintResult = processor.process_mint_event(
            event_data=event_data,
            previous_balance=position.previous_balance,
            previous_index=position.previous_index,
            scaled_delta=scaled_amount,
        )
        user_op = UserOperation.WITHDRAW if result.is_repay else UserOperation.DEPOSIT
        return PositionDelta(
            balance_delta=result.balance_delta,
            new_index=result.new_index,
            user_operation=user_op,
        )

    @staticmethod
    def _process_debt_event(
        scaled_event: ScaledTokenEvent,
        operation: Operation,
        position: PositionContext,
        scaled_amount: int | None,
    ) -> PositionDelta:
        assert scaled_event.index is not None
        assert scaled_event.balance_increase is not None
        op_type = operation.operation_type

        # REPAY/LIQUIDATION with DEBT_MINT: treat as burn
        if op_type in {
            OperationType.REPAY,
            OperationType.GHO_REPAY,
            OperationType.REPAY_WITH_ATOKENS,
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
        } and scaled_event.event_type in {
            ScaledTokenEventType.DEBT_MINT,
            ScaledTokenEventType.GHO_DEBT_MINT,
        }:
            assert operation.pool_event is not None
            if op_type in {
                OperationType.GHO_REPAY,
                OperationType.REPAY_WITH_ATOKENS,
                OperationType.REPAY,
            }:
                repay_amount, _ = eth_abi.abi.decode(
                    types=["uint256", "bool"],
                    data=operation.pool_event["data"],
                )
            elif op_type in {
                OperationType.GHO_LIQUIDATION,
                OperationType.LIQUIDATION,
            }:
                repay_amount, _, _, _ = eth_abi.abi.decode(
                    types=["uint256", "uint256", "address", "bool"],
                    data=operation.pool_event["data"],
                )
            else:
                msg = f"Unexpected operation type: {op_type}"
                raise EnrichmentError(msg)

            token_math = TokenMathFactory.get_token_math_for_token_revision(
                position.token_revision,
            )
            actual_scaled_burn = token_math.get_debt_burn_scaled_amount(
                repay_amount, scaled_event.index
            )

            event_data = DebtBurnEvent(
                from_=scaled_event.user_address,
                target=scaled_event.user_address,
                value=actual_scaled_burn,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=actual_scaled_burn,
            )
            processor = TokenProcessorFactory.get_debt_processor(
                position.token_revision,
            )
            result: ScaledTokenBurnResult = processor.process_burn_event(
                event_data=event_data,
                previous_balance=position.previous_balance,
                previous_index=position.previous_index,
                scaled_delta=actual_scaled_burn,
            )
            return PositionDelta(
                balance_delta=result.balance_delta,
                new_index=result.new_index,
                user_operation=UserOperation.REPAY,
            )

        # Debt burn
        if scaled_event.event_type in {
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_BURN,
        }:
            event_data = DebtBurnEvent(
                from_=scaled_event.from_address or scaled_event.user_address,
                target=scaled_event.target_address or scaled_event.user_address,
                value=scaled_event.amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            )
            processor = TokenProcessorFactory.get_debt_processor(
                position.token_revision,
            )
            result = processor.process_burn_event(
                event_data=event_data,
                previous_balance=position.previous_balance,
                previous_index=position.previous_index,
                scaled_delta=scaled_amount,
            )
            return PositionDelta(
                balance_delta=result.balance_delta,
                new_index=result.new_index,
                user_operation=UserOperation.REPAY,
            )

        # Debt mint (borrow)
        event_data = DebtMintEvent(
            caller=scaled_event.caller_address or scaled_event.user_address,
            on_behalf_of=scaled_event.user_address,
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        )
        processor = TokenProcessorFactory.get_debt_processor(position.token_revision)
        result = processor.process_mint_event(
            event_data=event_data,
            previous_balance=position.previous_balance,
            previous_index=position.previous_index,
            scaled_delta=scaled_amount,
        )
        user_op = UserOperation.REPAY if result.is_repay else UserOperation.BORROW
        return PositionDelta(
            balance_delta=result.balance_delta,
            new_index=result.new_index,
            user_operation=user_op,
        )

    @staticmethod
    def _process_gho_event(
        scaled_event: ScaledTokenEvent,
        operation: Operation,
        position: PositionContext,
        scaled_amount: int | None,
    ) -> PositionDelta:
        assert scaled_event.index is not None
        assert scaled_event.balance_increase is not None
        op_type = operation.operation_type

        gho_processor = TokenProcessorFactory.get_gho_debt_processor(
            position.token_revision,
        )

        # GHO debt burn
        if scaled_event.event_type == ScaledTokenEventType.GHO_DEBT_BURN:
            event_data = DebtBurnEvent(
                from_=scaled_event.from_address or scaled_event.user_address,
                target=scaled_event.target_address or scaled_event.user_address,
                value=scaled_event.amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            )
            result: GhoScaledTokenBurnResult = gho_processor.process_burn_event(
                event_data=event_data,
                previous_balance=position.previous_balance,
                previous_index=position.previous_index,
                previous_discount=position.previous_discount,
            )
            return PositionDelta(
                balance_delta=result.balance_delta,
                new_index=result.new_index,
                user_operation=UserOperation.REPAY,
                discount_scaled=result.discount_scaled,
                should_refresh_discount=result.should_refresh_discount,
            )

        # GHO debt mint
        actual_repay_amount: int | None = None
        if op_type == OperationType.GHO_REPAY:
            assert operation.pool_event is not None
            repay_amount_data, _ = eth_abi.abi.decode(
                types=["uint256", "bool"],
                data=operation.pool_event["data"],
            )
            actual_repay_amount = repay_amount_data

        event_data = DebtMintEvent(
            caller=scaled_event.caller_address or scaled_event.user_address,
            on_behalf_of=scaled_event.user_address,
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        )
        result: GhoScaledTokenMintResult = gho_processor.process_mint_event(
            event_data=event_data,
            previous_balance=position.previous_balance,
            previous_index=position.previous_index,
            previous_discount=position.previous_discount,
            actual_repay_amount=actual_repay_amount,
        )
        user_op = UserOperation.REPAY if result.is_repay else UserOperation.BORROW
        return PositionDelta(
            balance_delta=result.balance_delta,
            new_index=result.new_index,
            user_operation=user_op,
            discount_scaled=result.discount_scaled,
            should_refresh_discount=result.should_refresh_discount,
        )
