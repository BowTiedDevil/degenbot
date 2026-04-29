"""
Tests for Aave V3 event processing pipeline.

Uses Fake test doubles to construct minimal ScaledTokenEvent and Operation
instances without requiring on-chain data or DB state.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import eth_abi.abi
import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.aave.events import AaveV3PoolEvent, ScaledTokenEventType
from degenbot.aave.libraries.wad_ray_math import RAY
from degenbot.aave.operation_types import OperationType
from degenbot.aave.pipeline import EventPipeline, PositionContext, PositionDelta
from degenbot.aave.types import Operation, ScaledTokenEvent, UserOperation

if TYPE_CHECKING:
    from web3.types import LogReceipt

USER_ADDRESS: ChecksumAddress = ChecksumAddress("0x" + "11" * 20)
TOKEN_ADDRESS: ChecksumAddress = ChecksumAddress("0x" + "22" * 20)
CALLER_ADDRESS: ChecksumAddress = ChecksumAddress("0x" + "33" * 20)
FROM_ADDRESS: ChecksumAddress = ChecksumAddress("0x" + "44" * 20)
TARGET_ADDRESS: ChecksumAddress = ChecksumAddress("0x" + "55" * 20)

DEFAULT_INDEX = RAY


def _make_log_receipt(
    address: str = TOKEN_ADDRESS,
    topics: list[bytes] | None = None,
    data: bytes = b"",
    log_index: int = 0,
    block_number: int = 1,
    transaction_hash: bytes = b"\x00" * 32,
) -> LogReceipt:
    """Fake LogReceipt for testing."""
    if topics is None:
        topics = [b"\x00" * 32]
    return {
        "address": address,
        "topics": [HexBytes(t) for t in topics],
        "data": HexBytes(data),
        "logIndex": log_index,
        "blockNumber": block_number,
        "transactionHash": HexBytes(transaction_hash),
        "transactionIndex": 0,
        "blockHash": HexBytes(b"\x00" * 32),
        "removed": False,
    }


def _make_scaled_event(
    event_type: ScaledTokenEventType,
    amount: int = 1000,
    balance_increase: int | None = None,
    index: int | None = DEFAULT_INDEX,
    user_address: ChecksumAddress = USER_ADDRESS,
    caller_address: ChecksumAddress | None = None,
    from_address: ChecksumAddress | None = None,
    target_address: ChecksumAddress | None = None,
    event_address: str = TOKEN_ADDRESS,
) -> ScaledTokenEvent:
    """Fake ScaledTokenEvent for testing."""
    return ScaledTokenEvent(
        event=_make_log_receipt(address=event_address),
        event_type=event_type,
        user_address=user_address,
        caller_address=caller_address,
        from_address=from_address,
        target_address=target_address,
        amount=amount,
        balance_increase=balance_increase,
        index=index,
    )


def _make_operation(
    operation_type: OperationType,
    pool_revision: int = 4,
    pool_event: LogReceipt | None = None,
    minted_to_treasury_amount: int | None = None,
) -> Operation:
    """Fake Operation for testing."""
    return Operation(
        operation_id=1,
        operation_type=operation_type,
        pool_revision=pool_revision,
        pool_event=pool_event,
        scaled_token_events=[],
        transfer_events=[],
        balance_transfer_events=[],
        minted_to_treasury_amount=minted_to_treasury_amount,
    )


def _make_supply_pool_event(amount: int) -> LogReceipt:
    """Fake Supply pool event with ABI-encoded (address, uint256)."""
    data = eth_abi.abi.encode(["address", "uint256"], [USER_ADDRESS, amount])
    return _make_log_receipt(
        topics=[AaveV3PoolEvent.SUPPLY.value],
        data=data,
    )


def _make_borrow_pool_event(amount: int) -> LogReceipt:
    """Fake Borrow pool event with ABI-encoded (address, uint256, uint8, uint256)."""
    data = eth_abi.abi.encode(
        ["address", "uint256", "uint8", "uint256"],
        [USER_ADDRESS, amount, 2, 0],
    )
    return _make_log_receipt(
        topics=[AaveV3PoolEvent.BORROW.value],
        data=data,
    )


def _make_repay_pool_event(amount: int) -> LogReceipt:
    """Fake Repay pool event with ABI-encoded (uint256, bool)."""
    data = eth_abi.abi.encode(["uint256", "bool"], [amount, True])
    return _make_log_receipt(
        topics=[AaveV3PoolEvent.REPAY.value],
        data=data,
    )


def _make_withdraw_pool_event(amount: int) -> LogReceipt:
    """Fake Withdraw pool event with ABI-encoded (uint256)."""
    data = eth_abi.abi.encode(["uint256"], [amount])
    return _make_log_receipt(
        topics=[AaveV3PoolEvent.WITHDRAW.value],
        data=data,
    )


class TestInterestAccrual:
    """INTEREST_ACCRUAL operations produce zero balance delta."""

    def test_collateral_mint_interest_accrual(self) -> None:
        """Interest accrual Mint event: no balance change, index updated."""
        pipeline = EventPipeline(pool_revision=4)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            amount=500,
            balance_increase=500,
            index=DEFAULT_INDEX + 100,
        )
        operation = _make_operation(OperationType.INTEREST_ACCRUAL)
        position = PositionContext(
            previous_balance=10000,
            previous_index=DEFAULT_INDEX,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta == 0
        assert result.new_index == DEFAULT_INDEX + 100
        assert result.user_operation == UserOperation.DEPOSIT

    def test_debt_mint_interest_accrual(self) -> None:
        """Interest accrual on debt Mint: same zero-delta behavior."""
        pipeline = EventPipeline(pool_revision=4)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            amount=300,
            balance_increase=300,
            index=DEFAULT_INDEX + 50,
        )
        operation = _make_operation(OperationType.INTEREST_ACCRUAL)
        position = PositionContext(
            previous_balance=5000,
            previous_index=DEFAULT_INDEX,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta == 0
        assert result.new_index == DEFAULT_INDEX + 50


class TestCollateralMint:
    """SUPPLY → COLLATERAL_MINT: balance increases, user operation is DEPOSIT."""

    def test_supply_collateral_mint(self) -> None:
        """Standard supply: collateral balance increases."""
        supply_amount = 10**18
        pipeline = EventPipeline(pool_revision=4)
        pool_event = _make_supply_pool_event(supply_amount)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            amount=supply_amount,
            balance_increase=0,
            index=DEFAULT_INDEX,
        )
        operation = _make_operation(
            OperationType.SUPPLY,
            pool_event=pool_event,
        )
        position = PositionContext(
            previous_balance=0,
            previous_index=DEFAULT_INDEX,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta > 0
        assert result.new_index == DEFAULT_INDEX
        assert result.user_operation == UserOperation.DEPOSIT

    def test_supply_collateral_mint_with_existing_balance(self) -> None:
        """Supply when position already has balance."""
        supply_amount = 10**18
        pipeline = EventPipeline(pool_revision=4)
        pool_event = _make_supply_pool_event(supply_amount)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            amount=supply_amount,
            balance_increase=100,
            index=DEFAULT_INDEX + 10,
        )
        operation = _make_operation(
            OperationType.SUPPLY,
            pool_event=pool_event,
        )
        position = PositionContext(
            previous_balance=5000,
            previous_index=DEFAULT_INDEX,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta > 0
        assert result.new_index == DEFAULT_INDEX + 10


class TestDebtMint:
    """BORROW → DEBT_MINT: balance increases, user operation is BORROW."""

    def test_borrow_debt_mint(self) -> None:
        """Standard borrow: debt balance increases."""
        borrow_amount = 10**18
        pipeline = EventPipeline(pool_revision=4)
        pool_event = _make_borrow_pool_event(borrow_amount)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            amount=borrow_amount,
            balance_increase=0,
            index=DEFAULT_INDEX,
        )
        operation = _make_operation(
            OperationType.BORROW,
            pool_event=pool_event,
        )
        position = PositionContext(
            previous_balance=0,
            previous_index=DEFAULT_INDEX,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta > 0
        assert result.user_operation == UserOperation.BORROW


class TestCollateralBurn:
    """WITHDRAW → COLLATERAL_BURN: balance decreases, user operation is WITHDRAW."""

    def test_withdraw_collateral_burn(self) -> None:
        """Standard withdrawal: collateral balance decreases."""
        withdraw_amount = 10**18
        pipeline = EventPipeline(pool_revision=4)
        pool_event = _make_withdraw_pool_event(withdraw_amount)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            amount=withdraw_amount,
            balance_increase=0,
            index=DEFAULT_INDEX,
        )
        operation = _make_operation(
            OperationType.WITHDRAW,
            pool_event=pool_event,
        )
        position = PositionContext(
            previous_balance=5000,
            previous_index=DEFAULT_INDEX,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta < 0
        assert result.user_operation == UserOperation.WITHDRAW


class TestDebtBurn:
    """REPAY → DEBT_BURN: balance decreases, user operation is REPAY."""

    def test_repay_debt_burn(self) -> None:
        """Standard repay: debt balance decreases."""
        repay_amount = 10**18
        pipeline = EventPipeline(pool_revision=4)
        pool_event = _make_repay_pool_event(repay_amount)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            amount=repay_amount,
            balance_increase=0,
            index=DEFAULT_INDEX,
        )
        operation = _make_operation(
            OperationType.REPAY,
            pool_event=pool_event,
        )
        position = PositionContext(
            previous_balance=5000,
            previous_index=DEFAULT_INDEX,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta < 0
        assert result.user_operation == UserOperation.REPAY


class TestBadDebtLiquidation:
    """Bad debt liquidation zeroes out the position balance."""

    def test_bad_debt_liquidation_zeros_balance(self) -> None:
        """Liquidation on bad debt sets balance to zero."""
        debt_amount = 1000
        collateral_amount = 500
        liquidation_data = eth_abi.abi.encode(
            ["uint256", "uint256", "address", "bool"],
            [debt_amount, collateral_amount, TOKEN_ADDRESS, False],
        )
        pipeline = EventPipeline(pool_revision=4)
        pool_event = _make_log_receipt(
            topics=[AaveV3PoolEvent.LIQUIDATION_CALL.value],
            data=liquidation_data,
        )
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            amount=debt_amount,
            balance_increase=0,
            index=DEFAULT_INDEX + 50,
        )
        operation = _make_operation(
            OperationType.LIQUIDATION,
            pool_event=pool_event,
        )
        position = PositionContext(
            previous_balance=5000,
            previous_index=DEFAULT_INDEX,
            is_bad_debt=True,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.set_balance_to_zero is True
        assert result.balance_delta == -5000
        assert result.new_index == DEFAULT_INDEX + 50
        assert result.user_operation == UserOperation.REPAY

    def test_non_bad_debt_liquidation_no_zero_flag(self) -> None:
        """Regular liquidation does not set balance_to_zero flag."""
        debt_amount = 10**18
        pipeline = EventPipeline(pool_revision=4)
        collateral_amount = debt_amount // 2
        liquidation_data = eth_abi.abi.encode(
            ["uint256", "uint256", "address", "bool"],
            [debt_amount, collateral_amount, TOKEN_ADDRESS, False],
        )
        pool_event = _make_log_receipt(
            topics=[AaveV3PoolEvent.LIQUIDATION_CALL.value],
            data=liquidation_data,
        )
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            amount=debt_amount,
            balance_increase=0,
            index=DEFAULT_INDEX,
        )
        operation = _make_operation(
            OperationType.LIQUIDATION,
            pool_event=pool_event,
        )
        position = PositionContext(
            previous_balance=5000,
            previous_index=DEFAULT_INDEX,
            is_bad_debt=False,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.set_balance_to_zero is False


class TestTransfers:
    """Transfer events: raw_amount == scaled_amount, no index calculation."""

    def test_collateral_transfer(self) -> None:
        """Collateral transfer: scaled amount equals raw amount."""
        pipeline = EventPipeline(pool_revision=4)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_TRANSFER,
            amount=1000,
            balance_increase=50,
            index=DEFAULT_INDEX,
        )
        operation = _make_operation(OperationType.BALANCE_TRANSFER)
        position = PositionContext(
            previous_balance=5000,
            previous_index=DEFAULT_INDEX,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta == 1000
        assert result.user_operation == UserOperation.DEPOSIT

    def test_debt_transfer(self) -> None:
        """Debt transfer: balance delta equals raw amount."""
        pipeline = EventPipeline(pool_revision=4)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.DEBT_TRANSFER,
            amount=2000,
            balance_increase=0,
            index=DEFAULT_INDEX,
        )
        operation = _make_operation(OperationType.BALANCE_TRANSFER)
        position = PositionContext(
            previous_balance=5000,
            previous_index=DEFAULT_INDEX,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta == 2000
        assert result.user_operation == UserOperation.BORROW


class TestMintToTreasury:
    """MINT_TO_TREASURY: uses PoolMath for scaled amount."""

    def test_mint_to_treasury(self) -> None:
        """MINT_TO_TREASURY calculates scaled amount via PoolMath."""
        treasury_amount = 10**18
        pipeline = EventPipeline(pool_revision=4)
        scaled_event = _make_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            amount=treasury_amount,
            balance_increase=0,
            index=DEFAULT_INDEX,
        )
        operation = _make_operation(
            OperationType.MINT_TO_TREASURY,
            minted_to_treasury_amount=treasury_amount,
        )
        position = PositionContext(
            previous_balance=0,
            previous_index=DEFAULT_INDEX,
            token_revision=4,
        )

        result = pipeline.process(scaled_event, operation, position)

        assert result.balance_delta > 0
        assert result.user_operation == UserOperation.DEPOSIT


class TestPositionDelta:
    """PositionDelta frozen dataclass tests."""

    def test_default_values(self) -> None:
        """Default optional fields are zero/false."""
        delta = PositionDelta(
            balance_delta=100,
            new_index=DEFAULT_INDEX,
            user_operation=UserOperation.DEPOSIT,
        )
        assert delta.discount_scaled == 0
        assert delta.should_refresh_discount is False
        assert delta.set_balance_to_zero is False

    def test_frozen(self) -> None:
        """PositionDelta is immutable."""
        delta = PositionDelta(
            balance_delta=100,
            new_index=DEFAULT_INDEX,
            user_operation=UserOperation.DEPOSIT,
        )
        with pytest.raises(AttributeError):
            delta.balance_delta = 200  # type: ignore[misc]


class TestPositionContext:
    """PositionContext frozen dataclass tests."""

    def test_default_values(self) -> None:
        """Default optional fields match expected defaults."""
        ctx = PositionContext(
            previous_balance=1000,
            previous_index=DEFAULT_INDEX,
        )
        assert ctx.previous_discount == 0
        assert ctx.token_revision == 0
        assert ctx.pool_revision == 0
        assert ctx.is_gho is False
        assert ctx.is_bad_debt is False

    def test_frozen(self) -> None:
        """PositionContext is immutable."""
        ctx = PositionContext(
            previous_balance=1000,
            previous_index=DEFAULT_INDEX,
        )
        with pytest.raises(AttributeError):
            ctx.previous_balance = 2000  # type: ignore[misc]
