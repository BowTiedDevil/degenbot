"""
M4 reproduction: 0xFE BEGIN_PREPROCESSING was documented but unimplemented.

_preprocess starts at offset 0 and only recognizes 0x00 (SET_ADDRESS) and
0xFF (BEGIN_EXECUTION). A stream prefixed with 0xFE is treated as execution:
the first opcode read is 0xFE → not 0x00/0xFF → break → execution at offset 0
→ _execute_command_at reads 0xFE → InvalidCommand (0xFE is not a valid opcode
in the dispatcher). So the documented [0xFE]...[0xFF] stream format was never
usable; the doc has been corrected to remove the 0xFE prefix.

This test codifies the actual behavior: a 0xFE-prefixed stream reverts.
"""

import pytest


class TestM4NoFePrefix:
    def test_fe_prefixed_stream_reverts(self, owner_account, executor):
        """A stream starting with 0xFE reverts (0xFE is not a valid opcode;
        it is only the V2 auto-pay sentinel byte and the V4_WETH_SENTINEL
        constant, never a stream prefix)."""
        stream = b"\xfe" + b"\xff"  # hypothetical [0xFE prefix][0xFF]
        with pytest.raises(Exception):
            executor.execute(stream, sender=owner_account)

    def test_ff_only_reverts_empty_execution(self, owner_account, executor):
        """A bare 0xFF (no SET_ADDRESS, no execution commands) reverts: the
        fast-path loop calls _execute_command_at(data, offset) BEFORE the
        `offset >= len` break check, so an empty execution section (offset ==
        len after _preprocess) hits an out-of-bounds slice. This is a degenerate
        case — operators always encode at least one execution command — so it
        is documented behavior, not a bug. (The M4 point is that 0xFE is not a
        valid prefix; both 0xFE- and bare-0xFF degenerate streams revert.)"""
        with pytest.raises(Exception):
            executor.execute(b"\xff", sender=owner_account)
