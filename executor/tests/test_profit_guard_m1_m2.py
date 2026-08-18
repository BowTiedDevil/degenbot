"""
M1 + M2 regression guards.

M1 (RETRACTED as a guard; kept as documentation + BipsTooHigh):
  Original finding: check_mode != 0 with expected_value == 0 silently skips
  the profit check (`if expected_value > 0`). Analysis showed the "fix" (revert
  when expected_value == 0 and check_mode != 0) is INCOMPATIBLE with the
  no-prefund flash-borrow architecture — most paths legitimately start the
  executor at 0 balance, so expected_value == 0 is correct, not a
  misconfiguration. See .auto/m1-profit-guard-retraction.md.

  What WAS kept: the `bribe_bips <= 10000` (BipsTooHigh) bound — without it,
  bribe_bips > 10000 over-bribes (drains more than the actual profit). That is
  unambiguously correct and breaks nothing.

M2 (KEPT): bribe_recipient_idx == 0 now pays block.coinbase (the block builder)
  instead of burning ETH to address(0).
"""

import pytest

from .conftest_shared import (
    enc_preamble,
    make_config,
    AddressTable,
)

AMOUNT_WETH = 10 * 10**18


class TestM1ProfitGuard:
    """M1: only the BipsTooHigh bound is enforced; the expected_value==0 case
    is legal (flash-borrow paths start the executor at 0)."""

    def test_bribe_too_high_bips_reverts(self, weth, owner_account, executor):
        """bribe_bips > 10000 → BipsTooHigh (the error existed but was never raised)."""
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pre = weth.balanceOf(executor.address) + executor.balance
        at = AddressTable()
        # Bypass make_config's own assertion to hit the contract guard.
        config = (pre << 32) | (10001 << 8) | 1
        with pytest.raises(Exception):
            executor.execute(enc_preamble(at), config, sender=owner_account)

    def test_check_mode_nonzero_with_zero_expected_is_legal(
        self, weth, owner_account, executor
    ):
        """check_mode=1 with expected_value=0 is LEGAL: no-prefund flash-borrow
        paths start the executor at 0 balance. The profit check is a no-op
        (combined_after >= 0 always true), but that is correct behavior, not a
        misconfiguration. Must NOT revert. Uses an empty-but-valid execution
        (a single dummy command is needed because the loop executes
        _execute_command_at before the break check — see test_no_fe_prefix).
        We mint WETH so combined_after is meaningful and assert success."""
        # Provide a minimal valid execution: a SET_ADDRESS in preamble +
        # separator produces an execution-empty stream which reverts on the
        # out-of-bounds slice (unrelated to M1). So instead encode at least
        # one no-op-ish command. V4_SETTLE (0x55) reverts (PM not unlocked) here.
        # The cleanest legal-no-revert config is check_mode=1 + expected_value=0
        # with NO bribe; but execute needs >=1 execution command. Use a
        # WETH_DEPOSIT_ALL which is a no-op when self.balance==0:
        from .conftest_shared import enc_weth_deposit_all
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        at = AddressTable()
        # expected_value=0 (no-prefund), check_mode=1.
        config = make_config(check_mode=1, bribe_bips=0, expected_value=0)
        # WETH_DEPOSIT_ALL when balance==0 deposits 0 → no-op, executes cleanly.
        tx = executor.execute(enc_preamble(at) + enc_weth_deposit_all(), config, sender=owner_account)
        assert tx.status == 1


class TestM2CoinbaseBribe:
    """bribe_recipient_idx == 0 must pay block.coinbase, not burn to address(0)."""

    def test_coinbase_routing_covered_by_bribe_transfer_suite(self):
        """The M2 code change (empty(address) → block.coinbase for idx=0) is
        exercised end-to-end by
        tests/test_cmd_executor_bribe_transfer.py::test_bribe_coinbase_sends_eth,
        which verifies the executor's net gain decreases by exactly the bribe."""
        pytest.skip(
            "Covered by test_cmd_executor_bribe_transfer.py::test_bribe_coinbase_sends_eth"
        )
