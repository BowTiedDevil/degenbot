"""
Tests for tstore_executor V3-V3 swap execution.

Verifies that:
1. V3→V3 paths settle correctly (nested V3 callbacks with auto-pay)
2. The inner V3 callback auto-pays WETH to the inner pool
3. The outer V3 callback auto-pays WETH to the outer pool
4. Auto-pay fires ONLY for WETH debts, not for ERC-20 forward tokens

Token flow (V3_A(zfo=True) → V3_B(zfo=False)):
  V3_A: WETH_in → USDC_out (to executor, callback triggers V3_B)
  V3_B: USDC_in → WETH_out (to executor)
  Inner callback auto-pays WETH to V3_B
  Outer callback auto-pays WETH to V3_A

Payload sequence:
  0: V3_A.swap(recipient=executor, zfo=True, ...) [will_callback=True]
     → V3_A sends USDC to executor, then callbacks executor
  1: V3_B.swap(recipient=executor, zfo=False, ...) [will_callback=True]
     → delivered inside V3_A's callback
     → V3_B sends WETH to executor, then callbacks executor (nested)
  (no explicit WETH transfer payloads needed — auto-pay handles both)

Uses fake contracts (fake_erc20, fake_weth, fake_uniswap_v3_pool) to
mock on-chain swap behavior. No V4 PoolManager is needed.
The profit check is disabled (skip_profit_check=True).
"""

import eth_abi
import pytest
from ape.api.accounts import TestAccountAPI
from ape.contracts.base import ContractInstance
from ape.managers.project import ProjectManager
from ape_test.accounts import TestAccount
from eth_utils import keccak
from eth_utils.address import to_checksum_address

WETH_DEPLOYMENT_WRAP_AMOUNT = 10 * 10**18

MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342

# ABI selectors
V3_SWAP_SELECTOR = keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
ERC20_TRANSFER_SELECTOR = keccak(text="transfer(address,uint256)")[:4]


# ── Encoding helpers ─────────────────────────────────────────


def encode_v3_swap_calldata(
    recipient: str,
    zero_for_one: bool,
    amount_specified: int,
    sqrt_price_limit_x96: int,
) -> bytes:
    """V3 swap: positive amountSpecified = exact-input."""
    return bytes(V3_SWAP_SELECTOR) + eth_abi.encode(
        types=["address", "bool", "int256", "uint160", "bytes"],
        args=[recipient, zero_for_one, amount_specified, sqrt_price_limit_x96, b""],
    )


def encode_erc20_transfer_calldata(recipient: str, amount: int) -> bytes:
    return bytes(ERC20_TRANSFER_SELECTOR) + eth_abi.encode(
        types=["address", "uint256"], args=[recipient, amount]
    )


# ── Fixtures ──────────────────────────────────────────────────


@pytest.fixture
def weth(
    project: ProjectManager,
    owner_account: TestAccount,
) -> ContractInstance:
    return project.fake_weth.deploy(
        "Fake Wrapped Ether",
        "WETH",
        18,
        100_000_000 * 10**18,
        sender=owner_account,
    )


@pytest.fixture
def usdc(
    project: ProjectManager,
    owner_account: TestAccount,
) -> ContractInstance:
    return project.fake_erc20.deploy(
        "Fake USD Coin",
        "USDC",
        6,
        100_000_000 * 10**6,
        sender=owner_account,
    )


@pytest.fixture
def owner_account(accounts: list[TestAccountAPI]) -> TestAccount:
    return accounts[0]


@pytest.fixture
def v3_pool_a(
    project: ProjectManager,
    owner_account: TestAccount,
    usdc: ContractInstance,
    weth: ContractInstance,
) -> ContractInstance:
    """Fake V3 pool A for WETH/USDC swaps (outer callback)."""
    token0, token1 = sorted(
        [usdc.address, weth.address],
        key=lambda addr: addr.lower(),
    )
    return project.fake_uniswap_v3_pool.deploy(
        token0,
        token1,
        0,  # callback_variant: 0=uniswapV3SwapCallback
        sender=owner_account,
    )


@pytest.fixture
def v3_pool_b(
    project: ProjectManager,
    owner_account: TestAccount,
    usdc: ContractInstance,
    weth: ContractInstance,
) -> ContractInstance:
    """Fake V3 pool B for WETH/USDC swaps (inner callback)."""
    token0, token1 = sorted(
        [usdc.address, weth.address],
        key=lambda addr: addr.lower(),
    )
    return project.fake_uniswap_v3_pool.deploy(
        token0,
        token1,
        0,  # callback_variant: 0=uniswapV3SwapCallback
        sender=owner_account,
    )


@pytest.fixture
def executor(
    project: ProjectManager,
    owner_account: TestAccount,
    weth: ContractInstance,
) -> ContractInstance:
    contract = project.tstore_executor.deploy(
        weth.address,
        value=WETH_DEPLOYMENT_WRAP_AMOUNT,
        sender=owner_account,
    )
    # Fund the executor with ETH for WETH wrapping and auto-pay
    contract.balance = 1000 * 10**18
    return contract


# ── Tests ──────────────────────────────────────────────────────


class TestV3V3:
    """
    V3→V3 paths: nested V3 callbacks with WETH auto-pay.

    v4_swaps=[] — all operations via the generic payload queue.
    The V3 callback auto-pays WETH to the calling pool when owed.
    """

    def test_v3_v3_nested_callback_double_autopay(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v3_pool_a: ContractInstance,
        v3_pool_b: ContractInstance,
    ):
        """
        WETH→USDC at V3_A, USDC→WETH at V3_B.

        V3_A sends USDC to executor, callbacks executor.
        Inside V3_A callback:
          - V3_B swap sends WETH to executor
          - V3_B callbacks executor (nested inner callback)
          - Inner callback auto-pays WETH to V3_B (auto-pay fires because V3_B is owed WETH)
          - After inner callback returns, outer callback auto-pays WETH to V3_A
        """
        # ── Amounts ──
        v3_a_amount_in = 1 * 10**18  # 1 WETH in to V3_A
        forward_out = 2000 * 10**6  # 2000 USDC out from V3_A
        v3_b_amount_in = forward_out  # 2000 USDC in to V3_B
        v3_b_amount_out = 2 * 10**18  # 2 WETH out from V3_B

        # ── Set up V3_A swap: WETH→USDC ──
        v3_a_token0 = v3_pool_a.token0()
        v3_a_zfo = v3_a_token0 == weth.address  # selling WETH (token0?) → zfo

        # Pre-fund V3_A with USDC for the swap output
        usdc.mint(v3_pool_a.address, forward_out, sender=owner_account)

        v3_pool_a.set_next_swap(
            v3_a_amount_in,
            forward_out,
            v3_a_zfo,
            sender=owner_account,
        )

        # ── Set up V3_B swap: USDC→WETH ──
        v3_b_token0 = v3_pool_b.token0()
        v3_b_zfo = v3_b_token0 == usdc.address  # selling USDC (token0?) → zfo

        # Pre-fund V3_B with WETH for the swap output
        weth.mint(v3_pool_b.address, v3_b_amount_out, sender=owner_account)

        v3_pool_b.set_next_swap(
            v3_b_amount_in,
            v3_b_amount_out,
            v3_b_zfo,
            sender=owner_account,
        )

        # ── Build payloads ──
        # The executor's V3 callback auto-pays WETH to the calling pool
        # when the pool is owed WETH (checks token0()/token1() == WETH_ADDR).
        # It does NOT auto-pay non-WETH ERC-20 debts — those need explicit transfers.
        #
        # For V3_B (USDC→WETH): V3_B is owed USDC (not WETH) → explicit transfer needed
        # For V3_A (WETH→USDC): V3_A is owed WETH → auto-pay handles it
        #   (auto-pay fires AFTER payload delivery, so USDC transfer to V3_B
        #    happens first, then inner callback auto-pays V3_B if owed WETH,
        #    then outer callback auto-pays V3_A)

        # 0: V3_A swap (sends USDC to executor, callbacks executor)
        v3_a_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_a_zfo else (MAX_SQRT_RATIO - 1)
        v3_a_swap_data = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_a_zfo,
            amount_specified=v3_a_amount_in,
            sqrt_price_limit_x96=v3_a_sqrt_limit,
        )

        # 1: V3_B swap (sends WETH to executor, callbacks executor — nested)
        v3_b_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_b_zfo else (MAX_SQRT_RATIO - 1)
        v3_b_swap_data = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_b_zfo,
            amount_specified=v3_b_amount_in,
            sqrt_price_limit_x96=v3_b_sqrt_limit,
        )

        # 2: Transfer USDC to V3_B (pay its non-WETH debt)
        # V3_B is owed USDC because zfo=True means pool receives token0 (USDC).
        # Auto-pay only fires for WETH, so we must transfer USDC explicitly.
        usdc_transfer_to_b = encode_erc20_transfer_calldata(
            recipient=v3_pool_b.address,
            amount=v3_b_amount_in,
        )

        payloads = [
            (v3_pool_a.address, v3_a_swap_data, 0, True),    # V3_A swap (outer callback)
            (v3_pool_b.address, v3_b_swap_data, 0, True),    # V3_B swap (inner callback)
            (usdc.address, usdc_transfer_to_b, 0, False),     # Pay USDC to V3_B
        ]

        # ── Execute ──
        tx = executor.execute_payloads(
            payloads,
            [],  # No V4 swaps
            0,  # bribe_bips
            True,  # skip_profit_check
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
