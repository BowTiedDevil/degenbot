"""
Test that fake contracts emit events correctly, and verify transfer counting.

Events added:
- fake_erc20: Transfer, Approval
- fake_weth: Transfer (deposit/withdraw)
- fake_uniswap_v2_pair: Swap, Sync
- fake_uniswap_v3_pool: Swap
- fake_uniswap_v4_pool_manager: IPoolManager.Swap, IERC6909Claims.Transfer
"""

import pytest
from .verify import count_transfers, summarize_events
from .conftest_shared import v2_get_amount_out

V2_FEE = 30  # 0.3%


def test_erc20_transfer_event(project, owner_account, usdc, accounts):
    """ERC20.transfer() emits Transfer(sender, to, value)."""
    tx = usdc.transfer(accounts[1], 1000, sender=owner_account)
    assert tx.status == 1
    assert count_transfers(tx) == 1


def test_erc20_mint_event(project, owner_account, usdc, accounts):
    """ERC20.mint() emits Transfer(zero, to, value)."""
    tx = usdc.mint(accounts[1], 1000, sender=owner_account)
    assert tx.status == 1
    assert count_transfers(tx) == 1


def test_erc20_burn_event(project, owner_account, usdc):
    """ERC20.burn() emits Transfer(sender, zero, value)."""
    tx = usdc.burn(1000, sender=owner_account)
    assert tx.status == 1
    assert count_transfers(tx) == 1


def test_weth_deposit_event(project, owner_account, weth):
    """WETH.deposit() emits Transfer(zero, user, amount)."""
    tx = weth.deposit(value=10**18, sender=owner_account)
    assert tx.status == 1
    assert count_transfers(tx) == 1


def test_weth_withdraw_event(project, owner_account, weth):
    """WETH.withdraw() emits Transfer(user, zero, amount)."""
    weth.deposit(value=10**18, sender=owner_account)
    tx = weth.withdraw(10**18, sender=owner_account)
    assert tx.status == 1
    assert count_transfers(tx) == 1


def test_v2_swap_events(project, owner_account, weth, usdc):
    """V2 swap emits Transfer (output) + Sync + Swap.

    Fund pair, transfer input as excess balance, call swap(data=b"").
    K-invariant enforced by the pair — no set_next_swap needed.
    """
    v2 = project.fake_uniswap_v2_pair.deploy(
        weth.address, usdc.address, 0, V2_FEE, sender=owner_account
    )
    usdc.mint(v2.address, 2000 * 10**6, sender=owner_account)
    weth.mint(v2.address, 10 * 10**18, sender=owner_account)
    v2.sync(sender=owner_account)

    # Swap ~10% of WETH reserve
    amount_in = 1 * 10**18
    amount_out = v2_get_amount_out(
        amount_in, weth.balanceOf(v2.address), usdc.balanceOf(v2.address), V2_FEE
    )

    # Pay input to pair (excess balance), then swap with no callback
    weth.transfer(v2.address, amount_in, sender=owner_account)
    tx = v2.swap(0, amount_out, owner_account.address, b"", sender=owner_account)
    assert tx.status == 1

    events = summarize_events(tx)
    # 1 Transfer (USDC pair→owner), 1 Sync, 1 Swap
    assert events["Transfer(raw)"] == 1
    assert events["Sync"] == 1
    assert events["Swap"] == 1
