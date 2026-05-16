"""Tests for the unified EnrichedScaledTokenEvent model (Plan 010).

The unified model replaces 18 specific Pydantic event classes with a single
class. Tests verify construction, property derivation, and validation routing.
"""

import pytest
from eth_typing import ChecksumAddress
from pydantic import ValidationError
from web3.types import LogReceipt

from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import (
    _BURN_TYPES,
    _INTEREST_TYPES,
    _TRANSFER_TYPES,
    EnrichedScaledTokenEvent,
)


def _fake_address(n: int = 1) -> ChecksumAddress:
    return ChecksumAddress(f"0x{'0' * 39}{n}")


def _fake_log_receipt() -> LogReceipt:
    return {
        "address": _fake_address(),
        "topics": [b"\x00"],
        "data": b"\x00",
        "blockNumber": 1,
        "transactionHash": b"\x00",
        "transactionIndex": 0,
        "blockHash": b"\x00",
        "logIndex": 0,
        "removed": False,
    }


def _base_kwargs(**overrides: object) -> dict:
    """Base kwargs for building an EnrichedScaledTokenEvent.

    Uses scaled_amount=None to skip TokenMath validation in structure tests.
    """
    kwargs: dict = {
        "event": _fake_log_receipt(),
        "event_type": ScaledTokenEventType.COLLATERAL_MINT,
        "user_address": _fake_address(1),
        "raw_amount": 1000,
        "scaled_amount": None,
        "pool_revision": 5,
        "token_revision": 5,
        "token_address": _fake_address(2),
        "underlying_asset": _fake_address(3),
        "index": 10**27,
        "balance_increase": 0,
    }
    kwargs.update(overrides)
    return kwargs


class TestUnifiedModelConstruction:
    """The unified model builds for all event type categories."""

    def test_collateral_mint_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.COLLATERAL_MINT,
                caller_address=_fake_address(4),
            )
        )
        assert event.event_type == ScaledTokenEventType.COLLATERAL_MINT
        assert event.is_collateral is True
        assert event.is_debt is False
        assert event.is_mint is True
        assert event.is_burn is False
        assert event.is_transfer is False
        assert event.is_gho is False
        assert event.is_interest is False

    def test_collateral_burn_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.COLLATERAL_BURN,
                from_address=_fake_address(1),
            )
        )
        assert event.is_collateral is True
        assert event.is_burn is True

    def test_collateral_transfer_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.COLLATERAL_TRANSFER,
                from_address=_fake_address(1),
                to_address=_fake_address(5),
            )
        )
        assert event.is_transfer is True
        assert event.is_collateral is True

    def test_debt_mint_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.DEBT_MINT,
                caller_address=_fake_address(4),
            )
        )
        assert event.is_debt is True
        assert event.is_mint is True
        assert event.is_gho is False

    def test_debt_burn_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.DEBT_BURN,
                from_address=_fake_address(1),
            )
        )
        assert event.is_debt is True
        assert event.is_burn is True

    def test_debt_transfer_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.DEBT_TRANSFER,
                from_address=_fake_address(1),
                to_address=_fake_address(5),
            )
        )
        assert event.is_transfer is True
        assert event.is_debt is True

    def test_gho_debt_mint_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.GHO_DEBT_MINT,
                discount_percent=5000,
                discount_scaled=250,
                caller_address=_fake_address(4),
            )
        )
        assert event.is_debt is True
        assert event.is_gho is True
        assert event.is_mint is True
        assert event.discount_percent == 5000

    def test_gho_debt_burn_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.GHO_DEBT_BURN,
                discount_percent=0,
                discount_scaled=0,
                from_address=_fake_address(1),
            )
        )
        assert event.is_gho is True
        assert event.is_burn is True

    def test_gho_debt_transfer_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.GHO_DEBT_TRANSFER,
                from_address=_fake_address(1),
                to_address=_fake_address(5),
                discount_scaled=0,
            )
        )
        assert event.is_gho is True
        assert event.is_transfer is True

    def test_collateral_interest_mint_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
                balance_increase=1000,
            )
        )
        assert event.is_collateral is True
        assert event.is_mint is True
        assert event.is_interest is True

    def test_debt_interest_burn_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.DEBT_INTEREST_BURN,
                balance_increase=500,
            )
        )
        assert event.is_debt is True
        assert event.is_burn is True
        assert event.is_interest is True

    def test_gho_interest_mint_builds(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.GHO_DEBT_INTEREST_MINT,
                balance_increase=1000,
                discount_percent=3000,
                discount_scaled=100,
            )
        )
        assert event.is_gho is True
        assert event.is_interest is True
        assert event.is_mint is True


class TestUnifiedModelValidation:
    """Validation rules route correctly through the unified model."""

    def test_gho_discount_percent_out_of_range_raises(self) -> None:
        with pytest.raises(ValueError, match="discount_percent"):
            EnrichedScaledTokenEvent(
                **_base_kwargs(
                    event_type=ScaledTokenEventType.GHO_DEBT_MINT,
                    discount_percent=10001,
                    discount_scaled=250,
                )
            )

    def test_gho_discount_percent_negative_raises(self) -> None:
        with pytest.raises(ValueError, match="discount_percent"):
            EnrichedScaledTokenEvent(
                **_base_kwargs(
                    event_type=ScaledTokenEventType.GHO_DEBT_MINT,
                    discount_percent=-1,
                    discount_scaled=0,
                )
            )

    def test_non_gho_event_discount_percent_not_validated(self) -> None:
        """Non-GHO events can have discount_percent=None (not validated)."""
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(event_type=ScaledTokenEventType.COLLATERAL_MINT)
        )
        assert event.discount_percent is None

    def test_transfer_events_skip_index_validation(self) -> None:
        """Transfer events don't need index-based TokenMath validation."""
        # scaled_amount set to a value, but no TokenMath validation runs
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.COLLATERAL_TRANSFER,
                scaled_amount=999,  # arbitrary, not validated
                raw_amount=999,
                from_address=_fake_address(1),
                to_address=_fake_address(5),
            )
        )
        assert event.scaled_amount == 999

    def test_interest_events_skip_tokenmath_validation(self) -> None:
        """Interest events don't go through TokenMath validation."""
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(
                event_type=ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
                scaled_amount=0,  # standard for interest accrual
                balance_increase=1000,
            )
        )
        assert event.scaled_amount == 0


class TestUnifiedModelImmutability:
    """The model is frozen."""

    def test_model_is_frozen(self) -> None:
        event = EnrichedScaledTokenEvent(
            **_base_kwargs(event_type=ScaledTokenEventType.COLLATERAL_MINT)
        )
        with pytest.raises(ValidationError, match="frozen_instance"):
            event.raw_amount = 999


class TestPropertyDerivation:
    """Properties derive correctly from event_type for all combinations."""

    @pytest.mark.parametrize(
        ("event_type", "expected_collateral", "expected_debt", "expected_gho"),
        [
            (ScaledTokenEventType.COLLATERAL_MINT, True, False, False),
            (ScaledTokenEventType.COLLATERAL_BURN, True, False, False),
            (ScaledTokenEventType.COLLATERAL_TRANSFER, True, False, False),
            (ScaledTokenEventType.COLLATERAL_INTEREST_MINT, True, False, False),
            (ScaledTokenEventType.COLLATERAL_INTEREST_BURN, True, False, False),
            (ScaledTokenEventType.DEBT_MINT, False, True, False),
            (ScaledTokenEventType.DEBT_BURN, False, True, False),
            (ScaledTokenEventType.DEBT_TRANSFER, False, True, False),
            (ScaledTokenEventType.DEBT_INTEREST_MINT, False, True, False),
            (ScaledTokenEventType.DEBT_INTEREST_BURN, False, True, False),
            (ScaledTokenEventType.GHO_DEBT_MINT, False, True, True),
            (ScaledTokenEventType.GHO_DEBT_BURN, False, True, True),
            (ScaledTokenEventType.GHO_DEBT_TRANSFER, False, True, True),
            (ScaledTokenEventType.GHO_DEBT_INTEREST_MINT, False, True, True),
            (ScaledTokenEventType.GHO_DEBT_INTEREST_BURN, False, True, True),
        ],
    )
    def test_category_properties(
        self,
        event_type: ScaledTokenEventType,
        expected_collateral: bool,  # noqa: FBT001
        expected_debt: bool,  # noqa: FBT001
        expected_gho: bool,  # noqa: FBT001
    ) -> None:
        kwargs = _base_kwargs(event_type=event_type)

        # Add required fields based on event type
        if event_type in {ScaledTokenEventType.COLLATERAL_MINT, ScaledTokenEventType.DEBT_MINT}:
            kwargs["caller_address"] = _fake_address(4)
        if event_type in {ScaledTokenEventType.GHO_DEBT_MINT, ScaledTokenEventType.GHO_DEBT_BURN}:
            kwargs["discount_percent"] = 0
            kwargs["discount_scaled"] = 0
            if event_type == ScaledTokenEventType.GHO_DEBT_MINT:
                kwargs["caller_address"] = _fake_address(4)
        if event_type == ScaledTokenEventType.GHO_DEBT_TRANSFER:
            kwargs["from_address"] = _fake_address(1)
            kwargs["to_address"] = _fake_address(5)
            kwargs["discount_scaled"] = 0
        if event_type in _TRANSFER_TYPES:
            kwargs["from_address"] = _fake_address(1)
            kwargs["to_address"] = _fake_address(5)
        if event_type in _BURN_TYPES and "from_address" not in kwargs:
            kwargs["from_address"] = _fake_address(1)
        if event_type in _INTEREST_TYPES:
            kwargs["balance_increase"] = 0

        event = EnrichedScaledTokenEvent(**kwargs)
        assert event.is_collateral == expected_collateral
        assert event.is_debt == expected_debt
        assert event.is_gho == expected_gho

    @pytest.mark.parametrize(
        ("event_type", "expected_mint", "expected_burn", "expected_transfer", "expected_interest"),
        [
            (ScaledTokenEventType.COLLATERAL_MINT, True, False, False, False),
            (ScaledTokenEventType.COLLATERAL_BURN, False, True, False, False),
            (ScaledTokenEventType.COLLATERAL_TRANSFER, False, False, True, False),
            (ScaledTokenEventType.COLLATERAL_INTEREST_MINT, True, False, False, True),
            (ScaledTokenEventType.COLLATERAL_INTEREST_BURN, False, True, False, True),
            (ScaledTokenEventType.DEBT_MINT, True, False, False, False),
            (ScaledTokenEventType.DEBT_BURN, False, True, False, False),
            (ScaledTokenEventType.DEBT_TRANSFER, False, False, True, False),
            (ScaledTokenEventType.DEBT_INTEREST_MINT, True, False, False, True),
            (ScaledTokenEventType.DEBT_INTEREST_BURN, False, True, False, True),
            (ScaledTokenEventType.GHO_DEBT_MINT, True, False, False, False),
            (ScaledTokenEventType.GHO_DEBT_BURN, False, True, False, False),
            (ScaledTokenEventType.GHO_DEBT_TRANSFER, False, False, True, False),
            (ScaledTokenEventType.GHO_DEBT_INTEREST_MINT, True, False, False, True),
            (ScaledTokenEventType.GHO_DEBT_INTEREST_BURN, False, True, False, True),
        ],
    )
    def test_direction_properties(
        self,
        event_type: ScaledTokenEventType,
        expected_mint: bool,  # noqa: FBT001
        expected_burn: bool,  # noqa: FBT001
        expected_transfer: bool,  # noqa: FBT001
        expected_interest: bool,  # noqa: FBT001
    ) -> None:
        kwargs = _base_kwargs(event_type=event_type)

        if event_type in {ScaledTokenEventType.COLLATERAL_MINT, ScaledTokenEventType.DEBT_MINT}:
            kwargs["caller_address"] = _fake_address(4)
        if event_type in {ScaledTokenEventType.GHO_DEBT_MINT, ScaledTokenEventType.GHO_DEBT_BURN}:
            kwargs["discount_percent"] = 0
            kwargs["discount_scaled"] = 0
            if event_type == ScaledTokenEventType.GHO_DEBT_MINT:
                kwargs["caller_address"] = _fake_address(4)
        if event_type == ScaledTokenEventType.GHO_DEBT_TRANSFER:
            kwargs["from_address"] = _fake_address(1)
            kwargs["to_address"] = _fake_address(5)
            kwargs["discount_scaled"] = 0
        if event_type in _TRANSFER_TYPES:
            kwargs["from_address"] = _fake_address(1)
            kwargs["to_address"] = _fake_address(5)
        if event_type in _BURN_TYPES and "from_address" not in kwargs:
            kwargs["from_address"] = _fake_address(1)
        if event_type in _INTEREST_TYPES:
            kwargs["balance_increase"] = 0

        event = EnrichedScaledTokenEvent(**kwargs)
        assert event.is_mint == expected_mint
        assert event.is_burn == expected_burn
        assert event.is_transfer == expected_transfer
        assert event.is_interest == expected_interest
