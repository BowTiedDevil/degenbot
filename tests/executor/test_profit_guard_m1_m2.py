"""Regression guards for bribe bounds and zero-minimum flash execution."""

import pytest

from .conftest_shared import AddressTable, enc_preamble, enc_weth_deposit_all, make_config

AMOUNT_WETH = 10 * 10**18


def test_bribe_too_high_bips_reverts(weth, owner_account, executor):
    """A requested bribe above 100% reverts independently of min_profit."""
    weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
    config = (10_001 << 8) | 1

    with pytest.raises(Exception):
        executor.execute(enc_preamble(AddressTable()), config, sender=owner_account)


def test_zero_min_profit_is_legal_for_no_prefund_execution(weth, owner_account, executor):
    """Zero minimum is valid when the command stream is non-losing."""
    weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
    config = make_config(profit_mode=1, bribe_bips=0, min_profit=0)

    tx = executor.execute(
        enc_preamble(AddressTable()) + enc_weth_deposit_all(),
        config,
        sender=owner_account,
    )

    assert tx.status == 1
