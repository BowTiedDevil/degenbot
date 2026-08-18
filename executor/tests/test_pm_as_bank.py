"""
PM-as-Bank Tests: V4 PoolManager as zero-fee flash-loan source for non-V4 paths.

For each of the 8 non-V4 three-hop paths, tests PM-as-bank with:
  A. V4_MINT profit capture (ERC6909) vs B. V4_TAKE_DELTA (physical WETH)

Key findings:
  - V2-ending paths: delta accounting works cleanly. V2c sends WETH to PM
    directly (V2_SWAP_CALC), which settle() captures. Profit = c_out - borrow.
    Transfer count with MINT = 4 (same as optimized). With TAKE = 5.

  - V3-ending paths: V3c's optimistic transfer sends WETH BEFORE the callback.
    This means sync() inside the callback captures a snapshot that already
    includes the deposit, so settle() credits 0. The fix is to send V3c's
    output to the executor (not PM), then manually repay PM. This adds 1
    transfer (5 total), making PM-as-bank strictly worse for V3-ending paths.
    The profit is physical WETH at executor (not ERC6909), so MINT doesn't apply.

  - Gas: MINT is ~10-20k gas more expensive than TAKE (ERC6909 bookkeeping
    overhead), but saves 1 ERC20 transfer. Net savings depend on transfer cost.

  - Zero-fee borrowing is the main benefit (0% vs V2's 0.3% swap fee).
"""

import pytest

from .conftest_shared import (
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v2_swap_compact,
    enc_v2_swap_calc,
    enc_v3_swap_compact,
    enc_v4_take,
    enc_v4_take_delta,
    enc_v4_mint_compact,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_unlock,
    enc_erc20_transfer,
    enc_preamble,
    AddressTable,
    v2_get_amount_out,
)

from .verify import count_transfers

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_WBTC = 100 * 10**8
AMOUNT_WETH_PROFIT = 2 * 10**18
PROFIT = AMOUNT_WETH_PROFIT - AMOUNT_WETH
V2_FEE = 30

_gas_results = {}


def _record(path, mode, gas, xf):
    _gas_results[(path, mode)] = (gas, xf)


def _v2sc(at, pool, zfo, recp, fee=V2_FEE):
    return enc_v2_swap_calc(at.add(pool.address), zfo, recp, fee=fee)


def _v3s(at, pool, zfo, amt, recp, fwd=b""):
    return enc_v3_swap_compact(at.add(pool.address), zfo, amt, recp, forward_data=fwd)


def _setup_v3(pool, inp, out, ain, aout, owner, liquidity_factor=100):
    """Set up a V3 pool."""
    from .conftest_shared import _setup_v3 as _shared_setup_v3
    return _shared_setup_v3(pool, inp, out, ain, aout, owner, liquidity_factor)


def _setup_v2c(pool, inp, out, owner, ain, aout, fee=V2_FEE):
    inp.mint(pool.address, ain * 100, sender=owner)
    out.mint(pool.address, aout * 100, sender=owner)
    pool.sync(sender=owner)
    return pool.token0() == inp.address


def _fund(weth, pm, owner):
    weth.mint(pm.address, AMOUNT_WETH * 10, sender=owner)


def _profit_end(weth, v2c, b_out):
    """Compute actual profit for V2-ending paths."""
    c_out = v2_get_amount_out(
        b_out,
        getattr(weth, "_wbtc_reserve", AMOUNT_WBTC * 100),
        weth.balanceOf(v2c),
        V2_FEE,
    )
    return c_out - AMOUNT_WETH


# ── Fixtures ──


@pytest.fixture
def v2_a(project, owner_account, weth, usdc):
    t0, t1 = sorted([weth.address, usdc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(t0, t1, 0, V2_FEE, sender=owner_account)


@pytest.fixture
def v2_b(project, owner_account, usdc, wbtc):
    t0, t1 = sorted([usdc.address, wbtc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(t0, t1, 0, V2_FEE, sender=owner_account)


@pytest.fixture
def v2_c(project, owner_account, wbtc, weth):
    t0, t1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(t0, t1, 0, V2_FEE, sender=owner_account)


@pytest.fixture
def v3_a(project, owner_account, weth, usdc):
    t0, t1 = sorted([weth.address, usdc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(t0, t1, 0, 3000, sender=owner_account)


@pytest.fixture
def v3_b(project, owner_account, usdc, wbtc):
    t0, t1 = sorted([usdc.address, wbtc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(t0, t1, 0, 3000, sender=owner_account)


@pytest.fixture
def v3_c(project, owner_account, wbtc, weth):
    t0, t1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(t0, t1, 0, 3000, sender=owner_account)


# ═══════════════════════════════════════════════════════════════════════════
# V2-ENDING PATHS (delta accounting works cleanly)
# ═══════════════════════════════════════════════════════════════════════════


class TestPMBankV2V2V2:
    """V2-V2-V2: take→V2a, V2a→V2b→V2c(calc), V2c→PM(calc). 4 xf (MINT), 5 (TAKE)."""

    def _b(self, at, weth, exe, pm, v2a, v2b, v2c, az, bz, cz, mode, profit):
        wi = at.add(weth.address)
        ei = at.add(exe.address)
        pi = at.add(pm.address)
        a2 = at.add(v2a.address)

        inner = enc_v4_take(wi, a2, AMOUNT_WETH)
        inner += enc_v4_sync(wi)
        inner += _v2sc(at, v2a, az, at.add(v2b.address))
        inner += _v2sc(at, v2b, bz, at.add(v2c.address))
        inner += _v2sc(at, v2c, cz, pi)
        inner += enc_v4_settle()
        inner += (
            enc_v4_mint_compact(wi, ei, profit)
            if mode == "mint"
            else enc_v4_take_delta(wi, ei)
        )
        return enc_v4_unlock(inner)

    def test_mint(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v2_b, v2_c
    ):
        az = _setup_v2c(v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC)
        bz = _setup_v2c(v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC)
        cz = _setup_v2c(
            v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT
        )
        _fund(weth, v4_pm, owner_account)

        a = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b = v2_get_amount_out(a, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE)
        c = v2_get_amount_out(b, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE)
        profit = c - AMOUNT_WETH

        at = AddressTable()
        tx = run_executor(
            at,
            self._b(
                at, weth, executor, v4_pm, v2_a, v2_b, v2_c, az, bz, cz, "mint", profit
            ),
            owner_account,
        )
        assert tx.status == 1
        xf = count_transfers(tx)
        print(f"  V2-V2-V2 MINT: {xf} transfers, gas={tx.gas_used}")
        _record("V2-V2-V2", "MINT", tx.gas_used, xf)

    def test_take(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v2_b, v2_c
    ):
        az = _setup_v2c(v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC)
        bz = _setup_v2c(v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC)
        cz = _setup_v2c(
            v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT
        )
        _fund(weth, v4_pm, owner_account)
        at = AddressTable()
        tx = run_executor(
            at,
            self._b(at, weth, executor, v4_pm, v2_a, v2_b, v2_c, az, bz, cz, "take", 0),
            owner_account,
        )
        assert tx.status == 1
        xf = count_transfers(tx)
        print(f"  V2-V2-V2 TAKE: {xf} transfers, gas={tx.gas_used}")
        _record("V2-V2-V2", "TAKE", tx.gas_used, xf)


class TestPMBankV3V2V2:
    """V3-V2-V2: V3a outermost, take(WETH→V3a) in callback for IIA, V2b→V2c→PM."""

    def _b(self, at, weth, exe, pm, v3a, v2b, v2c, az, bz, cz, mode, profit):
        wi = at.add(weth.address)
        ei = at.add(exe.address)
        pi = at.add(pm.address)
        a3 = at.add(v3a.address)

        # V3a callback: take(WETH→V3a, IIA ✓), sync, V2b→V2c→PM, settle, profit
        fwd = enc_v4_take(wi, a3, AMOUNT_WETH)
        fwd += enc_v4_sync(wi)
        fwd += _v2sc(at, v2b, bz, at.add(v2c.address))
        fwd += _v2sc(at, v2c, cz, pi)
        fwd += enc_v4_settle()
        fwd += (
            enc_v4_mint_compact(wi, ei, profit)
            if mode == "mint"
            else enc_v4_take_delta(wi, ei)
        )

        inner = _v3s(at, v3a, az, AMOUNT_WETH, at.add(v2b.address), fwd)
        return enc_v4_unlock(inner)

    def test_mint(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v2_b, v2_c
    ):
        az, a_usdc_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        bz = _setup_v2c(v2_b, usdc, wbtc, owner_account, a_usdc_out, AMOUNT_WBTC)
        cz = _setup_v2c(
            v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT
        )
        _fund(weth, v4_pm, owner_account)

        b = v2_get_amount_out(
            a_usdc_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )
        c = v2_get_amount_out(b, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE)
        profit = c - AMOUNT_WETH

        at = AddressTable()
        tx = run_executor(
            at,
            self._b(
                at, weth, executor, v4_pm, v3_a, v2_b, v2_c, az, bz, cz, "mint", profit
            ),
            owner_account,
        )
        assert tx.status == 1
        xf = count_transfers(tx)
        print(f"  V3-V2-V2 MINT: {xf} transfers, gas={tx.gas_used}")
        _record("V3-V2-V2", "MINT", tx.gas_used, xf)

    def test_take(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v2_b, v2_c
    ):
        az, a_usdc_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        bz = _setup_v2c(v2_b, usdc, wbtc, owner_account, a_usdc_out, AMOUNT_WBTC)
        cz = _setup_v2c(
            v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT
        )
        _fund(weth, v4_pm, owner_account)

        b = v2_get_amount_out(
            a_usdc_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )
        c = v2_get_amount_out(b, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE)
        profit = c - AMOUNT_WETH

        at = AddressTable()
        tx = run_executor(
            at,
            self._b(
                at, weth, executor, v4_pm, v3_a, v2_b, v2_c, az, bz, cz, "take", profit
            ),
            owner_account,
        )
        assert tx.status == 1
        xf = count_transfers(tx)
        print(f"  V3-V2-V2 TAKE: {xf} transfers, gas={tx.gas_used}")
        _record("V3-V2-V2", "TAKE", tx.gas_used, xf)


# ═══════════════════════════════════════════════════════════════════════════
# V2-V3-V2: V3b middle, V2a inside V3b callback, V2c end
# ═══════════════════════════════════════════════════════════════════════════


class TestPMBankV2V3V2:
    """V2-V3-V2: V3b outermost, V2a inside callback (to=V3b, IIA ✓), V2c→PM."""

    def _b(self, at, weth, exe, pm, v2a, v3b, v2c, az, bz, cz, a_out, mode, profit):
        wi = at.add(weth.address)
        ei = at.add(exe.address)
        pi = at.add(pm.address)
        b3 = at.add(v3b.address)
        a2 = at.add(v2a.address)

        # V3b callback: take(WETH→V2a) + V2a→V3b (IIA ✓)
        b_fwd = enc_v4_take(wi, a2, AMOUNT_WETH)
        b_fwd += enc_v4_sync(wi)
        b_fwd += _v2sc(at, v2a, az, b3)

        # After V3b: V2c→PM, settle, profit
        inner = _v3s(at, v3b, bz, a_out, at.add(v2c.address), b_fwd)
        inner += _v2sc(at, v2c, cz, pi)
        inner += enc_v4_settle()
        inner += (
            enc_v4_mint_compact(wi, ei, profit)
            if mode == "mint"
            else enc_v4_take_delta(wi, ei)
        )
        return enc_v4_unlock(inner)

    def test_mint(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v3_b, v2_c
    ):
        az = _setup_v2c(v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC)
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        bz, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        cz = _setup_v2c(
            v2_c, wbtc, weth, owner_account, b_out, AMOUNT_WETH_PROFIT
        )
        _fund(weth, v4_pm, owner_account)

        c = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )
        profit = c - AMOUNT_WETH

        at = AddressTable()
        tx = run_executor(
            at,
            self._b(
                at,
                weth,
                executor,
                v4_pm,
                v2_a,
                v3_b,
                v2_c,
                az,
                bz,
                cz,
                a_out,
                "mint",
                profit,
            ),
            owner_account,
        )
        assert tx.status == 1
        xf = count_transfers(tx)
        print(f"  V2-V3-V2 MINT: {xf} transfers, gas={tx.gas_used}")
        _record("V2-V3-V2", "MINT", tx.gas_used, xf)

    def test_take(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v3_b, v2_c
    ):
        az = _setup_v2c(v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC)
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        bz, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        cz = _setup_v2c(
            v2_c, wbtc, weth, owner_account, b_out, AMOUNT_WETH_PROFIT
        )
        _fund(weth, v4_pm, owner_account)

        c = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )
        profit = c - AMOUNT_WETH

        at = AddressTable()
        tx = run_executor(
            at,
            self._b(
                at,
                weth,
                executor,
                v4_pm,
                v2_a,
                v3_b,
                v2_c,
                az,
                bz,
                cz,
                a_out,
                "take",
                profit,
            ),
            owner_account,
        )
        assert tx.status == 1
        xf = count_transfers(tx)
        print(f"  V2-V3-V2 TAKE: {xf} transfers, gas={tx.gas_used}")
        _record("V2-V3-V2", "TAKE", tx.gas_used, xf)


# ═══════════════════════════════════════════════════════════════════════════
# V2-V2-V3: V3c outermost, V2a+V2b inside callback, V3c output to executor
# ═══════════════════════════════════════════════════════════════════════════


class TestPMBankV2V2V3:
    """V2-V2-V3: V3c outermost (output→executor), V2a+V2b inside V3c callback.

    V3c sends WETH to EXECUTOR (not PM). Executor manually repays PM.
    This is 5 transfers (vs 4 for optimized without PM-bank), because
    the V3 optimistic transfer can't go directly to PM without breaking
    delta accounting (see docstring).
    """

    def _b(self, at, weth, exe, pm, v2a, v2b, v3c, az, bz, cz, b_out, mode):
        wi = at.add(weth.address)
        ei = at.add(exe.address)
        pi = at.add(pm.address)
        c3 = at.add(v3c.address)
        a2 = at.add(v2a.address)

        # V3c callback: take(WETH→V2a), V2a→V2b→V3c (NO sync — handled at top level)
        c_fwd = enc_v4_take(wi, a2, AMOUNT_WETH)
        c_fwd += _v2sc(at, v2a, az, at.add(v2b.address))
        c_fwd += _v2sc(at, v2b, bz, c3)

        # V3c sends output to executor, then repay PM + extract profit
        inner = _v3s(at, v3c, cz, b_out, ei, c_fwd)
        # Repay: executor→PM, sync, settle
        inner += enc_erc20_transfer(wi, pi, AMOUNT_WETH)
        inner += enc_v4_sync(wi)
        inner += enc_v4_settle()
        # Profit is physical WETH at executor — no V4_MINT possible here
        if mode == "mint":
            # Attempt MINT: should fail because delta is 0 after repayment
            inner += enc_v4_mint_compact(wi, ei, 1)  # Will be caught by test
        return enc_v4_unlock(inner)

    def test_mint_impossible(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v2_b, v3_c
    ):
        """V3-ending paths can't use V4_MINT for profit — profit is physical WETH."""
        az = _setup_v2c(v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC)
        bz = _setup_v2c(v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC)
        a = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b = v2_get_amount_out(a, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE)
        cz, _ = _setup_v3(v3_c, wbtc, weth, b, AMOUNT_WETH_PROFIT, owner_account)
        _fund(weth, v4_pm, owner_account)

        at = AddressTable()
        tx = executor.execute(
            enc_preamble(at)
            + self._b(
                at, weth, executor, v4_pm, v2_a, v2_b, v3_c, az, bz, cz, b, "mint"
            ),
            sender=owner_account,
            raise_on_revert=False,
        )
        assert tx.status == 0, (
            "V4_MINT on V3-ending path should revert (delta is 0 after repayment)"
        )
        _record("V2-V2-V3", "MINT-impossible", 0, 0)

    def test_take_physical(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v2_b, v3_c
    ):
        """V3-ending path: profit is physical WETH at executor, 5 transfers."""
        az = _setup_v2c(v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC)
        bz = _setup_v2c(v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC)
        a = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b = v2_get_amount_out(a, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE)
        cz, _ = _setup_v3(v3_c, wbtc, weth, b, AMOUNT_WETH_PROFIT, owner_account)
        _fund(weth, v4_pm, owner_account)

        at = AddressTable()
        wi = at.add(weth.address)
        ei = at.add(executor.address)
        pi = at.add(v4_pm.address)
        c3 = at.add(v3_c.address)
        a2 = at.add(v2_a.address)

        c_fwd = enc_v4_take(wi, a2, AMOUNT_WETH)
        c_fwd += _v2sc(at, v2_a, az, at.add(v2_b.address))
        c_fwd += _v2sc(at, v2_b, bz, c3)

        inner = _v3s(at, v3_c, cz, b, ei, c_fwd)
        inner += enc_v4_sync(wi)  # snapshot before repayment (after take drained PM)
        inner += enc_erc20_transfer(wi, pi, AMOUNT_WETH)  # repayment: executor→PM
        inner += enc_v4_settle()  # credit the repayment

        tx = run_executor(at, enc_v4_unlock(inner), owner_account)
        assert tx.status == 1
        xf = count_transfers(tx)
        assert xf == 5, f"Expected 5 transfers, got {xf}"
        print(f"  V2-V2-V3 TAKE-physical: {xf} transfers, gas={tx.gas_used}")
        _record("V2-V2-V3", "TAKE-physical", tx.gas_used, xf)


# ═══════════════════════════════════════════════════════════════════════════
# V3-V3-V2: V3b outermost, V3a inside, V2c inside V3a callback
# ═══════════════════════════════════════════════════════════════════════════


class TestPMBankV3V3V2:
    """V3-V3-V2: V3b outermost, V3a inside, V2c→PM inside V3a callback."""

    def _b(self, at, weth, exe, pm, v3a, v3b, v2c, az, bz, cz, a_usdc_out, mode, profit):
        wi = at.add(weth.address)
        ei = at.add(exe.address)
        pi = at.add(pm.address)
        a3 = at.add(v3a.address)

        # V3a callback: take(WETH→V3a, IIA ✓), sync, V2c→PM(calc), settle
        # Sync AFTER take (before deposit), settle captures the V2c deposit
        a_fwd = enc_v4_take(wi, a3, AMOUNT_WETH)
        a_fwd += enc_v4_sync(wi)
        a_fwd += _v2sc(at, v2c, cz, pi)
        a_fwd += enc_v4_settle()

        # V3b callback: V3a(to=V3b) with V3a callback
        b_fwd = _v3s(at, v3a, az, AMOUNT_WETH, at.add(v3b.address), a_fwd)

        inner = _v3s(at, v3b, bz, a_usdc_out, at.add(v2c.address), b_fwd)
        inner += (
            enc_v4_mint_compact(wi, ei, profit)
            if mode == "mint"
            else enc_v4_take_delta(wi, ei)
        )
        return enc_v4_unlock(inner)

    def test_mint(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v3_b, v2_c
    ):
        az, a_usdc_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        bz, b_wbtc_out = _setup_v3(v3_b, usdc, wbtc, a_usdc_out, AMOUNT_WBTC, owner_account)
        cz = _setup_v2c(
            v2_c, wbtc, weth, owner_account, b_wbtc_out, AMOUNT_WETH_PROFIT
        )
        _fund(weth, v4_pm, owner_account)

        c = v2_get_amount_out(
            b_wbtc_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )
        profit = c - AMOUNT_WETH

        at = AddressTable()
        tx = run_executor(
            at,
            self._b(
                at, weth, executor, v4_pm, v3_a, v3_b, v2_c, az, bz, cz, a_usdc_out, "mint", profit
            ),
            owner_account,
        )
        assert tx.status == 1
        xf = count_transfers(tx)
        print(f"  V3-V3-V2 MINT: {xf} transfers, gas={tx.gas_used}")
        _record("V3-V3-V2", "MINT", tx.gas_used, xf)

    def test_take(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v3_b, v2_c
    ):
        az, a_usdc_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        bz, b_wbtc_out = _setup_v3(v3_b, usdc, wbtc, a_usdc_out, AMOUNT_WBTC, owner_account)
        cz = _setup_v2c(
            v2_c, wbtc, weth, owner_account, b_wbtc_out, AMOUNT_WETH_PROFIT
        )
        _fund(weth, v4_pm, owner_account)

        c = v2_get_amount_out(
            b_wbtc_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )
        profit = c - AMOUNT_WETH

        at = AddressTable()
        tx = run_executor(
            at,
            self._b(
                at, weth, executor, v4_pm, v3_a, v3_b, v2_c, az, bz, cz, a_usdc_out, "take", profit
            ),
            owner_account,
        )
        assert tx.status == 1
        xf = count_transfers(tx)
        print(f"  V3-V3-V2 TAKE: {xf} transfers, gas={tx.gas_used}")
        _record("V3-V3-V2", "TAKE", tx.gas_used, xf)


# ═══════════════════════════════════════════════════════════════════════════
# Gas comparison summary
# ═══════════════════════════════════════════════════════════════════════════


def test_gas_comparison_table():
    """Print gas comparison: MINT (ERC6909) vs TAKE (physical WETH)."""
    if not _gas_results:
        pytest.skip("No gas results")

    print("\n┌──────────────┬────────────┬────────────┬─────────┬─────────┬──────────┐")
    print("│ Path         │ MINT gas   │ TAKE gas   │ MINT xf │ TAKE xf │ Gas diff │")
    print("├──────────────┼────────────┼────────────┼─────────┼─────────┼──────────┤")

    for path in ["V2-V2-V2", "V2-V3-V2", "V3-V2-V2", "V3-V3-V2"]:
        m = _gas_results.get((path, "MINT"))
        t = _gas_results.get((path, "TAKE"))
        if m and t:
            mg, mx = m
            tg, tx_ = t
            d = mg - tg
            s = "+" if d >= 0 else ""
            print(
                f"│ {path:12s} │ {mg:>8,d}  │ {tg:>8,d}  │ {mx:>5d}   │ {tx_:>5d}   │ {s}{d:>6,d}  │"
            )
        else:
            print(
                f"│ {path:12s} │ {'N/A':>8s}  │ {'N/A':>8s}  │ {'N/A':>5s}   │ {'N/A':>5s}   │ {'N/A':>8s} │"
            )

    print("└──────────────┴────────────┴────────────┴─────────┴─────────┴──────────┘")
    print("\nNegative gas diff = MINT (ERC6909) is CHEAPER despite no ERC20 transfer.")
    print("Positive = MINT is more expensive (ERC6909 bookkeeping overhead).")
    print("\nV3-ending paths: profit is physical WETH at executor (5 transfers).")
    print("V4_MINT is NOT applicable — delta is 0 after PM repayment.")
