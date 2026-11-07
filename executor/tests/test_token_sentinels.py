import pytest

from .conftest_shared import (
    AddressTable,
    ZERO_ADDRESS,
    _make_pool_key,
    _setup_v4_swap,
    enc_preamble,
    enc_v4_settle_all,
    enc_v4_swap_compact,
    enc_v4_take_delta,
    enc_v4_unlock,
)

SELF = 0xFD


@pytest.mark.parametrize("currency_idx", [0xFA, 0xFB])
def test_token_sentinels_resolve_without_table_entries(currency_idx, owner_account, executor):
    commands = enc_v4_unlock(enc_v4_take_delta(currency_idx, SELF))
    tx = executor.execute(enc_preamble(AddressTable()) + commands, sender=owner_account)
    assert tx.status == 1


@pytest.mark.parametrize("currency_idx,fixture_name", [(0xFA, "usdc"), (0xFB, "wbtc")])
def test_settle_all_includes_stablecoin_sentinels(
    currency_idx, fixture_name, request, usdc, wbtc, weth, owner_account, executor, v4_pm
):
    token = request.getfixturevalue(fixture_name)
    amount_in = 10**18
    amount_out = 2_000_000_000
    pool_key = _make_pool_key(weth.address, token.address, fee=3000, tick_spacing=60)
    zfo = pool_key[0] == weth.address
    _setup_v4_swap(
        v4_pm, owner_account, pool_key, amount_in, amount_out, zfo, output_token=token
    )
    weth.mint(executor.address, amount_in, sender=owner_account)

    at = AddressTable(user0_addr=usdc.address, user1_addr=wbtc.address)
    weth_idx = at.add(weth.address)
    token_idx = at.add(token.address)
    hooks_idx = at.add(ZERO_ADDRESS)
    inner = enc_v4_swap_compact(
        weth_idx if pool_key[0] == weth.address else token_idx,
        token_idx if pool_key[1] == token.address else weth_idx,
        pool_key[2], pool_key[3], hooks_idx, zfo, amount_in,
    )
    inner += enc_v4_settle_all()

    tx = executor.execute(enc_preamble(at) + enc_v4_unlock(inner), sender=owner_account)
    assert tx.status == 1
    assert token.balanceOf(executor.address) == amount_out


def test_unassigned_reserved_index_reverts(owner_account, executor):
    commands = enc_v4_unlock(enc_v4_take_delta(0xF9, SELF))
    with pytest.raises(Exception):
        executor.execute(enc_preamble(AddressTable()) + commands, sender=owner_account)
