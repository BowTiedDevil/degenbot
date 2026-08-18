"""V3-V4-V3 CurrencyNotSettled regression (on-chain, Ape + Foundry fakes).

Reproduces permutation row #17's ``CurrencyNotSettled()`` revert and proves the
fix (``V4_SWAP_DYNAMIC`` + ``V4_TAKE_DELTA`` + ``V4_SETTLE_ALL``) resolves it.

Root cause
----------
The V3-V4-V3 encoder used a *static* solver amount for the inner V4 swap
(``V4_SWAP_COMPACT(out_a)``) and a *static* take for the V4 output
(``V4_TAKE_COMPACT(out_b)``). V3a delivers its actual forward_a to the
PoolManager via V3's optimistic transfer, and the V4 swap computes the actual
forward_b on-chain. Whenever the on-chain amounts differ from the solver's
``out_a`` / ``out_b`` — common under real price impact — a residual PM delta
survives at unlock-end and the PoolManager raises ``CurrencyNotSettled``.

This harness forces the mismatch directly: the fake V4 pool produces MORE
forward_b than the solver's ``b_out`` (over-production), exactly the case that
leaves a positive residual forward_b delta with the old encoder.

  OLD: V4_SETTLE + V4_SWAP_COMPACT(a_out) + V4_TAKE_COMPACT(b_out)
       → residual = (b_out_actual - b_out) > 0 → CurrencyNotSettled (revert)
  NEW: V4_SETTLE + V4_SWAP_DYNAMIC + V4_TAKE_DELTA(forward_b→v3c) + V4_SETTLE_ALL
       → consumes the actual settled forward_a, takes the full actual forward_b,
         sweeps dust → no residual → succeeds

Note: the nesting is unchanged between OLD and NEW — V3's optimistic output
transfer delivers forward_a to PM before V3a's callback runs the V4 unlock, so
the unlock sees the deposit. The bug is the static amounts, not the nesting.
"""

import pytest

from .conftest_shared import (
    AddressTable,
    enc_erc20_transfer,
    enc_preamble,
    enc_v3_swap_compact,
    enc_v4_settle,
    enc_v4_settle_all,
    enc_v4_swap_compact,
    enc_v4_swap_dynamic,
    enc_v4_sync,
    enc_v4_take_compact,
    enc_v4_take_delta,
    enc_v4_unlock,
    _make_pool_key,
    _setup_v3,
    _setup_v4_swap,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_WBTC = 100 * 10**8
AMOUNT_WETH_PROFIT = 2 * 10**18
# Surplus the V4 swap produces beyond the solver's b_out. Simulates real price
# impact / stale reserves causing the on-chain V4 output to exceed the solver
# prediction — the residual forward_b delta that triggers CurrencyNotSettled
# under the old static-amount encoder.
WBTC_SURPLUS = 5 * 10**8


@pytest.fixture
def v3_a(project, owner_account, weth, usdc):
    t0, t1 = sorted([weth.address, usdc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(t0, t1, 0, 3000, sender=owner_account)


@pytest.fixture
def v3_c(project, owner_account, wbtc, weth):
    t0, t1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(t0, t1, 0, 3000, sender=owner_account)


def _setup_v4_usdc_wbtc(pm, owner, usdc, wbtc, amount_in, amount_out, fee, tick_spacing):
    """Configure the fake V4 USDC→WBTC pool; returns (pool_key, zfo)."""
    pool_key = _make_pool_key(usdc.address, wbtc.address, fee=fee, tick_spacing=tick_spacing)
    zfo = pool_key[0] == usdc.address  # USDC(currency0) → WBTC(currency1) when zfo
    _setup_v4_swap(pm, owner, pool_key, amount_in, amount_out, zfo, output_token=wbtc)
    return pool_key, zfo


class TestV3V4V3ResidualRegression:
    """Force V4 over-production; assert OLD reverts, NEW succeeds + profits."""

    def _setup_pools(self, v3_a, v3_c, weth, usdc, wbtc, v4_pm, owner):
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner)
        b_out_actual = AMOUNT_WBTC + WBTC_SURPLUS  # V4 produces MORE than solver's b_out
        b_pk, b_zfo = _setup_v4_usdc_wbtc(
            v4_pm, owner, usdc, wbtc, a_out, b_out_actual, fee=500, tick_spacing=10
        )
        b_out = AMOUNT_WBTC  # solver's prediction (V3c exact-input + old TAKE_COMPACT)
        c_zfo, _c_out = _setup_v3(v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner)
        return a_zfo, a_out, b_pk, b_zfo, b_out, b_out_actual, c_zfo

    def test_old_static_encoder_reverts_on_v4_overproduction(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v3_c
    ):
        a_zfo, a_out, b_pk, b_zfo, b_out, _b_actual, c_zfo = self._setup_pools(
            v3_a, v3_c, weth, usdc, wbtc, v4_pm, owner_account
        )
        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3c_idx = at.add(v3_c.address)
        v3a_idx = at.add(v3_a.address)
        pm_idx = at.add(v4_pm.address)

        # OLD: SETTLE + V4_SWAP_COMPACT(a_out) + V4_TAKE_COMPACT(b_out).
        # b_out_actual > b_out → residual forward_b delta survives → revert.
        v4_inner = enc_v4_settle()
        v4_inner += enc_v4_swap_compact(
            at.add(b_pk[0]), at.add(b_pk[1]), b_pk[2], b_pk[3], 0xFF, b_zfo, a_out
        )
        v4_inner += enc_v4_take_compact(wbtc_idx, v3c_idx, b_out)

        a_fwd = enc_erc20_transfer(weth_idx, v3a_idx, AMOUNT_WETH)
        a_fwd += enc_v4_unlock(v4_inner)
        c_fwd = enc_v3_swap_compact(v3a_idx, a_zfo, AMOUNT_WETH, pm_idx, forward_data=a_fwd)
        commands = enc_v4_sync(usdc_idx)
        commands += enc_v3_swap_compact(v3c_idx, c_zfo, b_out, executor_idx, forward_data=c_fwd)

        tx = executor.execute(
            enc_preamble(at) + commands, 0, sender=owner_account, raise_on_revert=False
        )
        assert tx.status == 0, (
            "OLD static-amount encoder must revert with CurrencyNotSettled when the "
            "V4 swap's actual output exceeds the solver's b_out (residual PM delta)."
        )

    def test_new_dynamic_encoder_succeeds_on_v4_overproduction(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v3_c
    ):
        a_zfo, a_out, b_pk, b_zfo, b_out, _b_actual, c_zfo = self._setup_pools(
            v3_a, v3_c, weth, usdc, wbtc, v4_pm, owner_account
        )
        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3c_idx = at.add(v3_c.address)
        v3a_idx = at.add(v3_a.address)
        pm_idx = at.add(v4_pm.address)

        # NEW: SETTLE + V4_SWAP_DYNAMIC + V4_TAKE_DELTA(forward_b→v3c) + V4_SETTLE_ALL.
        v4_inner = enc_v4_settle()
        v4_inner += enc_v4_swap_dynamic(
            at.add(b_pk[0]), at.add(b_pk[1]), b_pk[2], b_pk[3], 0xFF, b_zfo
        )
        v4_inner += enc_v4_take_delta(wbtc_idx, v3c_idx)
        v4_inner += enc_v4_settle_all()

        a_fwd = enc_erc20_transfer(weth_idx, v3a_idx, AMOUNT_WETH)
        a_fwd += enc_v4_unlock(v4_inner)
        c_fwd = enc_v3_swap_compact(v3a_idx, a_zfo, AMOUNT_WETH, pm_idx, forward_data=a_fwd)
        commands = enc_v4_sync(usdc_idx)
        commands += enc_v3_swap_compact(v3c_idx, c_zfo, b_out, executor_idx, forward_data=c_fwd)

        weth_before = weth.balanceOf(executor)
        tx = run_executor(at, commands, owner_account)  # default: WETH+ETH profit check
        assert tx.status == 1, "NEW dynamic/delta encoder must succeed under V4 over-production"
        weth_after = weth.balanceOf(executor)
        profit = weth_after - weth_before
        assert profit > 0, (
            f"Executor must profit (V3c WETH output minus V3a WETH input); got profit={profit}"
        )
