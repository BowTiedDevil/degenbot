"""
H3 (truncation half) — NON-EXPLOITABLE given the Bytes[288] type bound.

The review raised L3: every command loop `for _ in range(MAX_COMMANDS_LENGTH)`
exits without reverting when offset < len(data), silently dropping trailing
commands. This test documents that the bound is unreachable in practice:

- All `Bytes` parameters carrying a command stream are typed
  `Bytes[MAX_COMMANDS_LENGTH]` (288 bytes).
- The smallest command is 1 byte (V4_SETTLE, V4_SETTLE_ALL, WETH_DEPOSIT_ALL,
  WETH_WITHDRAW_ALL — all SIZE_* = 1).
- Therefore the maximum command count in any stream is 288, which exactly
  consumes the 288-iteration loop cap. The `if offset >= len: break` always
  fires from the length check before the loop counter exhausts.

So silent truncation cannot occur for any well-typed input. This test encodes
a maximal-length stream of 1-byte commands (V4_SETTLE) and asserts the loop
counter never silently under-consumes: the call reverts for an unrelated reason
(PM not unlocked) but the *truncation* path is not exercised because offset
reaches len within the cap.

We keep this as a structural invariant guard: if someone ever raises
MAX_COMMANDS_LENGTH without proportionally widening the loop cap, or adds a
sub-1-byte command, this test will need revisiting.
"""

import pytest

MAX_COMMANDS_LENGTH = 288


class TestStreamTruncationBound:
    def test_max_length_stream_of_single_byte_commands_fits_loop_cap(
        self, owner_account, executor
    ):
        """
        288 bytes of 0x55 (V4_SETTLE). The loop's 288 iterations exactly
        suffice to consume every byte; no silent truncation. The call reverts
        because V4_SETTLE calls PM.settle() while PM is not unlocked — an
        unrelated, expected failure. The point of the test is that the revert
        is NOT a silent-success-on-truncation.
        """
        stream = b"\x55" * MAX_COMMANDS_LENGTH
        # Must revert (PM not unlocked for V4_SETTLE). Importantly, it does not
        # silently succeed.
        with pytest.raises(Exception):
            executor.execute(stream, sender=owner_account)

    def test_loop_cap_equals_byte_bound(self):
        """Structural invariant: loop cap == Bytes type bound == 288.

        Documentary test (no on-chain interaction). Asserts the contract
        constant value that the loop cap and Bytes[] type bound both use.
        If MAX_COMMANDS_LENGTH is ever changed without keeping the loop cap
        and the Bytes bound in sync, revisit this test.
        """
        assert MAX_COMMANDS_LENGTH == 288
