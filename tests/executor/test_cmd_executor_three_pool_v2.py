"""
Tests for cmd_executor three-pool V2 triangular arbitrage.

Scenario:
  Token A: WETH (18 decimals)
  Token B: USDC (6 decimals)
  Token C: WBTC (8 decimals)

  Pool A: WETH/USDC — sell WETH for USDC  (zero_for_one when WETH == token0)
  Pool B: USDC/WBTC — sell USDC for WBTC  (zero_for_one when USDC == token0)
  Pool C: WBTC/WETH — sell WBTC for WETH  (zero_for_one when WBTC == token0)

  Arbitrage path: WETH → USDC (A) → WBTC (B) → WETH (C)

  V2 flash swap: each pair sends output tokens optimistically, then calls
  the executor's callback. The callback must pay the pair its owed INPUT
  tokens before returning, or the K-invariant check will fail.

  - Pool A receives USDC output, owes WETH input to Pool A.
  - Pool B receives WBTC output, owes USDC input to Pool B.
  - Pool C receives WETH output, owes WBTC input to Pool C.

Three V2 swap methods are compared:

  1. V2_SWAP_COMPACT nested callbacks — full executor custody
     Every pool sends output to executor, each callback pays next pool.
     3 callbacks, 6 ERC-20 transfers (3 pool→exec + 3 exec→pool).

  2. V2_SWAP_COMPACT flash + 2× V2_SWAP_CALC — direct custody for B & C
     Pool A sends USDC directly to Pool B (recipient = Pool B). Pool B
     accumulates USDC as excess balance; V2_SWAP_CALC reads it as input
     and sends WBTC directly to Pool C (recipient = Pool C). Pool C
     accumulates WBTC as excess balance; V2_SWAP_CALC reads it as input
     and sends WETH to executor. Only Pool A needs a callback.
     1 callback, 4 ERC-20 transfers.

  3. All V2_SWAP_CALC — zero callbacks
     Executor pre-funds Pool A with WETH (creating excess balance), then
     each pool's excess-balance input feeds the next pool's direct output.
     0 callbacks, 4 ERC-20 transfers (exec→A + A→B + B→C + C→exec).

Optimal: Approach 2 (V2_SWAP_COMPACT flash + 2× V2_SWAP_CALC).
  - Same 4 transfers as approach 3 but no pre-funding needed — the
    executor "borrows" via flash, avoiding the WETH balance requirement.
  - 1 callback instead of 3 (saves callback dispatch + forward_data overhead).
  - Direct custody skips 2 intermediate executor transfers vs approach 1.

Key insight: V2 pairs send output optimistically to any `recipient`. When
the recipient is the next pool in the chain, the receiving pair accumulates
excess balance. V2_SWAP_CALC reads that excess as swap input — no executor
custody, no callback, no extra transfer. The only token that must flow
through the executor is the flash-repayment token (WETH), which the
executor receives from the last pool and forwards to the first pool
within the same callback.
"""

import pytest
from .conftest_shared import (
    enc_v2_swap_compact,
    enc_v2_swap_calc,
    enc_erc20_transfer,
    AddressTable,
    enc_preamble,
)

# ── V2 constant-product helpers ──


def v2_get_amount_out(amount_in, reserve_in, reserve_out, fee):
    """Compute V2 swap output using the (10000 - fee) formula.

    Matches cmd_executor._v2_get_amount_out:
        feeMultiplier = 10000 - fee
        amountOut = (amountIn * feeMultiplier * reserveOut) / (reserveIn * 10000 + amountIn * feeMultiplier)
    """
    fm = 10000 - fee
    return (amount_in * fm * reserve_out) // (reserve_in * 10000 + amount_in * fm)


# ── Fixtures ──

FEE = 30  # 0.3%

# Liquidity amounts per pool.
# Pool C has more WETH than the fair cross-rate implies, creating an
# arbitrage: WETH→USDC (A) → WBTC (B) → WETH (C) yields more WETH out.
#
# Cross-rate consistency check (before fees):
#   A rate: 2K WETH / 4M USDC = 2000 USDC/WETH
#   B rate: 4M USDC / 100K WBTC = 40 USDC/WBTC
#   Implied C: 2000 / 40 = 50 WBTC/WETH
#   Actual C: 2.2K WETH / 100K WBTC ≈ 45.45 WBTC/WETH
#   → WETH is cheap in Pool C (fewer WBTC needed per WETH) = arb opportunity
AMOUNT_WETH_LIQ_A = 2_000 * 10**18  # Pool A: 2K WETH
AMOUNT_USDC_LIQ_A = 4_000_000 * 10**6  # Pool A: 4M USDC
AMOUNT_USDC_LIQ_B = 4_000_000 * 10**6  # Pool B: 4M USDC
AMOUNT_WBTC_LIQ_B = 100_000 * 10**8  # Pool B: 100K WBTC
AMOUNT_WBTC_LIQ_C = 100_000 * 10**8  # Pool C: 100K WBTC
AMOUNT_WETH_LIQ_C = 2_200 * 10**18  # Pool C: 2.2K WETH (mispriced)

# Trade size into Pool A
AMOUNT_WETH_IN = 1 * 10**18  # 1 WETH


@pytest.fixture
def pool_a(project, owner_account, weth, usdc):
    """WETH/USDC pool — sell WETH for USDC."""
    token0, token1 = sorted([weth.address, usdc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, FEE, sender=owner_account
    )


@pytest.fixture
def pool_b(project, owner_account, usdc, wbtc):
    """USDC/WBTC pool — sell USDC for WBTC."""
    token0, token1 = sorted([usdc.address, wbtc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, FEE, sender=owner_account
    )


@pytest.fixture
def pool_c(project, owner_account, wbtc, weth):
    """WBTC/WETH pool — sell WBTC for WETH."""
    token0, token1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, FEE, sender=owner_account
    )


def _fund_pools(pool_a, pool_b, pool_c, weth, usdc, wbtc, owner):
    """Provide liquidity to all three pools and sync reserves.

    Pool C is mispriced: it holds more WETH than the cross-rate from
    pools A and B implies, creating a triangular arbitrage opportunity.
    """
    # Pool A: WETH / USDC (2000 USDC per WETH)
    usdc.mint(pool_a.address, AMOUNT_USDC_LIQ_A, sender=owner)
    weth.mint(pool_a.address, AMOUNT_WETH_LIQ_A, sender=owner)
    pool_a.sync(sender=owner)

    # Pool B: USDC / WBTC (40 USDC per WBTC)
    usdc.mint(pool_b.address, AMOUNT_USDC_LIQ_B, sender=owner)
    wbtc.mint(pool_b.address, AMOUNT_WBTC_LIQ_B, sender=owner)
    pool_b.sync(sender=owner)

    # Pool C: WBTC / WETH (mispriced — cheap WETH, ~45.5 WBTC/WETH instead of 50)
    wbtc.mint(pool_c.address, AMOUNT_WBTC_LIQ_C, sender=owner)
    weth.mint(pool_c.address, AMOUNT_WETH_LIQ_C, sender=owner)
    pool_c.sync(sender=owner)


def _compute_amounts(pool_a, pool_b, pool_c, weth, usdc, wbtc, weth_in):
    """Compute expected swap amounts for the triangular arbitrage A→B→C.

    Zero-for-one (zfo) determines which token is sold:
      zfo=True:  selling token0 → input reserves = token0, output reserves = token1
      zfo=False: selling token1 → input reserves = token1, output reserves = token0

    Returns (amount_usdc, amount_wbtc, amount_weth_out, zfo_a, zfo_b, zfo_c).
    """
    zfo_a = pool_a.token0() == weth.address  # sell WETH?, get USDC
    zfo_b = pool_b.token0() == usdc.address  # sell USDC?, get WBTC
    zfo_c = pool_c.token0() == wbtc.address  # sell WBTC?, get WETH

    # Pool A: sell WETH for USDC
    # Input reserves are always the token being sold (WETH),
    # output reserves are always the token being bought (USDC).
    # zfo only determines which direction the V2 pair's swap() goes.
    res_a_in = weth.balanceOf(pool_a.address)
    res_a_out = usdc.balanceOf(pool_a.address)
    amount_usdc = v2_get_amount_out(weth_in, res_a_in, res_a_out, FEE)

    # Pool B: sell USDC for WBTC
    # If USDC is token0 (zfo_b=True): input reserves = USDC balance, output = WBTC balance
    # If USDC is token1 (zfo_b=False): input reserves = USDC balance, output = WBTC balance
    res_b_in = usdc.balanceOf(pool_b.address)
    res_b_out = wbtc.balanceOf(pool_b.address)
    amount_wbtc = v2_get_amount_out(amount_usdc, res_b_in, res_b_out, FEE)

    # Pool C: sell WBTC for WETH
    # Same logic: input = WBTC reserves, output = WETH reserves
    res_c_in = wbtc.balanceOf(pool_c.address)
    res_c_out = weth.balanceOf(pool_c.address)
    amount_weth_out = v2_get_amount_out(amount_wbtc, res_c_in, res_c_out, FEE)

    return amount_usdc, amount_wbtc, amount_weth_out, zfo_a, zfo_b, zfo_c


# ═══════════════════════════════════════════════════════════════════════════
# Approach 1: V2_SWAP_COMPACT nested callbacks — full executor custody
# ═══════════════════════════════════════════════════════════════════════════


class TestApproach1AllV2SwapCompact:
    """Three V2_SWAP_COMPACT calls with nested callbacks.

    Every pool sends output to the executor. Each callback pays the owed
    input token to the calling pair. Because V2 calls are blocking,
    inner callbacks complete first — by the time we need to pay an outer
    pool, the executor has already received the payment token from an
    inner pool.

    Call stack (outermost → innermost):
      V2_SWAP_COMPACT Pool A (executor receives USDC)
        callback: V2_SWAP_COMPACT Pool B (executor receives WBTC), then pay A
                   callback: V2_SWAP_COMPACT Pool C (executor receives WETH), then pay B
                              callback: pay C (transfer WBTC owed)

    ERC-20 transfers: 6
      A → executor (USDC, optimistic)
      B → executor (WBTC, optimistic)
      C → executor (WETH, optimistic)
      executor → C (WBTC, callback pay)
      executor → B (USDC, callback pay)
      executor → A (WETH, callback pay)
    Callbacks: 3 (A, B, C)
    """

    def test_three_pool_nested_callback(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):

        weth.mint(executor.address, AMOUNT_WETH_IN, sender=owner_account)

        _fund_pools(pool_a, pool_b, pool_c, weth, usdc, wbtc, owner_account)
        amount_usdc, amount_wbtc, amount_weth_out, zfo_a, zfo_b, zfo_c = (
            _compute_amounts(pool_a, pool_b, pool_c, weth, usdc, wbtc, AMOUNT_WETH_IN)
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        pool_a_idx = at.add(pool_a.address)
        pool_b_idx = at.add(pool_b.address)
        pool_c_idx = at.add(pool_c.address)

        # Innermost: Pool C callback — pay WBTC that Pool C is owed.
        # Pool C sent WETH to executor, owes WBTC input.
        # We must transfer the exact amount_in that satisfies K.
        # Using _v2_get_amount_in on-chain = the auto-pay formula.
        # For simplicity, pay the same amount that Pool B sent out
        # (which satisfies K because it was produced by the same formula).
        pool_c_callback = enc_erc20_transfer(wbtc_idx, pool_c_idx, amount_wbtc)

        # Pool B callback: swap Pool C (sends WETH to executor), then pay
        # USDC to Pool B. Pool B sent WBTC, owes USDC input.
        pool_b_callback = enc_v2_swap_compact(
            pool_c_idx,
            zfo_c,
            amount_weth_out,
            executor_idx,
            forward_data=pool_c_callback,
        )
        pool_b_callback += enc_erc20_transfer(usdc_idx, pool_b_idx, amount_usdc)

        # Pool A callback: swap Pool B (sends WBTC to executor), then pay
        # WETH to Pool A. Pool A sent USDC, owes WETH input.
        pool_a_callback = enc_v2_swap_compact(
            pool_b_idx,
            zfo_b,
            amount_wbtc,
            executor_idx,
            forward_data=pool_b_callback,
        )
        pool_a_callback += enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)

        # Top-level: V2_SWAP_COMPACT Pool A (sends USDC to executor)
        commands = enc_v2_swap_compact(
            pool_a_idx,
            zfo_a,
            amount_usdc,
            executor_idx,
            forward_data=pool_a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        # Executor should end up with net WETH profit
        assert weth.balanceOf(executor.address) > 0, "executor should have WETH profit"


# ═══════════════════════════════════════════════════════════════════════════
# Approach 2: V2_SWAP_COMPACT flash + 2× V2_SWAP_CALC — direct custody
# ═══════════════════════════════════════════════════════════════════════════


class TestApproach2FlashPlusSwapCalc:
    """Flash borrow from Pool A, then V2_SWAP_CALC for pools B and C.

    V2 callback constraint: pair.swap() invokes the callback on the
    `to` address (the recipient). V2_SWAP_COMPACT passes forward_data
    as the callback data, so the recipient MUST be the executor —
    otherwise the callback goes to the wrong contract.

    V2_SWAP_CALC, however, calls pair.swap() with data=b"" (no
    callback), so it CAN send output directly to the next pool,
    creating excess balance that the next V2_SWAP_CALC reads as input.
    This eliminates 2 intermediate executor custody transfers.

    Flow:
      V2_SWAP_COMPACT Pool A (WETH→USDC, recipient=executor, forward_data)
        → Pool A sends USDC to executor, callback fires
        callback:
          ERC20_TRANSFER USDC to Pool B (creates excess)
          V2_SWAP_CALC Pool B (excess USDC → WBTC, recipient=Pool C)
            → Pool B sends WBTC directly to Pool C (no callback)
          V2_SWAP_CALC Pool C (excess WBTC → WETH, recipient=executor)
            → Pool C sends WETH directly to executor (no callback)
          ERC20_TRANSFER WETH to Pool A (flash repayment)

    ERC-20 transfers: 5 (A→exec, exec→B, B→C, C→exec, exec→A)
    Callbacks: 1 (Pool A via uniswapV2Call)

    vs Approach 1: saves 2 callbacks and 1 transfer (B→C and C→exec
    go directly between pools, skipping executor custody).
    """

    def test_three_pool_flash_plus_calc(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):
        _fund_pools(pool_a, pool_b, pool_c, weth, usdc, wbtc, owner_account)
        amount_usdc, amount_wbtc, amount_weth_out, zfo_a, zfo_b, zfo_c = (
            _compute_amounts(pool_a, pool_b, pool_c, weth, usdc, wbtc, AMOUNT_WETH_IN)
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        pool_a_idx = at.add(pool_a.address)
        pool_b_idx = at.add(pool_b.address)
        pool_c_idx = at.add(pool_c.address)

        # Pool A callback: transfer USDC to Pool B, V2_SWAP_CALC B + C, repay A.
        # V2_SWAP_COMPACT recipient must be executor (the callback target),
        # so USDC first goes to executor, then we transfer to Pool B.
        # V2_SWAP_CALC can send directly to next pool (no callback needed).
        pool_a_callback = enc_erc20_transfer(usdc_idx, pool_b_idx, amount_usdc)
        pool_a_callback += enc_v2_swap_calc(pool_b_idx, zfo_b, pool_c_idx, fee=FEE)
        pool_a_callback += enc_v2_swap_calc(pool_c_idx, zfo_c, executor_idx, fee=FEE)
        pool_a_callback += enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)

        # Top-level: V2_SWAP_COMPACT Pool A — recipient MUST be executor
        # (so the callback fires on the executor, not on Pool B).
        commands = enc_v2_swap_compact(
            pool_a_idx,
            zfo_a,
            amount_usdc,
            executor_idx,
            forward_data=pool_a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0, "executor should have WETH profit"


# ═══════════════════════════════════════════════════════════════════════════
# Approach 3: All V2_SWAP_CALC — zero callbacks
# ═══════════════════════════════════════════════════════════════════════════


class TestApproach3AllSwapCalc:
    """Pre-fund Pool A with WETH, then V2_SWAP_CALC through the chain.

    No callbacks at all. The executor must hold WETH before executing
    (available from deployment wrap). The pre-funding transfer creates
    excess balance in Pool A. Each pool's output goes directly to the
    next pool (or executor for the last one), cascading excess balance.

    Flow:
      ERC20_TRANSFER WETH to Pool A  (creates excess balance)
      V2_SWAP_CALC Pool A (excess WETH → USDC, recipient=Pool B)
        → Pool A sends USDC directly to Pool B (no callback)
      V2_SWAP_CALC Pool B (excess USDC → WBTC, recipient=Pool C)
        → Pool B sends WBTC directly to Pool C (no callback)
      V2_SWAP_CALC Pool C (excess WBTC → WETH, recipient=executor)
        → Pool C sends WETH to executor (no callback)

    ERC-20 transfers: 4 (exec→A, A→B, B→C, C→exec)
    Callbacks: 0
    """

    def test_three_pool_all_calc(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):
        weth.mint(executor.address, AMOUNT_WETH_IN, sender=owner_account)
        _fund_pools(pool_a, pool_b, pool_c, weth, usdc, wbtc, owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        pool_a_idx = at.add(pool_a.address)
        pool_b_idx = at.add(pool_b.address)
        pool_c_idx = at.add(pool_c.address)

        zfo_a = pool_a.token0() == weth.address
        zfo_b = pool_b.token0() == usdc.address
        zfo_c = pool_c.token0() == wbtc.address

        # Pre-fund Pool A with WETH (creates excess balance)
        # Then chain V2_SWAP_CALC with direct custody through B and C.
        # No amount_out needed — V2_SWAP_CALC computes it from reserves +
        # excess balance at runtime.
        commands = enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)
        commands += enc_v2_swap_calc(pool_a_idx, zfo_a, pool_b_idx, fee=FEE)
        commands += enc_v2_swap_calc(pool_b_idx, zfo_b, pool_c_idx, fee=FEE)
        commands += enc_v2_swap_calc(pool_c_idx, zfo_c, executor_idx, fee=FEE)

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0, "executor should have WETH profit"


# ═══════════════════════════════════════════════════════════════════════════
# Gas comparison
# ═══════════════════════════════════════════════════════════════════════════


class TestGasComparison:
    """Run all three approaches and print gas usage for comparison.

    Each test method deploys its own pools + executor (via inherited fixtures)
    and prints gas usage. pytest output shows the relative gas costs.

    Summary of transfer/callback counts:
      Approach 1: 6 transfers, 3 callbacks — naive nested callbacks
      Approach 2: 5 transfers, 1 callback — flash + 2× V2_SWAP_CALC direct custody
      Approach 3: 4 transfers, 0 callbacks — all V2_SWAP_CALC (best if executor has WETH)
    """

    def test_approach_1_gas(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):
        _fund_pools(pool_a, pool_b, pool_c, weth, usdc, wbtc, owner_account)
        amount_usdc, amount_wbtc, amount_weth_out, zfo_a, zfo_b, zfo_c = (
            _compute_amounts(pool_a, pool_b, pool_c, weth, usdc, wbtc, AMOUNT_WETH_IN)
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        pool_a_idx = at.add(pool_a.address)
        pool_b_idx = at.add(pool_b.address)
        pool_c_idx = at.add(pool_c.address)

        pool_c_callback = enc_erc20_transfer(wbtc_idx, pool_c_idx, amount_wbtc)
        pool_b_callback = enc_v2_swap_compact(
            pool_c_idx,
            zfo_c,
            amount_weth_out,
            executor_idx,
            forward_data=pool_c_callback,
        )
        pool_b_callback += enc_erc20_transfer(usdc_idx, pool_b_idx, amount_usdc)
        pool_a_callback = enc_v2_swap_compact(
            pool_b_idx,
            zfo_b,
            amount_wbtc,
            executor_idx,
            forward_data=pool_b_callback,
        )
        pool_a_callback += enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)
        commands = enc_v2_swap_compact(
            pool_a_idx,
            zfo_a,
            amount_usdc,
            executor_idx,
            forward_data=pool_a_callback,
        )

        tx = executor.execute(enc_preamble(at) + commands, sender=owner_account)
        assert tx.status == 1
        print(
            f"\n    Approach 1 (3× COMPACT, nested callbacks):   {tx.gas_used:>8,} gas"
        )

    def test_approach_2_gas(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):
        _fund_pools(pool_a, pool_b, pool_c, weth, usdc, wbtc, owner_account)
        amount_usdc, amount_wbtc, amount_weth_out, zfo_a, zfo_b, zfo_c = (
            _compute_amounts(pool_a, pool_b, pool_c, weth, usdc, wbtc, AMOUNT_WETH_IN)
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        pool_a_idx = at.add(pool_a.address)
        pool_b_idx = at.add(pool_b.address)
        pool_c_idx = at.add(pool_c.address)

        pool_a_callback = enc_erc20_transfer(usdc_idx, pool_b_idx, amount_usdc)
        pool_a_callback += enc_v2_swap_calc(pool_b_idx, zfo_b, pool_c_idx, fee=FEE)
        pool_a_callback += enc_v2_swap_calc(pool_c_idx, zfo_c, executor_idx, fee=FEE)
        pool_a_callback += enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)
        commands = enc_v2_swap_compact(
            pool_a_idx,
            zfo_a,
            amount_usdc,
            executor_idx,
            forward_data=pool_a_callback,
        )

        tx = executor.execute(enc_preamble(at) + commands, sender=owner_account)
        assert tx.status == 1
        print(f"    Approach 2 (1× COMPACT + 2× CALC, direct):   {tx.gas_used:>8,} gas")

    def test_approach_3_gas(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):
        weth.mint(executor.address, AMOUNT_WETH_IN, sender=owner_account)
        _fund_pools(pool_a, pool_b, pool_c, weth, usdc, wbtc, owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        pool_a_idx = at.add(pool_a.address)
        pool_b_idx = at.add(pool_b.address)
        pool_c_idx = at.add(pool_c.address)

        zfo_a = pool_a.token0() == weth.address
        zfo_b = pool_b.token0() == usdc.address
        zfo_c = pool_c.token0() == wbtc.address

        commands = enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)
        commands += enc_v2_swap_calc(pool_a_idx, zfo_a, pool_b_idx, fee=FEE)
        commands += enc_v2_swap_calc(pool_b_idx, zfo_b, pool_c_idx, fee=FEE)
        commands += enc_v2_swap_calc(pool_c_idx, zfo_c, executor_idx, fee=FEE)

        tx = executor.execute(enc_preamble(at) + commands, sender=owner_account)
        assert tx.status == 1
        print(f"    Approach 3 (3× CALC, zero callbacks):        {tx.gas_used:>8,} gas")


# ═══════════════════════════════════════════════════════════════════════════
# Correctness: verify the same economic result across all approaches
# ═══════════════════════════════════════════════════════════════════════════


class TestEconomicEquivalence:
    """All three approaches should produce positive WETH profit for the executor."""

    def _run_approach(
        self,
        approach,
        weth,
        usdc,
        wbtc,
        owner_account,
        executor,
        pool_a,
        pool_b,
        pool_c,
    ):
        """Run a single approach and return executor's WETH profit."""
        _fund_pools(pool_a, pool_b, pool_c, weth, usdc, wbtc, owner_account)
        amount_usdc, amount_wbtc, amount_weth_out, zfo_a, zfo_b, zfo_c = (
            _compute_amounts(pool_a, pool_b, pool_c, weth, usdc, wbtc, AMOUNT_WETH_IN)
        )

        if approach == 3:
            # Approach 3 pre-funds Pool A with WETH — executor must hold WETH.
            weth.mint(executor.address, AMOUNT_WETH_IN, sender=owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        pool_a_idx = at.add(pool_a.address)
        pool_b_idx = at.add(pool_b.address)
        pool_c_idx = at.add(pool_c.address)

        if approach == 1:
            pool_c_callback = enc_erc20_transfer(wbtc_idx, pool_c_idx, amount_wbtc)
            pool_b_callback = enc_v2_swap_compact(
                pool_c_idx,
                zfo_c,
                amount_weth_out,
                executor_idx,
                forward_data=pool_c_callback,
            )
            pool_b_callback += enc_erc20_transfer(usdc_idx, pool_b_idx, amount_usdc)
            pool_a_callback = enc_v2_swap_compact(
                pool_b_idx,
                zfo_b,
                amount_wbtc,
                executor_idx,
                forward_data=pool_b_callback,
            )
            pool_a_callback += enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)
            commands = enc_v2_swap_compact(
                pool_a_idx,
                zfo_a,
                amount_usdc,
                executor_idx,
                forward_data=pool_a_callback,
            )
        elif approach == 2:
            pool_a_callback = enc_erc20_transfer(usdc_idx, pool_b_idx, amount_usdc)
            pool_a_callback += enc_v2_swap_calc(pool_b_idx, zfo_b, pool_c_idx, fee=FEE)
            pool_a_callback += enc_v2_swap_calc(
                pool_c_idx, zfo_c, executor_idx, fee=FEE
            )
            pool_a_callback += enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)
            commands = enc_v2_swap_compact(
                pool_a_idx,
                zfo_a,
                amount_usdc,
                executor_idx,
                forward_data=pool_a_callback,
            )
        else:  # approach 3
            commands = enc_erc20_transfer(weth_idx, pool_a_idx, AMOUNT_WETH_IN)
            commands += enc_v2_swap_calc(pool_a_idx, zfo_a, pool_b_idx, fee=FEE)
            commands += enc_v2_swap_calc(pool_b_idx, zfo_b, pool_c_idx, fee=FEE)
            commands += enc_v2_swap_calc(pool_c_idx, zfo_c, executor_idx, fee=FEE)

        weth_before = weth.balanceOf(executor.address)
        executor.execute(enc_preamble(at) + commands, sender=owner_account)
        return weth.balanceOf(executor.address) - weth_before

    def test_approach_1_profit(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):
        profit = self._run_approach(
            1, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
        )
        assert profit > 0, "Approach 1 should produce WETH profit"

    def test_approach_2_profit(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):
        profit = self._run_approach(
            2, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
        )
        assert profit > 0, "Approach 2 should produce WETH profit"

    def test_approach_3_profit(
        self, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
    ):
        profit = self._run_approach(
            3, weth, usdc, wbtc, owner_account, executor, pool_a, pool_b, pool_c
        )
        assert profit > 0, "Approach 3 should produce WETH profit"
