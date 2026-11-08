"""External execution-receipt and profit-accounting semantics."""

import pytest

from .conftest_shared import (
    AddressTable,
    enc_erc20_transfer,
    enc_preamble,
    enc_v2_swap_direct,
    enc_weth_deposit_all,
    enc_weth_withdraw,
    v2_get_amount_out,
)


@pytest.fixture
def v2_pair(project, owner_account, weth, usdc):
    token0, token1 = sorted([weth.address, usdc.address], key=str.lower)
    return project.fake_uniswap_v2_pair.deploy(token0, token1, 0, 30, sender=owner_account)


@pytest.mark.parametrize("profit_mode", [1, 2])
def test_execute_returns_zero_receipt(executor, owner_account, profit_mode):
    """A measured zero is a two-field receipt, not an unchecked scalar zero."""
    receipt = executor.execute.call(
        enc_preamble(AddressTable()) + enc_weth_deposit_all(),
        profit_mode,
        sender=owner_account,
        gas=1_000_000,
    )

    assert receipt == (0, 0)


@pytest.mark.parametrize("bribe_bips", [0, 5000])
def test_physical_receipt_and_bribe_accounting(
    executor, owner_account, weth, usdc, v2_pair, accounts, bribe_bips
):
    amount_in = 1_000 * 10**6
    preexisting_weth = 7 * 10**18
    usdc.mint(v2_pair.address, amount_in * 100, sender=owner_account)
    weth.mint(v2_pair.address, 100 * 10**18, sender=owner_account)
    v2_pair.sync(sender=owner_account)
    expected_gross = v2_get_amount_out(
        amount_in, usdc.balanceOf(v2_pair), weth.balanceOf(v2_pair), 30
    )

    usdc.mint(executor.address, amount_in, sender=owner_account)
    weth.mint(executor.address, preexisting_weth, sender=owner_account)
    before = weth.balanceOf(executor) + executor.balance
    bribe_recipient = accounts[1]
    recipient_before = bribe_recipient.balance

    at = AddressTable()
    usdc_idx = at.add(usdc.address)
    pair_idx = at.add(v2_pair.address)
    executor_idx = at.add(executor.address)
    bribe_recipient_idx = at.add(bribe_recipient.address)
    commands = enc_erc20_transfer(usdc_idx, pair_idx, amount_in)
    zero_for_one = v2_pair.token0() == usdc.address
    commands += enc_v2_swap_direct(pair_idx, zero_for_one, expected_gross, executor_idx)
    stream = enc_preamble(at) + commands

    expected_bribe = expected_gross * bribe_bips // 10_000
    base_config = (bribe_bips << 8) | (bribe_recipient_idx << 24) | 1
    receipt = executor.execute.call(stream, base_config, sender=owner_account, gas=2_000_000)
    assert receipt == (expected_gross, expected_bribe)

    with pytest.raises(Exception):
        executor.execute.call(
            stream,
            ((expected_gross + 1) << 32) | base_config,
            sender=owner_account,
            gas=2_000_000,
        )

    exact_min_profit = (expected_gross << 32) | base_config
    tx = executor.execute(stream, exact_min_profit, sender=owner_account)
    assert tx.status == 1
    after = weth.balanceOf(executor) + executor.balance
    assert after - before == expected_gross - expected_bribe
    assert bribe_recipient.balance - recipient_before == expected_bribe


@pytest.mark.parametrize("mode", [0, 3, 4, 255])
def test_invalid_profit_modes_fail_loudly(executor, owner_account, mode):
    stream = enc_preamble(AddressTable()) + enc_weth_deposit_all()

    with pytest.raises(Exception):
        executor.execute.call(stream, mode, sender=owner_account, gas=1_000_000)


def test_default_config_rejects_physical_balance_decrease(executor, owner_account, weth, accounts):
    amount = 10**18
    weth.mint(executor.address, amount, sender=owner_account)
    at = AddressTable()
    weth_idx = at.add(weth.address)
    recipient_idx = at.add(accounts[1].address)
    stream = enc_preamble(at) + enc_erc20_transfer(weth_idx, recipient_idx, amount)

    with pytest.raises(Exception):
        executor.execute.call(stream, sender=owner_account, gas=1_000_000)


def test_sweep_is_separate_from_execution(executor, owner_account, weth, accounts):
    destination = accounts[1]
    weth_amount = 3 * 10**18
    native_amount = 2 * 10**18
    weth.mint(executor.address, weth_amount, sender=owner_account)
    executor.execute(
        enc_preamble(AddressTable()) + enc_weth_withdraw(native_amount),
        1,
        sender=owner_account,
    )
    destination_before = destination.balance
    combined_before = weth.balanceOf(executor) + executor.balance

    tx = executor.sweep(destination.address, sender=owner_account)

    assert tx.status == 1
    assert weth.balanceOf(executor) == 0
    assert executor.balance == 0
    assert destination.balance - destination_before == combined_before


def test_sweep_is_owner_only(executor, accounts):
    with pytest.raises(Exception):
        executor.sweep(accounts[1].address, sender=accounts[2])


def test_min_profit_rejects_shortfall(executor, owner_account):
    stream = enc_preamble(AddressTable()) + enc_weth_deposit_all()

    with pytest.raises(Exception):
        executor.execute.call(stream, (1 << 32) | 1, sender=owner_account, gas=1_000_000)
