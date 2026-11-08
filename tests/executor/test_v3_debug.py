"""Test V3c pool with zfo=True through executor."""
import pytest
from .conftest_shared import Q96, _isqrt, enc_v3_swap_compact, enc_preamble, AddressTable

AMOUNT_WBTC = 100 * 10**8
AMOUNT_WETH_PROFIT = 2 * 10**18

def test_v3c_zfo(project, owner_account, wbtc, weth, executor, v4_pm):
    t0, t1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    v3_c = project.fake_uniswap_v3_pool.deploy(t0, t1, 0, 3000, sender=owner_account)
    zfo = v3_c.token0() == wbtc.address
    if zfo:
        price_scaled = AMOUNT_WETH_PROFIT * Q96 * Q96 * (10 ** 18)
        price_scaled = price_scaled // (AMOUNT_WBTC * (10 ** 8))
    else:
        price_scaled = AMOUNT_WBTC * Q96 * Q96 * (10 ** 8)
        price_scaled = price_scaled // (AMOUNT_WETH_PROFIT * (10 ** 18))
    sqrt_price_x96 = _isqrt(price_scaled)
    v3_c.initialize(sqrt_price_x96, sender=owner_account)
    wbtc.mint(v3_c.address, AMOUNT_WBTC * 100, sender=owner_account)
    weth.mint(v3_c.address, AMOUNT_WETH_PROFIT * 100, sender=owner_account)
    v3_c.add_liquidity(sender=owner_account)
    wbtc.mint(executor.address, AMOUNT_WBTC, sender=owner_account)
    at = AddressTable(weth_addr=weth.address, executor_addr=executor.address)
    v3c_idx = at.add(v3_c.address)
    owner_idx = at.add(owner_account.address)
    commands = enc_v3_swap_compact(v3c_idx, zfo, AMOUNT_WBTC, owner_idx)
    full = enc_preamble(at) + commands
    tx = executor.execute(full, 0, sender=owner_account, raise_on_revert=False)
    assert tx.status == 1, f"V3c zfo={zfo} swap failed!"
    print(f"SUCCESS! zfo={zfo}, Gas: {tx.gas_used}")
