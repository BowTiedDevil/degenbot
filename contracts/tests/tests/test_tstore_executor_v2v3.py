"""
Tests for tstore_executor V2-V3 and V3-V2 swap execution.

Verifies that:
1. V2→V3 paths settle correctly (V2 flash borrow + V3 callback + WETH repayment)
2. V3→V2 paths settle correctly (V3 callback + V2 direct/flash swap)
3. Auto-pay fires for WETH debts in V3 callbacks, but not for ERC-20
4. V2 callback only resumes payloads — no auto-pay

Token flows:

V2→V3 (WETH→USDC@V2, USDC→WETH@V3):
  V2_A flash borrows USDC to executor →
  V3_B swap receives USDC, sends WETH →
  V3_B callback auto-pays WETH? No — V3_B is owed USDC (not WETH) →
  Explicit USDC transfer to V3_B →
  WETH transfer to V2_A to repay flash borrow

V3→V2 Case 1 (zfo=True): V3 buys forward, V2 direct swap
  V3_A sends USDC to executor, callbacks →
  Transfer USDC to V2_B →
  V2_B direct swap (no callback) sends WETH →
  V3_A callback auto-pays WETH to V3_A

V3→V2 Case 2 (zfo=False): V3 sells forward, WETH transfer to V2
  V3_A sends WETH to executor, callbacks →
  WETH transfer to V2_B →
  V2_B sends forward to executor →
  V3_A callback: V3_A is owed forward token (not WETH) → explicit transfer
  OR V2 sends forward to V3_A directly (recipient=V3_A)

Uses fake contracts. The profit check is disabled (skip_profit_check=True).
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
V2_SWAP_SELECTOR = keccak(text="swap(uint256,uint256,address,bytes)")[:4]
V3_SWAP_SELECTOR = keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
ERC20_TRANSFER_SELECTOR = keccak(text="transfer(address,uint256)")[:4]


# ── Encoding helpers ─────────────────────────────────────────


def encode_v2_swap_calldata(
    zero_for_one: bool,
    amount_out: int,
    recipient: str,
    flash_borrow: bool = True,
) -> bytes:
    """Encode a Uniswap V2 pool swap(uint256,uint256,address,bytes) call."""
    amount0_out, amount1_out = (0, amount_out) if zero_for_one else (amount_out, 0)
    data = b"\x01" if flash_borrow else b""
    return bytes(V2_SWAP_SELECTOR) + eth_abi.encode(
        types=["uint256", "uint256", "address", "bytes"],
        args=[amount0_out, amount1_out, recipient, data],
    )


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
def v2_pair(
    project: ProjectManager,
    owner_account: TestAccount,
    usdc: ContractInstance,
    weth: ContractInstance,
) -> ContractInstance:
    """Fake V2 pair for USDC/WETH swaps."""
    token0, token1 = sorted(
        [usdc.address, weth.address],
        key=lambda addr: addr.lower(),
    )
    return project.fake_uniswap_v2_pair.deploy(
        token0,
        token1,
        0,  # callback_variant: 0=uniswapV2Call
        sender=owner_account,
    )


@pytest.fixture
def v3_pool(
    project: ProjectManager,
    owner_account: TestAccount,
    usdc: ContractInstance,
    weth: ContractInstance,
) -> ContractInstance:
    """Fake V3 pool for USDC/WETH swaps."""
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
    contract.balance = 1000 * 10**18
    return contract


# ── Tests: V2 → V3 ──────────────────────────────────────────


class TestV2ToV3:
    """
    V2→V3 paths: V2 flash borrows forward token, V3 swap receives it,
    WETH repays V2.

    v4_swaps=[] — all operations via the generic payload queue.
    """

    def test_v2_v3_weth_usdc_to_weth(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v2_pair: ContractInstance,
        v3_pool: ContractInstance,
    ):
        """
        WETH→USDC at V2 (flash borrow), USDC→WETH at V3.

        V2_A sends USDC to executor (optimistic), callbacks executor.
        Inside V2 callback:
          - V3_B swap receives USDC (from executor), sends WETH
          - V3_B callbacks executor (nested V3 callback)
          - V3 callback: V3_B is owed USDC (not WETH) → NO auto-pay
          - Transfer USDC to V3_B
          - Transfer WETH to V2_A to repay flash borrow
        """
        # ── Amounts ──
        v2_amount_in = 1 * 10**18  # 1 WETH into V2
        forward_out = 2000 * 10**6  # 2000 USDC from V2
        v3_amount_in = forward_out  # 2000 USDC into V3
        v3_amount_out = 2 * 10**18  # 2 WETH from V3

        # ── Set up V2 swap: WETH→USDC ──
        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == weth.address

        usdc.mint(v2_pair.address, forward_out, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, forward_out, v2_zfo, sender=owner_account)

        # ── Set up V3 swap: USDC→WETH ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == usdc.address  # selling USDC (token0?) → zfo

        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Build payloads ──
        # V3_B is owed USDC → no auto-pay → must transfer explicitly
        # V2 has no auto-pay → must transfer WETH explicitly
        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)

        payloads = [
            # 0: V2 flash swap (sends USDC to executor, callbacks)
            (v2_pair.address, encode_v2_swap_calldata(
                zero_for_one=v2_zfo,
                amount_out=forward_out,
                recipient=executor.address,
                flash_borrow=True,
            ), 0, True),
            # 1: V3 swap (sends WETH to executor, callbacks)
            (v3_pool.address, encode_v3_swap_calldata(
                recipient=executor.address,
                zero_for_one=v3_zfo,
                amount_specified=v3_amount_in,
                sqrt_price_limit_x96=v3_sqrt_limit,
            ), 0, True),
            # 2: Transfer USDC to V3_B (pay its non-WETH debt)
            (usdc.address, encode_erc20_transfer_calldata(
                recipient=v3_pool.address,
                amount=v3_amount_in,
            ), 0, False),
            # 3: Transfer WETH to V2_A (repay flash borrow)
            (weth.address, encode_erc20_transfer_calldata(
                recipient=v2_pair.address,
                amount=v2_amount_in,
            ), 0, False),
        ]

        # ── Execute ──
        tx = executor.execute_payloads(
            payloads,
            [],
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


# ── Tests: V3 → V2 ──────────────────────────────────────────


class TestV3ToV2:
    """
    V3→V2 paths: V3 callback is the flash borrow mechanism,
    V2 swap is either direct (no callback) or flash.
    Auto-pay handles WETH debts to V3 when applicable.

    v4_swaps=[] — all operations via the generic payload queue.
    """

    def test_v3_v2_direct_swap_zfo_true(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v2_pair: ContractInstance,
        v3_pool: ContractInstance,
    ):
        """
        WETH→USDC at V3, USDC→WETH at V2 (direct swap, no callback).

        V3_A sends USDC to executor, callbacks executor.
        Inside V3 callback:
          - Transfer USDC to V2_B
          - V2_B direct swap (no callback) sends WETH to executor
          - V3 callback auto-pays WETH to V3_A after payload delivery
        """
        # ── Amounts ──
        v3_amount_in = 1 * 10**18  # 1 WETH into V3
        forward_out = 2000 * 10**6  # 2000 USDC from V3
        v2_amount_in = forward_out  # 2000 USDC into V2
        v2_amount_out = 2 * 10**18  # 2 WETH from V2

        # ── Set up V3 swap: WETH→USDC ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == weth.address

        usdc.mint(v3_pool.address, forward_out, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, forward_out, v3_zfo, sender=owner_account)

        # ── Set up V2 swap: USDC→WETH ──
        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == usdc.address

        weth.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        usdc.mint(v2_pair.address, v2_amount_in, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Build payloads ──
        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)

        payloads = [
            # 0: V3 swap (sends USDC to executor, callbacks)
            (v3_pool.address, encode_v3_swap_calldata(
                recipient=executor.address,
                zero_for_one=v3_zfo,
                amount_specified=v3_amount_in,
                sqrt_price_limit_x96=v3_sqrt_limit,
            ), 0, True),
            # 1: Transfer USDC to V2
            (usdc.address, encode_erc20_transfer_calldata(
                recipient=v2_pair.address,
                amount=forward_out,
            ), 0, False),
            # 2: V2 direct swap (sends WETH to executor, no callback)
            (v2_pair.address, encode_v2_swap_calldata(
                zero_for_one=v2_zfo,
                amount_out=v2_amount_out,
                recipient=executor.address,
                flash_borrow=False,
            ), 0, False),
        ]
        # No explicit WETH transfer to V3 — auto-pay handles it

        # ── Execute ──
        tx = executor.execute_payloads(
            payloads,
            [],
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

    def test_v3_v2_weth_transfer_to_v2_zfo_false(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v2_pair: ContractInstance,
        v3_pool: ContractInstance,
    ):
        """
        WETH→USDC at V3, USDC→WETH at V2 with explicit WETH transfer to V2.

        This variant tests the case where V2 needs WETH (zfo=False on the
        V2 pair means WETH=token0 and the swap output is token0), requiring
        an explicit WETH transfer to V2 before/during the swap.

        V3 sends USDC to executor, callbacks executor.
        Inside V3 callback:
          - V2_B swap (flash borrow) sends USDC to executor, callbacks executor
          - Inside V2 callback: transfer WETH to V2_B (repay flash borrow)
          - V3 callback auto-pays WETH to V3_A after all payloads delivered
        """
        # ── Amounts ──
        v3_amount_in = 1 * 10**18  # 1 WETH into V3
        forward_from_v3 = 2000 * 10**6  # 2000 USDC from V3
        v2_amount_in = 500 * 10**6  # 500 USDC into V2 (V2 flash borrows less)
        v2_amount_out = 1 * 10**18  # 1 WETH from V2

        # ── Set up V3 swap: WETH→USDC ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == weth.address

        usdc.mint(v3_pool.address, forward_from_v3, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, forward_from_v3, v3_zfo, sender=owner_account)

        # ── Set up V2 swap: USDC→WETH (flash borrow WETH) ──
        # We construct a scenario where V2 flash borrows WETH to the executor
        # and executor must transfer WETH back to V2.
        v2_token0 = v2_pair.token0()
        # If WETH is token1 (USDC < WETH):
        # zfo for "USDC in, WETH out" = v2_token0 == usdc → True (selling token0, getting token1)
        v2_zfo = v2_token0 == usdc.address

        weth.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        usdc.mint(v2_pair.address, v2_amount_in, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Build payloads ──
        # V3 swap → V2 flash swap → WETH transfer to V2 → auto-pay WETH to V3
        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)

        payloads = [
            # 0: V3 swap (sends USDC to executor, callbacks)
            (v3_pool.address, encode_v3_swap_calldata(
                recipient=executor.address,
                zero_for_one=v3_zfo,
                amount_specified=v3_amount_in,
                sqrt_price_limit_x96=v3_sqrt_limit,
            ), 0, True),
            # 1: Transfer USDC to V2
            (usdc.address, encode_erc20_transfer_calldata(
                recipient=v2_pair.address,
                amount=v2_amount_in,
            ), 0, False),
            # 2: V2 swap (sends WETH to executor, callbacks for flash borrow repayment)
            (v2_pair.address, encode_v2_swap_calldata(
                zero_for_one=v2_zfo,
                amount_out=v2_amount_out,
                recipient=executor.address,
                flash_borrow=True,
            ), 0, True),
            # 3: WETH transfer to V2 (pay flash borrow)
            (weth.address, encode_erc20_transfer_calldata(
                recipient=v2_pair.address,
                amount=v2_amount_in,
            ), 0, False),
        ]
        # Auto-pay for V3's WETH debt happens after all payloads are delivered.

        # ── Execute ──
        tx = executor.execute_payloads(
            payloads,
            [],
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
