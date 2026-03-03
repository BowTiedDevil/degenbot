"""Test for Issue 0021: Implicit Borrow Processing for DEBT_MINT without BORROW Event.

Issue: Transaction 0x37416a998da98779737e6c62607defcf9d0a7fbfd38651e54b8c058710eb3992
at block 20363588 has a DEBT_MINT event for WETH borrowing but no BORROW event
from the Pool contract. This caused the debt mint to be skipped during processing,
resulting in an incorrect database balance.

Root Cause: The _create_interest_accrual_operations method skipped DEBT_MINT events
when there was a REPAY event in the transaction, even if the DEBT_MINT was for
a different asset (WETH borrow vs weETH repay).

Fix: Added IMPLICIT_BORROW operation type and logic to create these operations
for unassigned DEBT_MINT events that aren't interest accrual.
"""

import pytest
from degenbot.cli.aave_transaction_operations import (
    OperationType,
)


class TestImplicitBorrowOperationType:
    """Test that IMPLICIT_BORROW operation type exists and can be used."""

    def test_implicit_borrow_operation_type_exists(self):
        """Test that IMPLICIT_BORROW operation type was added."""
        assert hasattr(OperationType, "IMPLICIT_BORROW")
        assert OperationType.IMPLICIT_BORROW is not None

    def test_implicit_borrow_has_correct_value(self):
        """Test that IMPLICIT_BORROW has a unique auto value."""
        # Get all operation types
        all_ops = list(OperationType)
        op_names = [op.name for op in all_ops]

        # IMPLICIT_BORROW should be in the list
        assert "IMPLICIT_BORROW" in op_names

        # All values should be unique
        op_values = [op.value for op in all_ops]
        assert len(op_values) == len(set(op_values)), "Operation type values should be unique"

    def test_implicit_borrow_ordering(self):
        """Test that IMPLICIT_BORROW comes after MINT_TO_TREASURY and before UNKNOWN."""
        all_ops = list(OperationType)
        op_names = [op.name for op in all_ops]

        # IMPLICIT_BORROW should be between MINT_TO_TREASURY and UNKNOWN
        assert op_names.index("MINT_TO_TREASURY") < op_names.index("IMPLICIT_BORROW")
        assert op_names.index("IMPLICIT_BORROW") < op_names.index("UNKNOWN")
