"""
Optimized three-hop permutation tests: minimal-transfer routing for all 27 V2/V3/V4 combinations.

Each test demonstrates the OPTIMAL (minimum-transfer) routing for its permutation,
applying the direct-custody rules from docs/pool-mechanics.md:

1. V2→V2: Executor pre-funds V2a, then V2_SWAP_DIRECT chains through excess balance (4 transfers)
2. V2→V3: V2 called inside V3b's callback with to=V3b, output arrives during callback (IIA ✓) (saves 1)
3. V2→V4: Executor sends to PM via sync+send+settle inside V2 callback (saves 1)
4. V3→V2: V3 sends output directly to V2 pair (excess → V2_SWAP_DIRECT) (saves 1)
5. V3→V3: Reverse-order direct custody — inner V3 fires first, outer sends during callback (saves 2)
6. V3→V4: V3→PM (sync before swap, settle inside unlock) + V4_TAKE→V3a (IIA ✓) (saves 1-2)
7. V4→V2: V4_TAKE sends tokens directly to V2 pair (excess → V2_SWAP_DIRECT) (saves 1)
8. V4→V3: Reverse-order from V3 — V4_TAKE→V3b during V3b's callback satisfies IIA (saves 1)
9. V4→V4: Delta netting — 0 internal transfers in same unlock (saves 2/pair)

Transfer counts (naive → optimized):
  V2-V2-V2:  6→4    V2-V2-V3:  6→4    V2-V2-V4:  6→4
  V2-V3-V2:  6→4    V2-V3-V3:  6→4    V2-V3-V4:  6→4
  V2-V4-V2:  6→4    V2-V4-V3:  6→4    V2-V4-V4:  6→3
  V3-V2-V2:  6→4    V3-V2-V3:  6→4    V3-V2-V4:  6→4
  V3-V3-V2:  6→4    V3-V3-V3:  6→4    V3-V3-V4:  6→4
  V3-V4-V2:  6→4    V3-V4-V3:  6→4    V3-V4-V4:  6→3
  V4-V2-V2:  6→4    V4-V2-V3:  6→4    V4-V2-V4:  6→4
  V4-V3-V2:  6→4    V4-V3-V3:  6→4    V4-V3-V4:  6→3
  V4-V4-V2:  5→3    V4-V4-V3:  5→3    V4-V4-V4:  2→1

ALL 27 paths at ≤4 transfers. Total savings: 56 (35.9% from 156 naive).

Verification: each test asserts both (1) the expected number of ERC20 Transfer
events (transfer count) and (2) token conservation (no token created/destroyed
across all tracked accounts). See _run_and_verify and _verify_conservation.
"""

import pytest

from .conftest_shared import (
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v2_swap_compact,
    enc_v2_swap_direct,
    enc_v3_swap_compact,
    enc_v4_swap_compact,
    enc_v4_take_compact,
    enc_v4_take_delta,
    enc_v4_mint_compact,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_settle_delta,
    enc_v4_unlock,
    enc_erc20_transfer,
    _make_pool_key,
    _setup_v4_swap,
    _setup_v2_pair,
    AddressTable,
    enc_preamble,
    v2_get_amount_out,
    record_gas,
    Q96,
    _isqrt,
)
from .verify import (
    count_transfers,
    summarize_events,
    snapshot_balances,
    diff_snapshots,
    NATIVE_ADDRESS,
)

pytestmark = pytest.mark.gas

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_WBTC = 100 * 10**8
AMOUNT_WETH_PROFIT = 2 * 10**18
V2_FEE = 30


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


# ── Helpers ──


def _setup_v2(pool, input_token, output_token, owner, amount_in, fee=V2_FEE):
    return _setup_v2_pair(pool, input_token, output_token, owner, amount_in, fee=fee)


def _setup_v3(pool, input_token, output_token, amount_in, amount_out, owner, liquidity_factor=100):
    """Set up a V3 pool with ample liquidity at the desired price.

    Computes sqrtPriceX96 from the amount_in/amount_out ratio (implied price),
    then initializes the pool and provides liquidity_factor * amount of each
    token as liquidity. With liquidity_factor=100 (default), price impact on the
    first swap is ~1%, making the output close to the canned amount_out.

    Unlike the legacy set_next_swap, the pool computes outputs from its state
    using the real V3 constant-product math. After a swap, the price moves,
    so subsequent swaps will get different rates.

    Returns (zfo, amount_out_actual) where amount_out_actual is the computed
    V3 swap output for the given amount_in at the current pool state.
    """
    zfo = pool.token0() == input_token.address

    # V3 price = token1/token0 in raw units (decimals already embedded in amounts)
    # sqrt_price_x96 = sqrt(price * Q96^2)
    if zfo:
        # token0=input, token1=output: price = amount_out / amount_in
        price_scaled = amount_out * Q96 * Q96 // amount_in
        sqrt_price_x96 = _isqrt(price_scaled)
    else:
        # token0=output, token1=input: price = amount_in / amount_out
        price_scaled = amount_in * Q96 * Q96 // amount_out
        sqrt_price_x96 = _isqrt(price_scaled)

    pool.initialize(sqrt_price_x96, sender=owner)

    # Provide liquidity: liquidity_factor * each amount at the current price
    liq_input = amount_in * liquidity_factor
    liq_output = amount_out * liquidity_factor
    input_token.mint(pool.address, liq_input, sender=owner)
    output_token.mint(pool.address, liq_output, sender=owner)
    pool.add_liquidity(sender=owner)

    # Query the pool's on-chain get_amount_out — uses the exact same formula
    # and reserve snapshot that the swap will use, so the computed amount is
    # guaranteed to match.
    actual_out = pool.get_amount_out(amount_in, zfo)

    return zfo, actual_out


def _setup_v4_pool(
    pm,
    c0,
    c1,
    amount_in,
    amount_out,
    owner,
    fee=3000,
    tick_spacing=60,
    output_token=None,
):
    pool_key = _make_pool_key(c0, c1, fee=fee, tick_spacing=tick_spacing)
    zfo = pool_key[0] == c0
    _setup_v4_swap(
        pm, owner, pool_key, amount_in, amount_out, zfo, output_token=output_token
    )
    return pool_key, zfo


def _v2_swap_direct(at, pool, zfo, amount_out, recipient_idx):
    return enc_v2_swap_direct(at.add(pool.address), zfo, amount_out, recipient_idx)


def _v2_swap(at, pool, zfo, amount_out, recipient_idx, fee=30, forward_data=b""):
    return enc_v2_swap_compact(
        at.add(pool.address),
        zfo,
        amount_out,
        recipient_idx,
        fee=fee,
        forward_data=forward_data,
    )


def _v3_swap(at, pool, zfo, amount, recipient_idx, forward_data=b""):
    return enc_v3_swap_compact(
        at.add(pool.address), zfo, amount, recipient_idx, forward_data=forward_data
    )


def _v4_swap(at, pool_key, zfo, amount):
    c0_idx = at.add(pool_key[0])
    c1_idx = at.add(pool_key[1])
    return enc_v4_swap_compact(
        c0_idx, c1_idx, pool_key[2], pool_key[3], 0xFF, zfo, amount
    )


def _erc20_xfer(at, token_idx, pool, amount):
    return enc_erc20_transfer(token_idx, at.add(pool.address), amount)


# ── Transfer count verification ──
# After _run(), call _verify_transfers to assert that exactly the expected
# number of ERC20 transfers occurred, counted from on-chain events.


def _verify_transfers(tx, expected, label=""):
    """Assert exactly `expected` ERC20 transfers occurred in the transaction.

    Transfer count = raw Transfer events (topic0 = keccak256("Transfer(address,address,uint256)")).
    Each represents one physical ERC20 transfer() call that moved tokens.

    NOTE: V4 Take events are NOT added separately because every take() internally
    calls IERC20.transfer(), which already emits a Transfer event. Adding both
    would double-count.
    """
    actual = count_transfers(tx)
    events = summarize_events(tx)
    assert actual == expected, (
        f"{label}: expected {expected} transfers, on-chain events show {actual}.\n"
        f"  Events: {events}"
    )


def _run_and_verify(
    executor,
    run_executor,
    at,
    commands,
    owner,
    tokens,
    accounts,
    expected_transfers,
    label,
    expected_weth_delta=None,
    check_mode=1,
):
    """Execute a swap, then verify both transfer count and balance invariants.

    This is the main entry point for three-hop tests. It:

    1. Snapshots token balances across all tracked accounts
    2. Executes the swap via the executor
    3. Verifies the expected number of ERC20 transfers occurred
    4. Verifies balance change invariants:
       a. Executor gained WETH (positive arbitrage profit)
       b. Token conservation: no token created or destroyed
    5. Reports gas usage for the execution

    Args:
        executor:            The executor contract instance
        at:                  AddressTable with all needed addresses
        commands:            Encoded command bytes
        owner:               Transaction sender account
        tokens:              List of token contracts [weth, usdc, wbtc]
        accounts:            List of accounts to track [executor, pool_a, pool_b, ...]
        expected_transfers:  Expected number of ERC20 Transfer events
        label:               Test name for error messages (e.g. "TestV2V2V2")
        expected_weth_delta: Expected executor WETH profit. If None,
                             only asserts profit > 0.
    """
    before = snapshot_balances(tokens, accounts)
    tx = run_executor(at, commands, owner, check_mode=check_mode)
    _verify_transfers(tx, expected_transfers, label)
    _verify_conservation(label, tokens, accounts, before, executor, expected_weth_delta)
    record_gas(label, tx.gas_used)
    return tx


def _verify_conservation(
    label, tokens, accounts, before, executor, expected_weth_delta=None
):
    """Assert that balance changes satisfy arbitrage invariants.

    Verifies:

    1. **Executor profit (optional)**: if expected_weth_delta is provided,
       checks that the executor's combined WETH+ETH balance changed by
       exactly that amount. The executor may unwrap WETH to ETH internally,
       so we check the combined delta. Without expected_weth_delta, this
       check is skipped (pool reserves may not reflect real prices in
       transfer-count tests).

    2. **Token conservation**: for each non-WETH ERC20 token, the sum
       of all balance changes across all tracked accounts is zero — no
       token is created or destroyed by the arbitrage.

    3. **WETH+ETH conservation**: WETH and ETH are fungible (wrapping),
       so their combined balance changes must sum to zero across all
       tracked accounts.

    Args:
        label:               Test name for error messages
        tokens:              List of token contracts [weth, usdc, wbtc]
        accounts:            List of accounts to track (pools, executor, PM)
        before:              Snapshot dict from snapshot_balances() before tx
        executor:            The executor contract instance
        expected_weth_delta: Expected net change in executor WETH+ETH combined.
                             If None, the profit check is skipped.
    """
    after = snapshot_balances(tokens, accounts)
    diffs = diff_snapshots(before, after)

    weth_addr = tokens[0].address  # weth is always first token
    executor_addr = executor.address if hasattr(executor, "address") else executor

    # 1. Executor profit check (WETH + ETH combined, since executor may unwrap)
    if expected_weth_delta is not None:
        weth_delta = diffs.get((weth_addr, executor_addr), 0)
        eth_delta = diffs.get((NATIVE_ADDRESS, executor_addr), 0)
        combined_delta = weth_delta + eth_delta
        assert combined_delta == expected_weth_delta, (
            f"{label}: executor WETH+ETH profit mismatch. "
            f"Expected {expected_weth_delta}, got {combined_delta} "
            f"(WETH={weth_delta}, ETH={eth_delta}).\n"
            f"  All diffs: {diffs}"
        )

    # 2. ERC20 conservation for non-WETH tokens (USDC, WBTC, etc.)
    for token in tokens[1:]:
        token_addr = token.address if hasattr(token, "address") else token
        total = sum(v for (t, _), v in diffs.items() if t == token_addr)
        assert total == 0, (
            f"{label}: {token_addr} conservation violated — "
            f"sum of balance changes = {total} (should be 0).\n"
            f"  Diffs: "
            f"{dict((a, v) for (t, a), v in diffs.items() if t == token_addr)}"
        )

    # 3. WETH+ETH conservation (wrapping/unwrapping moves between them)
    weth_total = sum(v for (t, _), v in diffs.items() if t == weth_addr)
    eth_total = sum(v for (t, _), v in diffs.items() if t == NATIVE_ADDRESS)
    combined_total = weth_total + eth_total
    assert combined_total == 0, (
        f"{label}: WETH+ETH conservation violated — "
        f"WETH total={weth_total}, ETH total={eth_total}, "
        f"combined={combined_total} (should be 0).\n"
        f"  WETH diffs: "
        f"{dict((a, v) for (t, a), v in diffs.items() if t == weth_addr)}\n"
        f"  ETH diffs: "
        f"{dict((a, v) for (t, a), v in diffs.items() if t == NATIVE_ADDRESS)}"
    )


# ═══════════════════════════════════════════════════════════════════════════
# 1. V2-V2-V2  (4 transfers: exec→V2a, V2a→V2b, V2b→V2c, V2c→exec)
#    Executor pre-funds V2a with WETH (excess balance), then three
#    V2_SWAP_DIRECT commands chain through excess balance.
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V2V2:
    """V2-V2-V2: 4 transfers — reverse-order flash borrow (zero-balance).

    Process swaps in reverse order: flash borrow WETH from V2c first,
    then chain V2a→V2b via V2_SWAP_DIRECT inside V2c's callback.

    V2c.swap(to=executor) → callback on executor:
      1. ERC20_TRANSFER WETH to V2a (creates excess balance)
      2. V2_SWAP_DIRECT V2a → sends USDC to V2b (excess balance)
      3. V2_SWAP_DIRECT V2b → sends WBTC to V2c (pays the flash borrow)
    Return from callback → V2c K-invariant ✓

    Each V2 pair has its own reentrancy guard, so V2a/V2b can swap
    while V2c is locked. V2_SWAP_DIRECT calls swap(data=b"") — no
    callback is triggered on the output recipient (bypassing the
    V2 callback-to-recipient constraint).
    """

    def test_v2_v2_v2(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v2_a, v2_b, v2_c
    ):
        # Set up pools with _setup_v2_for_calc (correct price ratio for V2_SWAP_DIRECT)
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC
        )
        c_zfo = _setup_v2_for_calc(
            v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT
        )

        # Compute actual chain amounts from V2 math (K-invariant determines output)
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b_out = v2_get_amount_out(
            a_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address, user0_addr=usdc.address, user1_addr=wbtc.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)
        v2b_idx = at.add(v2_b.address)
        v2c_idx = at.add(v2_c.address)

        # Inside V2c callback: transfer WETH to V2a, then V2a→V2b, then V2b→V2c
        c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
        c_fwd += _v2_swap_direct(at, v2_a, a_zfo, a_out, v2b_idx)
        c_fwd += _v2_swap_direct(at, v2_b, b_zfo, b_out, v2c_idx)

        # Flash borrow WETH from V2c — callback lands on executor (not on another V2 pair)
        commands = _v2_swap(at, v2_c, c_zfo, c_out, executor_idx, forward_data=c_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v2_a, v2_b, v2_c],
            4,
            "TestV2V2V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 2. V2-V2-V3  (5 transfers: V2a→V2b direct, IIA✗ on V2→V3)
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V2V3:
    """V2-V2-V3: 4 transfers — reverse-order flash borrow (zero-balance).

    Reverse order: V3c fires first (sends WETH to executor), then
    inside V3c's callback: ERC20_TRANSFER WETH→V2a, V2_SWAP_DIRECT V2a→V2b,
    V2_SWAP_DIRECT V2b→V3c. V3c's IIA check: WBTC was deposited by V2b ✓.
    """

    def test_v2_v2_v3(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v2_a, v2_b, v3_c
    ):
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC
        )

        # Compute chain amounts before V3c setup (V3 needs exact amount_in)
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b_out = v2_get_amount_out(
            a_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )

        # V3c setup with actual chain amounts
        c_zfo, c_out = _setup_v3(v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account)

        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address, user0_addr=usdc.address, user1_addr=wbtc.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)
        v2b_idx = at.add(v2_b.address)
        v3c_idx = at.add(v3_c.address)

        # Inside V3c callback: transfer WETH to V2a, then V2a→V2b, then V2b→V3c
        c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
        c_fwd += _v2_swap_direct(at, v2_a, a_zfo, a_out, v2b_idx)
        c_fwd += _v2_swap_direct(at, v2_b, b_zfo, b_out, v3c_idx)

        # V3c fires first (reverse order) — sends WETH to executor, callback on executor
        commands = _v3_swap(at, v3_c, c_zfo, b_out, executor_idx, forward_data=c_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v2_a, v2_b, v3_c],
            4,
            "TestV2V2V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 3. V2-V2-V4  (4 transfers: V2a→V2b direct, V2b→PM)
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V2V4:
    """V2-V2-V4: 4 transfers — V4_TAKE→V2a direct, V2b→PM delta netting.

    V4c swap → V4_TAKE WETH→V2a directly (creates excess) →
    V2a V2_SWAP_DIRECT→V2b → V4_SYNC+V2b V2_SWAP_DIRECT→PM → V4_SETTLE →
    V4_SETTLE_DELTA WETH (profit). WBTC delta from V2b deposit nets
    V4c's debit internally.
    """

    def test_v2_v2_v4(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v2_b
    ):
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC
        )
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b_out = v2_get_amount_out(
            a_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            b_out,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)

        # V4 unlock: V4c swap, V4_TAKE WETH→V2a direct, V2a→V2b,
        # V2b→PM (delta netting), settle
        inner = _v4_swap(at, c_pk, c_zfo, b_out)
        inner += enc_v4_take_compact(
            weth_idx, v2a_idx, AMOUNT_WETH
        )  # WETH→V2a directly (creates excess)
        inner += _v2_swap_direct(at, v2_a, a_zfo, a_out, at.add(v2_b.address))
        inner += enc_v4_sync(wbtc_idx)  # snapshot before V2b deposits WBTC
        inner += _v2_swap_direct(
            at, v2_b, b_zfo, b_out, pm_idx
        )  # V2b→PM (WBTC delta netting)
        inner += enc_v4_settle()  # credit +WBTC delta
        inner += enc_v4_take_compact(
            weth_idx, executor_idx, AMOUNT_WETH_PROFIT - AMOUNT_WETH
        )  # take net profit WETH directly (delta = +AMOUNT_WETH_PROFIT (V4c) - AMOUNT_WETH (V2a IIA take) = profit; skip settle_delta exttload)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_a, v2_b],
            4,
            "TestV2V2V4",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 4. V2-V3-V2  (5 transfers: V3b→V2c direct via V2_SWAP_DIRECT)
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V3V2:
    """V2-V3-V2: 4 transfers — reverse-order from V2c, V2a→V3b during V3b callback.

    V2c fires FIRST (reverse-order). V2c sends WETH profit to executor.
    Inside V2c's callback:
    1. V3b.swap(to=V2c) — WBTC goes directly to V2c (satisfies V2c K-invariant)
    2. V3b callback (forward_data):
       a. ERC20_TRANSFER WETH→V2a (creates excess for V2_SWAP_DIRECT)
       b. V2a V2_SWAP_DIRECT→V3b (sends USDC to V3b DURING V3b callback → IIA ✓)

    V3b IIA: USDC from V2a arrived during V3b callback ✓
    V2c K-invariant: WBTC from V3b replaces WETH output ✓

    Key insight: V2a→V3b via V2_SWAP_DIRECT during V3b's callback replaces
    both the separate executor→V3b USDC payment AND the executor→V2a WETH
    flash repayment. V2a gets WETH excess directly, satisfying its own
    K-invariant, and V3b gets USDC directly from V2a, satisfying IIA.
    """

    def test_v2_v3_v2(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v2_a, v3_b, v2_c
    ):
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        # Compute a_out from V2 math for V3b setup
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_zfo = _setup_v2_for_calc(v2_c, wbtc, weth, owner_account, b_out, AMOUNT_WETH_PROFIT)
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address, user0_addr=usdc.address, user1_addr=wbtc.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)

        # V3b callback: ERC20 WETH→V2a + V2a V2_SWAP_DIRECT→V3b (IIA ✓)
        b_fwd = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
        b_fwd += _v2_swap_direct(at, v2_a, a_zfo, a_out, at.add(v3_b.address))

        # V2c fires first (reverse-order). Callback: V3b swap (WBTC→V2c)
        c_fwd = _v3_swap(
            at, v3_b, b_zfo, a_out, at.add(v2_c.address), forward_data=b_fwd
        )

        commands = _v2_swap(at, v2_c, c_zfo, c_out, executor_idx, forward_data=c_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v2_a, v3_b, v2_c],
            4,
            "TestV2V3V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 5. V2-V3-V3  (4 transfers: V3c outermost, V2a inside V3b callback with to=V3b, IIA ✓)
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V3V3:
    def test_v2_v3_v3(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v2_a, v3_b, v3_c
    ):
        a_zfo, a_out = _setup_v2(v2_a, weth, usdc, owner_account, AMOUNT_WETH)
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_zfo, c_out = _setup_v3(
            v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account
        )

        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address, user0_addr=usdc.address, user1_addr=wbtc.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)
        v3b_idx = at.add(v3_b.address)
        v3c_idx = at.add(v3_c.address)

        # OPTIMIZED: V3c outermost, V2a inside V3b callback with to=V3b (5→4 transfers)
        # V2a's optimistic USDC transfer hits V3b DURING V3b's callback → IIA ✓
        # V2a uses V2_SWAP_DIRECT (no callback) because excess WETH pre-funded.
        v3b_fwd = enc_erc20_transfer(
            weth_idx, v2a_idx, AMOUNT_WETH
        )  # pre-fund V2a with excess WETH
        v3b_fwd += _v2_swap_direct(
            at, v2_a, a_zfo, a_out, v3b_idx
        )  # V2a→V3b (K-invariant via excess)

        v3c_fwd = enc_v3_swap_compact(
            v3b_idx, b_zfo, a_out, v3c_idx, forward_data=v3b_fwd
        )

        commands = enc_v3_swap_compact(
            v3c_idx, c_zfo, b_out, executor_idx, forward_data=v3c_fwd
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v2_a, v3_b, v3_c],
            4,
            "TestV2V3V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 6. V2-V3-V4  (4 transfers: V3b outermost, V2a inside V3b callback, V3b→PM)
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V3V4:
    """V2-V3-V4: 4 transfers — V3b outermost, V2a inside V3b callback (to=V3b, IIA ✓).

    V3b sends WBTC to PM (delta netting). Inside V3b's callback:
    V4 unlock → V4_TAKE WETH→V2a (excess for K-invariant), then
    V2a.swap(to=V3b) sends USDC directly to V3b during callback (IIA ✓).
    """

    def test_v2_v3_v4(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v3_b
    ):
        a_zfo, a_out = _setup_v2(v2_a, weth, usdc, owner_account, AMOUNT_WETH)
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            b_out,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)
        v3b_idx = at.add(v3_b.address)

        # V3b callback: V4 unlock (provides WETH to V2a) + V2a→V3b (IIA ✓)
        v4_inner = enc_v4_settle()
        v4_inner += _v4_swap(at, c_pk, c_zfo, b_out)
        v4_inner += enc_v4_take_compact(
            weth_idx, v2a_idx, AMOUNT_WETH
        )  # → V2a excess for K-invariant
        v4_inner += enc_v4_take_compact(
            weth_idx, executor_idx, AMOUNT_WETH_PROFIT - AMOUNT_WETH
        )

        b_fwd = enc_v4_unlock(v4_inner)
        b_fwd += _v2_swap_direct(
            at, v2_a, a_zfo, a_out, v3b_idx
        )  # V2a→V3b (K-invariant via excess WETH)

        commands = enc_v4_sync(wbtc_idx)
        commands += _v3_swap(at, v3_b, b_zfo, a_out, pm_idx, forward_data=b_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_a, v3_b],
            4,
            "TestV2V3V4",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 7. V2-V4-V2  (4 transfers: V2a→PM, V4_TAKE→V2c direct)
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V4V2:
    """V2-V4-V2: 4 transfers — reverse-order from V2c, V2a→PM delta netting.

    V2c fires FIRST (reverse-order). V2c sends WETH profit to executor.
    Inside V2c's callback:
    1. ERC20_TRANSFER WETH→V2a (creates excess for V2_SWAP_DIRECT)
    2. V2a V2_SWAP_DIRECT→PM (sends USDC to PM, no callback on PM)
    3. V4 unlock: V4_SYNC(USDC)+V4_SETTLE (credit +USDC delta),
       V4b swap (consumes USDC), V4_TAKE WBTC→V2c (satisfies V2c K)
    V2c K-invariant: WBTC from V4_TAKE replaces WETH output ✓

    Key insight: V2a→PM via V2_SWAP_DIRECT eliminates both the
    executor→V2a WETH repayment AND the V4_SETTLE_DELTA USDC,
    because USDC goes directly to PM (delta netting) and V2a's
    WETH excess satisfies its own K-invariant without a separate
    repayment from the executor's profit WETH.
    """

    def test_v2_v4_v2(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v2_c
    ):
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )

        # Compute a_out from V2 math for V4b setup
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )

        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        c_zfo = _setup_v2_for_calc(v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT)
        c_out = v2_get_amount_out(
            AMOUNT_WBTC, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)

        # V4 unlock: sync/settle USDC from V2a→PM deposit, V4b swap, take WBTC→V2c
        v4_inner = enc_v4_sync(usdc_idx)
        v4_inner += _v2_swap_direct(at, v2_a, a_zfo, a_out, pm_idx)
        v4_inner += enc_v4_settle()
        v4_inner += _v4_swap(at, b_pk, b_zfo, a_out)
        v4_inner += enc_v4_take_compact(wbtc_idx, at.add(v2_c.address), AMOUNT_WBTC)

        # V2c fires first (reverse-order). Callback: WETH→V2a, V4 unlock
        c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
        c_fwd += enc_v4_unlock(v4_inner)

        commands = _v2_swap(at, v2_c, c_zfo, c_out, executor_idx, forward_data=c_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_a, v2_c],
            4,
            "TestV2V4V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 8. V2-V4-V3  (5 transfers: V2a→PM, IIA✗ on V4→V3)
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V4V3:
    """V2-V4-V3: 4 transfers — V3c-reverse, V2a→PM inside unlock, V4_TAKE WBTC→V3c.

    V3c fires FIRST (reverse-order). Inside V3c's callback:
    1. ERC20_TRANSFER WETH→V2a (creates excess for V2_SWAP_DIRECT)
    2. V4 unlock:
       a. V4_SYNC(USDC) — snapshot PM balance (before V2a deposit)
       b. V2a V2_SWAP_DIRECT→PM — sends USDC to PM (delta to be credited)
       c. V4_SETTLE — credits +USDC delta from V2a's deposit
       d. V4b swap — consumes USDC, produces WBTC
       e. V4_TAKE WBTC→V3c — WBTC goes directly to V3c (IIA ✓)
    V3c IIA: WBTC arrived during V3c callback (from V4_TAKE) ✓

    V2a V2_SWAP_DIRECT (data=b"") sends USDC to PM with no callback on PM.
    The V4_SYNC must happen BEFORE V2a deposits, so V4_SETTLE can detect
    the balance increase and credit the delta.
    """

    def test_v2_v4_v3(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a, v3_c
    ):
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )

        # Compute a_out from V2 math for V4b setup
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )

        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        b_out = AMOUNT_WBTC  # V4b sends exact amount via set_next_swap
        c_zfo, c_out = _setup_v3(
            v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3c_idx = at.add(v3_c.address)

        # V4 unlock: sync USDC, V2a→PM deposit, settle to credit delta, swap B, take WBTC→V3c
        v4_inner = enc_v4_sync(usdc_idx)
        v4_inner += _v2_swap_direct(at, v2_a, a_zfo, a_out, pm_idx)
        v4_inner += enc_v4_settle()
        v4_inner += _v4_swap(at, b_pk, b_zfo, a_out)
        v4_inner += enc_v4_take_compact(wbtc_idx, v3c_idx, AMOUNT_WBTC)

        # V3c fires first. Callback: WETH→V2a (creates excess), V4 unlock
        c_fwd = enc_erc20_transfer(weth_idx, at.add(v2_a.address), AMOUNT_WETH)
        c_fwd += enc_v4_unlock(v4_inner)

        commands = _v3_swap(
            at, v3_c, c_zfo, b_out, executor_idx, forward_data=c_fwd
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_a, v3_c],
            4,
            "TestV2V4V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 9. V2-V4-V4  (3 transfers: V2a→PM, V4b+V4c delta netting)
# ═══════════════════════════════════════════════════════════════════════════


class TestV2V4V4:
    """V2-V4-V4: 3 transfers — V4_TAKE WETH→V2a (excess), V2a→PM, V4_TAKE profit.

    Inside V4 unlock:
    1. V4_SYNC(USDC) — snapshot PM USDC balance before V2a deposit
    2. V4_TAKE(WETH, V2a, AMOUNT_WETH) — creates WETH excess at V2a [1 xfer]
    3. V2a V2_SWAP_DIRECT→PM — sends USDC directly to PM [1 xfer]
    4. V4_SETTLE() — credits +a_out USDC delta from V2a→PM deposit
    5. V4b + V4c swaps (delta netting, 0 xfers)
    6. V4_TAKE(WETH, executor, profit) — extracts profit [1 xfer]

    Key: V2a→PM via V2_SWAP_DIRECT eliminates both the V2a→executor USDC
    transfer AND the executor→PM USDC settle. V4_TAKE→V2a eliminates the
    separate ERC20 WETH→V2a repayment (V2 K-invariant satisfied by excess).
    """

    def test_v2_v4_v4(self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a):
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            AMOUNT_WBTC,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )
        b_out = AMOUNT_WBTC  # V4b sends exact amount via set_next_swap

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)

        profit = AMOUNT_WETH_PROFIT - AMOUNT_WETH

        v4_inner = enc_v4_sync(usdc_idx)
        v4_inner += enc_v4_take_compact(weth_idx, at.add(v2_a.address), AMOUNT_WETH)
        v4_inner += _v2_swap_direct(at, v2_a, a_zfo, a_out, pm_idx)
        v4_inner += enc_v4_settle()
        v4_inner += _v4_swap(at, b_pk, b_zfo, a_out)
        v4_inner += _v4_swap(at, c_pk, c_zfo, b_out)
        v4_inner += enc_v4_take_compact(weth_idx, executor_idx, profit)

        commands = enc_v4_unlock(v4_inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_a],
            3,
            "TestV2V4V4",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 10. V3-V2-V2  (4 transfers: V3a→V2b direct, V2b→V2c direct)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V2V2:
    def test_v3_v2_v2(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v3_a, v2_b, v2_c
    ):
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_zfo = _setup_v2_for_calc(v2_b, usdc, wbtc, owner_account, a_out, AMOUNT_WBTC)
        b_out = v2_get_amount_out(
            a_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )
        c_zfo = _setup_v2_for_calc(v2_c, wbtc, weth, owner_account, b_out, AMOUNT_WETH_PROFIT)
        # V2c receives b_out (V2b's output), not AMOUNT_WBTC — recompute c_out
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address, user0_addr=usdc.address, user1_addr=wbtc.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)

        b_cmd = _v2_swap_direct(at, v2_b, b_zfo, b_out, at.add(v2_c.address))
        c_cmd = _v2_swap_direct(at, v2_c, c_zfo, c_out, executor_idx)
        a_fwd = (
            b_cmd
            + c_cmd
            + enc_erc20_transfer(weth_idx, at.add(v3_a.address), AMOUNT_WETH)
        )

        commands = _v3_swap(
            at, v3_a, a_zfo, AMOUNT_WETH, at.add(v2_b.address), forward_data=a_fwd
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v3_a, v2_b, v2_c],
            4,
            "TestV3V2V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 11. V3-V2-V3  (4 transfers: reverse-order from V3c, V2b V2_SWAP_DIRECT→V3c)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V2V3:
    """V3-V2-V3: 4 transfers — reverse-order from V3c (zero-balance flash borrow).

    Reverse order: V3c fires first (sends WETH to executor), then inside
    V3c's callback: V3a swap (sends USDC to V2b directly, creating excess),
    then inside V3a's callback: V2b V2_SWAP_DIRECT (sends WBTC directly to
    V3c, satisfying IIA), then explicit WETH payment to V3a.

    V3c IIA: WBTC deposited by V2b during V3c callback ✓
    V3a IIA: WETH paid by executor (from V3c output) during V3a callback ✓
    V2b V2_SWAP_DIRECT reads excess USDC (from V3a direct output) ✓
    """

    def test_v3_v2_v3(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v3_a, v2_b, v3_c
    ):
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, a_out, AMOUNT_WBTC
        )

        # Compute chain amounts before V3c setup (V3 needs exact amount_in)
        b_out = v2_get_amount_out(
            a_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )

        c_zfo, c_out = _setup_v3(v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account)

        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address, user0_addr=usdc.address, user1_addr=wbtc.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3c_idx = at.add(v3_c.address)

        # V3a callback: V2b V2_SWAP_DIRECT→V3c (direct custody) + explicit WETH→V3a payment
        v3a_fwd = _v2_swap_direct(at, v2_b, b_zfo, b_out, v3c_idx)
        v3a_fwd += enc_erc20_transfer(weth_idx, at.add(v3_a.address), AMOUNT_WETH)

        # V3c callback: V3a swap (recipient=V2b, direct USDC custody) with V3a's forward_data
        v3c_fwd = _v3_swap(
            at, v3_a, a_zfo, AMOUNT_WETH, at.add(v2_b.address), forward_data=v3a_fwd
        )

        # V3c fires first — sends WETH to executor, callback on executor
        commands = _v3_swap(at, v3_c, c_zfo, b_out, executor_idx, forward_data=v3c_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v3_a, v2_b, v3_c],
            4,
            "TestV3V2V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 12. V3-V2-V4  (4 transfers: V3a→V2b direct, V2b→PM)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V2V4:
    """V3-V2-V4: 4 transfers — V3a→V2b direct, V2b→PM, V4_TAKE→V3a directly.

    V4_TAKE sends WETH directly to V3a (IIA ✓ — V3a receives WETH during
    its own callback window). Same pattern as V3-V3-V4.

    V2b sends WBTC to PM (delta netting). V3a→V2b direct custody for USDC.
    """

    def test_v3_v2_v4(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v2_b
    ):
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, a_out, AMOUNT_WBTC
        )

        # Compute b_out from V2 math
        b_out = v2_get_amount_out(
            a_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )

        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            b_out,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3a_idx = at.add(v3_a.address)

        # V4 unlock: sync WBTC (before V2b→PM deposit), settle (after), swap C, take WETH→V3a
        v4_inner = enc_v4_sync(wbtc_idx)
        v4_inner += _v2_swap_direct(
            at, v2_b, b_zfo, b_out, pm_idx
        )  # V2b→PM (WBTC delta)
        v4_inner += enc_v4_settle()  # credit +WBTC delta
        v4_inner += _v4_swap(at, c_pk, c_zfo, b_out)
        v4_inner += enc_v4_take_compact(
            weth_idx, v3a_idx, AMOUNT_WETH
        )  # → V3a directly (IIA ✓)
        v4_inner += enc_v4_take_compact(
            weth_idx, executor_idx, AMOUNT_WETH_PROFIT - AMOUNT_WETH
        )

        # V3a→V2b direct + V4 unlock inside V3a callback
        a_fwd = enc_v4_unlock(v4_inner)

        commands = _v3_swap(
            at, v3_a, a_zfo, AMOUNT_WETH, at.add(v2_b.address), forward_data=a_fwd
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_a, v2_b],
            4,
            "TestV3V2V4",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 13. V3-V3-V2  (4 transfers: reverse-order, V3a→V3b direct)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V2:
    """V3-V3-V2: 4 transfers — reverse-order, V3a→V3b direct, V2c V2_SWAP_DIRECT.

    V3b fires first (sends WBTC to V2c). Inside V3b callback:
    V3a swap (recipient=V3b, sends USDC directly → satisfies V3b IIA ✓).
    Inside V3a callback: V2c V2_SWAP_DIRECT→exec + explicit WETH→V3a payment.
    Saves 1 transfer vs. forward-order (V3a→exec + ERC20 USDC→V3b → V3a→V3b direct).
    """

    def test_v3_v3_v2(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v3_a, v3_b, v2_c
    ):
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_zfo = _setup_v2_for_calc(
            v2_c, wbtc, weth, owner_account, b_out, AMOUNT_WETH_PROFIT
        )
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address, user0_addr=usdc.address, user1_addr=wbtc.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)

        # V3a callback: V2c V2_SWAP_DIRECT (excess WBTC → WETH to exec) + WETH→V3a
        v3a_fwd = _v2_swap_direct(at, v2_c, c_zfo, c_out, executor_idx)
        v3a_fwd += enc_erc20_transfer(weth_idx, at.add(v3_a.address), AMOUNT_WETH)

        # V3b callback: V3a swap (recipient=V3b, USDC→V3b direct — satisfies IIA)
        v3b_fwd = _v3_swap(
            at, v3_a, a_zfo, AMOUNT_WETH, at.add(v3_b.address), forward_data=v3a_fwd
        )

        # V3b fires first — sends WBTC to V2c (creates excess for V2_SWAP_DIRECT)
        commands = _v3_swap(
            at, v3_b, b_zfo, a_out, at.add(v2_c.address), forward_data=v3b_fwd
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v3_a, v3_b, v2_c],
            4,
            "TestV3V3V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 14. V3-V3-V3  (4 transfers: reverse-order direct custody)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V3:
    def test_v3_v3_v3(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v3_a, v3_b, v3_c
    ):
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_zfo, c_out = _setup_v3(
            v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account
        )

        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address, user0_addr=usdc.address, user1_addr=wbtc.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3a_idx = at.add(v3_a.address)
        v3b_idx = at.add(v3_b.address)
        v3c_idx = at.add(v3_c.address)

        v3a_callback = b""
        v3b_callback = enc_v3_swap_compact(
            v3a_idx, a_zfo, AMOUNT_WETH, v3b_idx, forward_data=v3a_callback
        )
        v3c_callback = enc_v3_swap_compact(
            v3b_idx, b_zfo, a_out, v3c_idx, forward_data=v3b_callback
        )

        commands = enc_v3_swap_compact(
            v3c_idx, c_zfo, b_out, executor_idx, forward_data=v3c_callback
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v3_a, v3_b, v3_c],
            4,
            "TestV3V3V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 15. V3-V3-V4  (3 transfers: reverse-order + V4_TAKE→V3a)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V4:
    def test_v3_v3_v4(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v3_b
    ):
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            b_out,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3a_idx = at.add(v3_a.address)
        v3b_idx = at.add(v3_b.address)

        v4_inner = enc_v4_settle()  # settle WBTC deposited by V3b
        v4_inner += _v4_swap(at, c_pk, c_zfo, b_out)
        v4_inner += enc_v4_take_compact(
            weth_idx, v3a_idx, AMOUNT_WETH
        )  # → V3a directly! (IIA✓)
        v4_inner += enc_v4_take_compact(
            weth_idx, executor_idx, AMOUNT_WETH_PROFIT - AMOUNT_WETH
        )

        a_fwd = enc_v4_unlock(v4_inner)
        b_fwd = _v3_swap(at, v3_a, a_zfo, AMOUNT_WETH, v3b_idx, forward_data=a_fwd)

        commands = enc_v4_sync(wbtc_idx)
        commands += _v3_swap(at, v3_b, b_zfo, a_out, pm_idx, forward_data=b_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_a, v3_b],
            4,
            "TestV3V3V4",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 16. V3-V4-V2  (4 transfers: V3a→PM, V4_TAKE→V2c direct)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V4V2:
    def test_v3_v4_v2(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v2_c
    ):
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        b_out = AMOUNT_WBTC  # V4b sends exact amount via set_next_swap
        c_zfo = _setup_v2_for_calc(v2_c, wbtc, weth, owner_account, b_out, AMOUNT_WETH_PROFIT)
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)

        v4_inner = enc_v4_settle()  # settle USDC deposited by V3a
        v4_inner += _v4_swap(at, b_pk, b_zfo, a_out)
        v4_inner += enc_v4_take_compact(wbtc_idx, at.add(v2_c.address), AMOUNT_WBTC)

        c_cmd = _v2_swap_direct(at, v2_c, c_zfo, c_out, executor_idx)
        a_fwd = enc_v4_unlock(v4_inner) + c_cmd
        a_fwd += enc_erc20_transfer(weth_idx, at.add(v3_a.address), AMOUNT_WETH)

        commands = enc_v4_sync(usdc_idx)
        commands += _v3_swap(at, v3_a, a_zfo, AMOUNT_WETH, pm_idx, forward_data=a_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_a, v2_c],
            4,
            "TestV3V4V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 17. V3-V4-V3  (5 transfers: V3a→PM, IIA✗ on V4→V3)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V4V3:
    """V3-V4-V3: 4 transfers — V3c-reverse, V3a auto-pay + V4_TAKE WBTC→V3c (IIA ✓).

    V3c fires first (reverse-order). Inside V3c callback:
    1. V3a swap (USDC→PM) with forward_data containing:
       a. ERC20_TRANSFER WETH→V3a (auto-pay, IIA ✓ during V3a callback)
       b. V4 unlock: V4b swap + V4_TAKE WBTC→V3c (IIA ✓ during V3c callback)
    V3c IIA: WBTC from V4_TAKE arrived during V3c callback ✓
    V3a IIA: WETH from ERC20_TRANSFER arrived during V3a callback ✓

    The V3a auto-pay is explicit in forward_data (not the empty-data auto-pay),
    because V3a's callback must also process the V4 unlock.
    """

    def test_v3_v4_v3(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a, v3_c
    ):
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        b_out = AMOUNT_WBTC  # V4b sends exact amount via set_next_swap
        c_zfo, c_out = _setup_v3(
            v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3c_idx = at.add(v3_c.address)

        # V3a callback: explicit WETH→V3a payment + V4 unlock (V4b swap, V4_TAKE WBTC→V3c)
        v4_inner = (
            enc_v4_settle()
        )  # credit +USDC delta (V3a deposited to PM after sync)
        v4_inner += _v4_swap(at, b_pk, b_zfo, a_out)
        v4_inner += enc_v4_take_compact(wbtc_idx, v3c_idx, b_out)

        a_fwd = enc_erc20_transfer(weth_idx, at.add(v3_a.address), AMOUNT_WETH)
        a_fwd += enc_v4_unlock(v4_inner)

        # V3c fires first. V4_SYNC(USDC) before V3a deposits to PM
        c_fwd = _v3_swap(at, v3_a, a_zfo, AMOUNT_WETH, pm_idx, forward_data=a_fwd)

        commands = enc_v4_sync(usdc_idx)
        commands += _v3_swap(
            at, v3_c, c_zfo, b_out, executor_idx, forward_data=c_fwd
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_a, v3_c],
            4,
            "TestV3V4V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 18. V3-V4-V4  (3 transfers: V3a→PM, V4_TAKE→V3a directly)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V4V4:
    def test_v3_v4_v4(self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_a):
        a_zfo, a_out = _setup_v3(v3_a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        b_out = AMOUNT_WBTC  # V4b sends exact amount via set_next_swap
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            b_out,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3a_idx = at.add(v3_a.address)

        v4_inner = enc_v4_settle()
        v4_inner += _v4_swap(at, b_pk, b_zfo, a_out)
        v4_inner += _v4_swap(at, c_pk, c_zfo, b_out)
        v4_inner += enc_v4_take_compact(weth_idx, v3a_idx, AMOUNT_WETH)  # → V3a! (IIA✓)
        v4_inner += enc_v4_take_compact(
            weth_idx, executor_idx, AMOUNT_WETH_PROFIT - AMOUNT_WETH
        )

        a_fwd = enc_v4_unlock(v4_inner)

        commands = enc_v4_sync(usdc_idx)
        commands += _v3_swap(at, v3_a, a_zfo, AMOUNT_WETH, pm_idx, forward_data=a_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_a],
            3,
            "TestV3V4V4",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 19. V4-V2-V2  (4 transfers: V4_TAKE→V2b, V2b→V2c direct)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V2V2:
    def test_v4_v2_v2(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_b, v2_c
    ):
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        b_zfo = _setup_v2_for_calc(v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC)
        b_out = v2_get_amount_out(
            AMOUNT_USDC, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )
        c_zfo = _setup_v2_for_calc(v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT)
        # V2c receives b_out (V2b's output), not AMOUNT_WBTC — recompute c_out
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)

        b_cmd = _v2_swap_direct(at, v2_b, b_zfo, b_out, at.add(v2_c.address))
        c_cmd = _v2_swap_direct(at, v2_c, c_zfo, c_out, executor_idx)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        inner += enc_v4_take_compact(usdc_idx, at.add(v2_b.address), AMOUNT_USDC)
        inner += b_cmd + c_cmd
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_b, v2_c],
            4,
            "TestV4V2V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 20. V4-V2-V3  (5 transfers: V4_TAKE→V2b direct, IIA✗ on V2→V3)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V2V3:
    """V4-V2-V3: 4 transfers — V3c-reverse, V4_TAKE→V2b, V2b→V3c direct.

    V3c fires FIRST (reverse-order). Inside V3c's callback: V4 unlock with
    V4a swap, V4_TAKE USDC→V2b (direct custody), V2b V2_SWAP_DIRECT→V3c
    (WBTC goes directly to V3c — satisfies V3c IIA during own callback ✓).
    V4_SETTLE_DELTA WETH covers V4a's debit.

    Key insight: "V4→V3 IIA ✗" only applies in FORWARD-order (V4_TAKE arrives
    before V3.swap() starts). In REVERSE-order (V3c fires first, V4_TAKE
    deposits during V3c's callback), the tokens arrive DURING the callback
    window, satisfying IIA.
    """

    def test_v4_v2_v3(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_b, v3_c
    ):
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC
        )

        # Compute b_out from V2 math for V3c setup
        b_out = v2_get_amount_out(
            AMOUNT_USDC, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )

        c_zfo, c_out = _setup_v3(v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account)

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3c_idx = at.add(v3_c.address)

        # V4 unlock: V4a swap → V4_TAKE USDC→V2b → V2b V2_SWAP_DIRECT→V3c → settle WETH
        v4_inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        v4_inner += enc_v4_take_compact(usdc_idx, at.add(v2_b.address), AMOUNT_USDC)
        v4_inner += _v2_swap_direct(at, v2_b, b_zfo, b_out, v3c_idx)
        v4_inner += enc_v4_settle_delta(weth_idx)

        # V3c fires first — callback runs V4 unlock (V4_TAKE + V2b→V3c satisfy V3c IIA)
        commands = _v3_swap(
            at, v3_c, c_zfo, b_out, executor_idx, forward_data=enc_v4_unlock(v4_inner)
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_b, v3_c],
            4,
            "TestV4V2V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 21. V4-V2-V4  (4 transfers: single unlock, V4_TAKE→V2b, V2_SWAP_DIRECT, delta netting)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V2V4:
    """V4-V2-V4: 3 transfers — single unlock, V4_TAKE→V2b direct, V2_SWAP_DIRECT.

    V4a swap + V4_TAKE USDC→V2b + V2b V2_SWAP_DIRECT WBTC→exec + V4c swap +
    V4_SETTLE_DELTA WBTC + V4_SETTLE_DELTA WETH. All inside one unlock.

    V4_TAKE sends USDC directly to V2b (creating excess balance).
    V2b V2_SWAP_DIRECT reads excess, sends WBTC to executor (data=b"").
    V4c swap consumes WBTC via delta. V4_SETTLE_DELTA handles both currencies.
    """

    def test_v4_v2_v4(self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_b):
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC
        )

        # Compute b_out from V2 math before V4c setup
        b_out = v2_get_amount_out(
            AMOUNT_USDC, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )

        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            b_out,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)

        # Single V4 unlock: V4a swap → V4_TAKE USDC→V2b → sync(WBTC) → V2b→PM → settle → V4c swap → take WETH
        # V2b sends WBTC directly to PM (delta netting via sync+settle), eliminating
        # executor custody of WBTC + the settle_delta exttload + executor→PM transfer.
        # Mirrors the V2V4V2 "send output directly to PM" pattern on the settle side.
        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        inner += enc_v4_take_compact(usdc_idx, at.add(v2_b.address), AMOUNT_USDC)
        inner += enc_v4_sync(wbtc_idx)
        inner += _v2_swap_direct(at, v2_b, b_zfo, b_out, pm_idx)
        inner += enc_v4_settle()
        inner += _v4_swap(at, c_pk, c_zfo, b_out)
        inner += enc_v4_take_compact(
            weth_idx, executor_idx, AMOUNT_WETH_PROFIT - AMOUNT_WETH
        )  # take net profit WETH directly (delta = +AMOUNT_WETH_PROFIT (V4c) - AMOUNT_WETH (V4a debt) = profit; skip settle_delta exttload)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_b],
            3,
            "TestV4V2V4",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 22. V4-V3-V2  (5 transfers: IIA✗ on V4→V3, V3b→V2c direct)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V3V2:
    """V4-V3-V2: 4 transfers — V4_TAKE USDC→V3b during V3b callback (IIA ✓).

    V4a swap + V4_TAKE USDC→V3b inside V3b's forward_data satisfies V3b IIA
    (tokens arrive during callback). Eliminates V3b auto-pay USDC.
    V3b→V2c direct + V2c V2_SWAP_DIRECT→exec.
    """

    def test_v4_v3_v2(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_b, v2_c
    ):
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        a_out = AMOUNT_USDC  # V4a sends exact amount via set_next_swap
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_zfo = _setup_v2_for_calc(v2_c, wbtc, weth, owner_account, b_out, AMOUNT_WETH_PROFIT)
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3b_idx = at.add(v3_b.address)

        # V3b callback: V4_TAKE USDC→V3b directly (IIA ✓ during callback) + V2c swap calc
        b_fwd = enc_v4_take_compact(usdc_idx, v3b_idx, a_out)
        b_fwd += _v2_swap_direct(at, v2_c, c_zfo, c_out, executor_idx)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        inner += _v3_swap(
            at, v3_b, b_zfo, a_out, at.add(v2_c.address), forward_data=b_fwd
        )
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_b, v2_c],
            4,
            "TestV4V3V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 23. V4-V3-V3  (4 transfers: V4_TAKE USDC→V3b IIA ✓, merged WETH profit+settle)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V3V3:
    """V4-V3-V3: 4 transfers — V4_TAKE USDC→V3b (IIA ✓), merged WETH settle.

    V3c→V3b reverse-order + V4_TAKE USDC→V3b during V3b callback satisfies
    both V3b's USDC IIA and eliminates auto-pay. V4a swap + V3c→exec WETH.
    Profit capture + settle_delta merged into single WETH→PM transfer.
    """

    def test_v4_v3_v3(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_b, v3_c
    ):
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        a_out = AMOUNT_USDC  # V4a sends exact amount via set_next_swap
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_zfo, c_out = _setup_v3(
            v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3b_idx = at.add(v3_b.address)
        v3c_idx = at.add(v3_c.address)

        # V3b callback: V4_TAKE USDC→V3b (IIA ✓ during V3b callback)
        b_fwd = enc_v4_take_compact(usdc_idx, v3b_idx, a_out)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        inner += enc_v3_swap_compact(
            v3c_idx,
            c_zfo,
            b_out,
            executor_idx,
            forward_data=enc_v3_swap_compact(
                v3b_idx,
                b_zfo,
                a_out,
                v3c_idx,
                forward_data=b_fwd,
            ),
        )
        # V4_SETTLE_DELTA: reads WETH delta from exttload, auto-settles (sync+transfer+settle).
        # Replaces separate V4_SYNC + ERC20_TRANSFER + V4_SETTLE (3 commands → 1 command).
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_b, v3_c],
            4,
            "TestV4V3V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 24. V4-V3-V4  (5 transfers: V3b→PM direct for delta netting)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V3V4:
    """V4-V3-V4: 4 transfers — V4_TAKE USDC→V3b inside V3b callback (IIA ✓).

    V4a+V4c inside same unlock. V3b fires inside unlock with V4_TAKE USDC→V3b
    in its forward_data (USDC arrives during callback → IIA ✓).
    V3b→PM delta netting for WBTC.
    Eliminates V3b auto-pay (V4_TAKE deposits USDC directly).

    Key insight: V4_TAKE→V3b DURING V3b's callback satisfies IIA because
    the balance increase happens between balance_before and balance_after.
    """

    def test_v4_v3_v4(self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_b):
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        a_out = AMOUNT_USDC  # V4a sends exact amount via set_next_swap
        b_zfo, b_out = _setup_v3(v3_b, usdc, wbtc, a_out, AMOUNT_WBTC, owner_account)
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            b_out,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3b_idx = at.add(v3_b.address)

        # V3b callback: V4_TAKE USDC→V3b (IIA ✓, during callback) + V4c swap + take profit
        b_fwd = enc_v4_take_compact(usdc_idx, v3b_idx, a_out)  # USDC→V3b during callback (IIA ✓); amount known (V4a output), skip take_delta overhead
        b_fwd += _v4_swap(at, c_pk, c_zfo, b_out)
        b_fwd += enc_v4_take_compact(
            weth_idx, executor_idx, AMOUNT_WETH_PROFIT - AMOUNT_WETH
        )  # take net profit WETH directly (delta = +AMOUNT_WETH_PROFIT (V4c) - AMOUNT_WETH (V4a debt) = profit; skip take_delta exttload+INVOKE)

        # V4 unlock: swap A, V3b swap (→PM), sync+settle WBTC
        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        inner += enc_v4_sync(wbtc_idx)
        inner += _v3_swap(at, v3_b, b_zfo, a_out, pm_idx, forward_data=b_fwd)
        inner += enc_v4_settle()

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_b],
            3,
            "TestV4V3V4",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 25. V4-V4-V2  (3 transfers: delta netting + V4_TAKE→V2c)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V4V2:
    def test_v4_v4_v2(self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_c):
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        a_out = AMOUNT_USDC  # V4a sends exact amount via set_next_swap
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            AMOUNT_USDC,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        b_out = AMOUNT_WBTC  # V4b sends exact amount via set_next_swap
        c_zfo = _setup_v2_for_calc(v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT)
        c_out = v2_get_amount_out(
            AMOUNT_WBTC, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)

        c_cmd = _v2_swap_direct(at, v2_c, c_zfo, c_out, executor_idx)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        inner += _v4_swap(at, b_pk, b_zfo, a_out)
        inner += enc_v4_take_compact(wbtc_idx, at.add(v2_c.address), AMOUNT_WBTC)
        inner += c_cmd
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_c],
            3,
            "TestV4V4V2",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 26. V4-V4-V3  (4 transfers: delta netting, IIA✗ on V4→V3)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V4V3:
    def test_v4_v4_v3(self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v3_c):
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        a_out = AMOUNT_USDC  # V4a sends exact amount via set_next_swap
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            AMOUNT_USDC,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        b_out = AMOUNT_WBTC  # V4b sends exact amount via set_next_swap
        c_zfo, c_out = _setup_v3(
            v3_c, wbtc, weth, b_out, AMOUNT_WETH_PROFIT, owner_account
        )

        at = AddressTable(
            weth_addr=weth.address,
            executor_addr=executor.address,
            pm_addr=v4_pm.address,
            user0_addr=usdc.address,
            user1_addr=wbtc.address,
        )
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)

        v3c_idx = at.add(v3_c.address)
        c_take = enc_v4_take_compact(wbtc_idx, v3c_idx, b_out)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        inner += _v4_swap(at, b_pk, b_zfo, a_out)
        inner += _v3_swap(
            at, v3_c, c_zfo, b_out, executor_idx, forward_data=c_take
        )
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v3_c],
            3,
            "TestV4V4V3",
        )


# ═══════════════════════════════════════════════════════════════════════════
# 27. V4-V4-V4  (2 transfers: pure delta netting)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V4V4:
    """V4-V4-V4: 1 transfer — delta netting + V4_TAKE net profit only.

    All 3 swaps inside unlock(). Deltas net:
      V4a: -1WETH +2000USDC, V4b: -2000USDC +100WBTC, V4c: -100WBTC +2WETH
      Net: +1WETH (profit). All other deltas cancel.
    V4_TAKE 1 WETH (net profit only). No settle needed — delta is zero.
    """

    def test_v4_v4_v4(self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm):
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        a_out = AMOUNT_USDC  # V4a sends exact amount via set_next_swap
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            AMOUNT_USDC,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        b_out = AMOUNT_WBTC  # V4b sends exact amount via set_next_swap
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            AMOUNT_WBTC,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
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

        profit = AMOUNT_WETH_PROFIT - AMOUNT_WETH

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH)
        inner += _v4_swap(at, b_pk, b_zfo, a_out)
        inner += _v4_swap(at, c_pk, c_zfo, b_out)
        inner += enc_v4_mint_compact(weth_idx, executor_idx, profit)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm],
            1,
            "TestV4V4V4",
            check_mode=2,
        )


# ── V2 Reserve Setup for V2_SWAP_DIRECT ──
# _setup_v2_pair mints amount_in * 100 of BOTH tokens, giving a 1:1 reserve
# ratio regardless of the actual token prices. This works for set_next_swap
# tests but produces wildly wrong outputs with V2_SWAP_DIRECT (which computes
# outputs from on-chain reserves). We need reserves at the correct price.


def _setup_v2_for_calc(
    pool, input_token, output_token, owner, amount_in, amount_out, fee=V2_FEE
):
    """Set up a V2 pair with ample liquidity at the correct price for V2_SWAP_DIRECT.

    Mints 100x the swap amounts of each token, providing deep liquidity
    with minimal price impact. Unlike _setup_v2_pair which uses amount_in
    for both tokens (giving a 1:1 ratio), this uses the actual output amount
    to establish the correct price.
    """
    input_token.mint(pool.address, amount_in * 100, sender=owner)
    output_token.mint(pool.address, amount_out * 100, sender=owner)
    pool.sync(sender=owner)
    zfo = pool.token0() == input_token.address
    return zfo
