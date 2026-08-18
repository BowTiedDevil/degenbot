"""
L1 + L2 verification tests.

L1 (docs): SEND_ETH / SEND_ETH_ALL docstrings had stale 0x2A/0x2B opcodes
(actual: 0x16/0x17). Doc-only; this test asserts the encoding helpers emit the
correct opcodes by cross-checking against the command constants used in tests.

L2 (initialize fix): initialize() required msg.value == 2 but only consumed
1 wei (WETH.deposit(value=1)), stranding 1 wei ETH forever. Also had no owner
gate — anyone could call it. Post-fix: msg.value == 1 (no stranded ETH) and
msg.sender == OWNER_ADDR enforced.
"""

import pytest

from .conftest_shared import enc_send_eth, enc_send_eth_all, CMD_SEND_ETH, CMD_SEND_ETH_ALL


class TestL1SendEthDocstrings:
    """Verify SEND_ETH/SEND_ETH_ALL opcodes are 0x16/0x17 (the docstrings now match)."""

    def test_send_eth_opcode(self):
        # enc_send_eth emits [opcode][recipient_idx:1][amount:12]
        encoded = enc_send_eth(recipient_idx=1, amount=1)
        assert encoded[0:1] == CMD_SEND_ETH == b"\x16"

    def test_send_eth_all_opcode(self):
        encoded = enc_send_eth_all(recipient_idx=1)
        assert encoded[0:1] == CMD_SEND_ETH_ALL == b"\x17"


class TestL2Initialize:
    """initialize() requires exactly 1 wei and owner-only."""

    def test_initialize_rejects_non_owner(
        self, project, weth, v4_pm, accounts
    ):
        """A non-owner calling initialize reverts (Unauthorized)."""
        executor = project.cmd_executor.deploy(
            weth.address, v4_pm.address, sender=accounts[0]
        )
        attacker = accounts[5]
        accounts[0].transfer(attacker, 10**16)
        with pytest.raises(Exception):
            executor.initialize(value=1, sender=attacker)

    def test_initialize_rejects_wrong_value(
        self, project, weth, v4_pm, owner_account
    ):
        """msg.value == 2 (the old amount) now reverts (InvalidMsgValue)."""
        executor = project.cmd_executor.deploy(
            weth.address, v4_pm.address, sender=owner_account
        )
        with pytest.raises(Exception):
            executor.initialize(value=2, sender=owner_account)

    def test_initialize_one_wei_succeeds_and_strands_no_eth(
        self, project, weth, v4_pm, owner_account
    ):
        """msg.value == 1 (attached to the initialize call) succeeds. Post-call
        the executor must NOT have a stranded 1 wei ETH (the old msg.value==2
        path left 1 wei stuck). The 1 wei comes via msg.value of the initialize
        call itself — NOT a separate ETH transfer (which __default__ now rejects
        per the H3 donation gate)."""
        executor = project.cmd_executor.deploy(
            weth.address, v4_pm.address, sender=owner_account
        )
        # Executor has 0 ETH after deploy.
        assert executor.balance == 0
        # initialize with value=1 attached to the call (msg.value funds WETH.deposit).
        executor.initialize(value=1, sender=owner_account)
        # Post-fix: the 1 wei is consumed by WETH.deposit; no stranded ETH.
        # (Pre-fix with msg.value==2, 1 wei would be stranded here.)
        assert executor.balance == 0, (
            "initialize must not strand ETH; the 1 wei must be fully consumed by WETH.deposit"
        )
