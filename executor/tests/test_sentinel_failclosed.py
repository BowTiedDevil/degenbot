"""
Fail-closed sentinel/edge tests for the executor's sentinel and index guards.

Each command that accepts a sentinel byte or bounded index must reject
out-of-range values with InvalidCommand rather than silently no-op'ing,
bounds-reverting opaquely, or wrapping. The V4 hook-sentinel slot is the
exception: invalid hook sentinels are already fail-closed through the
t_addresses bounds check (empty returndata).

All revert assertions run through _expect_revert / _expect_accepted, which
pin the Ape-level revert data and fail on success, on unidentified reverts,
and on unrelated harness errors (proven by TestRevertHelpers).
"""

from typing import NamedTuple

import pytest
from ape.exceptions import ContractLogicError, CustomError, TransactionError
from ethpm_types.abi import ABIType, ErrorABI

from .conftest_shared import (
    AddressTable,
    enc_preamble,
    enc_v4_settle_delta,
    enc_v4_swap_compact,
    enc_v4_swap_dynamic,
    enc_v4_take_delta,
    enc_v4_unlock,
)

# Sentinel bytes (must match contract constants).
V4_WETH_SENTINEL = 0xFE
V4_SELF_SENTINEL = 0xFD
V4_PM_SENTINEL = 0xFC
V4_NATIVE_SENTINEL = 0xFF

# A revert with empty returndata surfaces in Ape as its default message: the
# shape of the t_addresses sentinel bounds check, which is a bare Vyper
# assert (no custom error, no revert data to decode).
_NO_REVERT_DATA = TransactionError.DEFAULT_MESSAGE


def _revert_id(err):
    """Name the revert an Ape exception carries, for assertion messages."""
    if isinstance(err, CustomError):
        fields = ", ".join(f"{k}={v}" for k, v in sorted(err.inputs.items()))
        return f"{err.name}({fields})"
    return repr(err.revert_message)


def _revert_is(err, name):
    """Whether the revert identifies `name`: a decoded custom error of that
    name, or a bubbled revert message equal to it."""
    if isinstance(err, CustomError):
        return err.name == name
    return err.revert_message == name


def _expect_revert(callable_, *, custom_error=None, opcode=None, message=None):
    """Assert `callable_` reverts at the guard, with the guard's revert data.

    Only Ape's revert exceptions are handled. A decoded custom error must
    match `custom_error` (and `opcode`, when pinned); a revert without
    decodable data must match `message` exactly. A successful call, a silent
    reverted receipt (no revert data to assert on), or any unrelated
    exception fails the test — nothing is swallowed.
    """
    assert (custom_error is None) != (message is None), "pin exactly one revert identity"
    # Capture the outcome first, assert afterwards: every assertion runs
    # outside any except block, so none of them can be swallowed.
    try:
        tx = callable_()
        revert = None
    except ContractLogicError as err:
        tx = None
        revert = err
    if revert is not None:
        if custom_error is not None:
            assert isinstance(revert, CustomError), (
                f"expected custom error {custom_error}, got {_revert_id(revert)}"
            )
            assert revert.name == custom_error, f"expected {custom_error}, got {_revert_id(revert)}"
            if opcode is not None:
                assert revert.inputs.get("opcode") == opcode, (
                    f"{custom_error} opcode: expected {opcode}, got {revert.inputs}"
                )
        else:
            assert not isinstance(revert, CustomError), (
                f"expected the empty-data revert {message!r}, got {_revert_id(revert)}"
            )
            assert revert.revert_message == message, (
                f"expected the empty-data revert {message!r}, got {_revert_id(revert)}"
            )
        return
    assert tx.status == 0, "expected revert, transaction succeeded"
    msg = "expected revert data from an Ape revert exception, got a silent reverted receipt"
    raise AssertionError(msg)


def _expect_accepted(callable_, *, downstream=None):
    """Assert `callable_` passes the guard under test.

    The transaction must succeed, or Ape must raise exactly the identified
    `downstream` revert — a failure downstream of the guard, never the
    guard's own revert. Any other revert or any unrelated exception fails
    the test.
    """
    # Capture the outcome first, assert afterwards: every assertion runs
    # outside any except block, so none of them can be swallowed.
    try:
        tx = callable_()
        revert = None
    except ContractLogicError as err:
        tx = None
        revert = err
    if revert is not None:
        assert downstream is not None, f"expected the guard to pass, got {_revert_id(revert)}"
        assert _revert_is(revert, downstream), (
            f"expected success or the {downstream} revert, got {_revert_id(revert)}"
        )
        return
    assert tx.status == 1, "expected the guard to pass, transaction reverted"


class TestM5V4TakeDeltaGuard:
    """V4_TAKE_DELTA must guard against delta <= 0 (no opaque PM revert)."""

    def test_take_delta_zero_delta_no_op_not_opaque_revert(
        self, weth, owner_account, executor, v4_pm
    ):
        """V4_TAKE_DELTA(WETH, self) inside an unlock with no prior swap sees
        delta == 0 and skips the take, so the unlock exits cleanly instead of
        PM.take(0) bubbling an opaque revert."""
        at = AddressTable()
        weth_idx = at.add(weth.address)
        commands = enc_v4_unlock(enc_v4_take_delta(weth_idx, V4_SELF_SENTINEL))
        tx = executor.execute(enc_preamble(at) + commands, sender=owner_account)
        assert tx.status == 1, "V4_TAKE_DELTA with zero delta should be a clean no-op"


class TestM6HookSentinelFailClosed:
    """Hook sentinel bytes other than 0xFF fail closed through the Vyper
    t_addresses bounds check (t_addresses[252..254] reverts).

    No explicit InvalidCommand guard sits on the hooks lookup: one on the hot
    V4-swap path costs gas for a purely cosmetic error-message improvement.
    These tests pin the existing fail-closed behavior — an empty-returndata
    revert, never success and never a silent mis-resolution."""

    def test_v4_swap_compact_pm_hook_sentinel_reverts(
        self, weth, usdc, owner_account, executor, v4_pm
    ):
        """hooks_idx = 0xFC (PM sentinel) — not a valid hook contract."""
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        commands = enc_v4_swap_compact(weth_idx, usdc_idx, 3000, 60, V4_PM_SENTINEL, True, 10**18)
        _expect_revert(
            lambda: executor.execute(enc_preamble(at) + commands, sender=owner_account),
            message=_NO_REVERT_DATA,
        )

    def test_v4_swap_compact_weth_hook_sentinel_reverts(
        self, weth, usdc, owner_account, executor, v4_pm
    ):
        """hooks_idx = 0xFE (WETH sentinel) — not a valid hook contract."""
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        commands = enc_v4_swap_compact(weth_idx, usdc_idx, 3000, 60, V4_WETH_SENTINEL, True, 10**18)
        _expect_revert(
            lambda: executor.execute(enc_preamble(at) + commands, sender=owner_account),
            message=_NO_REVERT_DATA,
        )

    def test_v4_swap_dynamic_self_hook_sentinel_reverts(
        self, weth, usdc, owner_account, executor, v4_pm
    ):
        """V4_SWAP_DYNAMIC with hooks_idx = 0xFD (SELF sentinel) — not a
        valid hook contract."""
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        commands = enc_v4_swap_dynamic(weth_idx, usdc_idx, 3000, 60, V4_SELF_SENTINEL, True)
        _expect_revert(
            lambda: executor.execute(enc_preamble(at) + commands, sender=owner_account),
            message=_NO_REVERT_DATA,
        )

    def test_v4_swap_compact_native_hook_sentinel_allowed(
        self, weth, usdc, owner_account, executor, v4_pm
    ):
        """hooks_idx = 0xFF (NATIVE sentinel = no hooks) is the one valid
        sentinel: the hook lookup must pass. The swap then runs at top level,
        outside a V4 unlock, so the PoolManager's lock guard (ManagerLocked)
        is the identified downstream revert; success would mean the pool took
        the swap. Either way the hooks sentinel guard must not fire."""
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        commands = enc_v4_swap_compact(
            weth_idx, usdc_idx, 3000, 60, V4_NATIVE_SENTINEL, True, 10**18
        )
        _expect_accepted(
            lambda: executor.execute(enc_preamble(at) + commands, sender=owner_account),
            downstream="ManagerLocked",
        )


class TestM7V4SettleDeltaSentinelFailClosed:
    """V4_SETTLE_DELTA with PM/SELF sentinel must raise InvalidCommand, not silently skip."""

    def _run_unlock_with_settle_delta(self, executor, at, owner, currency_idx):
        commands = enc_v4_unlock(enc_v4_settle_delta(currency_idx))
        return executor.execute(enc_preamble(at) + commands, sender=owner)

    def test_settle_delta_pm_sentinel_reverts(self, weth, owner_account, executor, v4_pm):
        at = AddressTable()
        _expect_revert(
            lambda: self._run_unlock_with_settle_delta(executor, at, owner_account, V4_PM_SENTINEL),
            custom_error="InvalidCommand",
            opcode=V4_PM_SENTINEL,
        )

    def test_settle_delta_self_sentinel_reverts(self, weth, owner_account, executor, v4_pm):
        at = AddressTable()
        _expect_revert(
            lambda: self._run_unlock_with_settle_delta(
                executor, at, owner_account, V4_SELF_SENTINEL
            ),
            custom_error="InvalidCommand",
            opcode=V4_SELF_SENTINEL,
        )


class TestM3BribeRecipientBounds:
    """bribe_recipient_idx >= 32 must raise InvalidCommand, not bounds-revert opaquely."""

    def _no_op_stream(self, at, weth):
        """A minimal valid execution stream for the config guard under test.

        A stream whose execution section is empty reverts in the dispatch
        loop before any config handling (see test_no_fe_prefix), so the
        guard needs one benign command: V4_TAKE_DELTA on a zero delta is a
        clean no-op and moves no value, so profit stays zero.
        """
        weth_idx = at.add(weth.address)
        return enc_preamble(at) + enc_v4_unlock(enc_v4_take_delta(weth_idx, V4_SELF_SENTINEL))

    def test_bribe_recipient_idx_32_reverts(self, weth, owner_account, executor):
        at = AddressTable()
        # bips=1, idx=32, mode=1 (WETH+ETH check) — idx=32 is out of bounds.
        config = (1 << 8) | (32 << 24) | 1
        _expect_revert(
            lambda: executor.execute(self._no_op_stream(at, weth), config, sender=owner_account),
            custom_error="InvalidCommand",
            opcode=32,
        )

    def test_bribe_recipient_idx_sentinel_reverts(self, weth, owner_account, executor):
        at = AddressTable()
        # idx = 0xFC (PM sentinel) — not a meaningful bribe target.
        config = (1 << 8) | (V4_PM_SENTINEL << 24) | 1
        _expect_revert(
            lambda: executor.execute(self._no_op_stream(at, weth), config, sender=owner_account),
            custom_error="InvalidCommand",
            opcode=V4_PM_SENTINEL,
        )

    def test_bribe_recipient_idx_31_accepted(self, weth, owner_account, executor):
        """idx=31 is the max valid table slot and must pass the guard. The
        stream earns no profit, so no bribe transfer is attempted and the
        transaction must succeed end to end."""
        at = AddressTable()
        config = (1 << 8) | (31 << 24) | 1
        _expect_accepted(
            lambda: executor.execute(self._no_op_stream(at, weth), config, sender=owner_account)
        )


# ── Meta-tests: the revert helpers themselves ──


class _FakeReceipt(NamedTuple):
    """Stand-in for an Ape receipt; the helpers only read ``status``."""

    status: int


def _raise_harness_error():
    msg = "harness exploded"
    raise RuntimeError(msg)


def _raise_revert(message):
    def _raise():
        raise ContractLogicError(revert_message=message)

    return _raise


def _raise_custom_error(name, **fields):
    def _raise():
        raise CustomError(
            abi=ErrorABI(name=name, inputs=[ABIType(name=k, type="uint256") for k in fields]),
            inputs=dict(fields),
        )

    return _raise


class TestRevertHelpers:
    """The revert helpers must fail on success and let unrelated harness
    errors through — the behaviors a catch-all ``except`` destroys."""

    def test_expect_revert_fails_on_success(self):
        with pytest.raises(AssertionError, match="expected revert"):
            _expect_revert(lambda: _FakeReceipt(status=1), custom_error="InvalidCommand", opcode=32)

    def test_expect_revert_fails_on_silent_reverted_receipt(self):
        with pytest.raises(AssertionError, match="silent reverted receipt"):
            _expect_revert(lambda: _FakeReceipt(status=0), custom_error="InvalidCommand", opcode=32)

    def test_expect_revert_propagates_unrelated_exception(self):
        with pytest.raises(RuntimeError, match="harness exploded"):
            _expect_revert(_raise_harness_error, custom_error="InvalidCommand", opcode=32)

    def test_expect_revert_fails_on_wrong_error(self):
        with pytest.raises(AssertionError, match="expected custom error"):
            _expect_revert(_raise_revert(_NO_REVERT_DATA), custom_error="InvalidCommand", opcode=32)

    def test_expect_revert_fails_on_wrong_opcode(self):
        with pytest.raises(AssertionError, match="opcode"):
            _expect_revert(
                _raise_custom_error("InvalidCommand", opcode=7),
                custom_error="InvalidCommand",
                opcode=32,
            )

    def test_expect_revert_fails_on_wrong_message(self):
        with pytest.raises(AssertionError, match="expected the empty-data revert"):
            _expect_revert(_raise_revert("ManagerLocked"), message=_NO_REVERT_DATA)

    def test_expect_accepted_fails_on_guard_revert(self):
        with pytest.raises(AssertionError, match="expected the guard to pass"):
            _expect_accepted(_raise_custom_error("InvalidCommand", opcode=32))

    def test_expect_accepted_fails_on_unidentified_revert(self):
        with pytest.raises(AssertionError, match="ManagerLocked"):
            _expect_accepted(_raise_revert(_NO_REVERT_DATA), downstream="ManagerLocked")

    def test_expect_accepted_propagates_unrelated_exception(self):
        with pytest.raises(RuntimeError, match="harness exploded"):
            _expect_accepted(_raise_harness_error, downstream="ManagerLocked")

    def test_expect_accepted_accepts_success(self):
        _expect_accepted(lambda: _FakeReceipt(status=1), downstream="ManagerLocked")

    def test_expect_accepted_accepts_identified_downstream(self):
        _expect_accepted(_raise_revert("ManagerLocked"), downstream="ManagerLocked")
