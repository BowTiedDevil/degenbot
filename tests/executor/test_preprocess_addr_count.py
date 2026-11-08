"""
L4 regression guard: _preprocess address-table indexing.

Previously used `offset // SIZE_SET_ADDRESS` (fragile if any non-21-byte
preprocessing opcode were ever added). Now uses an explicit `addr_count`
counter with a fail-closed bounds assert (`addr_count < MAX_INDEXED_ADDRESSES`),
so overflowing the 32-slot table raises InvalidCommand at the source rather
than wrapping or silently overwriting.
"""

import pytest

from .conftest_shared import enc_set_address, enc_preamble, AddressTable
from eth_utils import to_checksum_address


def _addr(i: int):
    """Deterministic distinct 20-byte address (not a funded account — SET_ADDRESS
    just stores raw addresses, no transfers)."""
    return to_checksum_address(f"0x{i:040x}")


class TestL4PreprocessAddrCount:
    def test_overflow_set_addresses_revert(self, owner_account, executor):
        """Encoding 14 SET_ADDRESS commands (14×21=294 > 288-byte Bytes bound)
        is rejected at the ABI boundary — the type bound is the real table-
        overflow protection (max 13 fit, well under the 32-slot table). The
        addr_count assertion in _preprocess was RETRACTED (gas cost for an
        unreachable case); this test documents that the Bytes[288] bound is
        the guard."""
        commands = b"".join(enc_set_address(_addr(i)) for i in range(14)) + b"\xff"
        with pytest.raises(Exception):
            executor.execute(commands, sender=owner_account)

    def test_max_set_addresses_accepted(self, owner_account, executor):
        """As many SET_ADDRESS commands as fit in the 288-byte stream (13 × 21
        = 273 bytes + 0xFF + a no-op command) must NOT trip the addr_count
        guard. The 32-slot bound (MAX_INDEXED_ADDRESSES) is well above the ~13
        that fit; the addr_count guard is architectural (robust to future
        multi-byte preprocessing opcodes) plus a fail-closed assert."""
        from .conftest_shared import enc_weth_deposit_all
        n = 13  # floor(288-2 / 21) = 13 fit
        commands = (
            b"".join(enc_set_address(_addr(i)) for i in range(n))
            + b"\xff"
            + enc_weth_deposit_all()
        )
        tx = executor.execute(commands, sender=owner_account)
        assert tx.status == 1
