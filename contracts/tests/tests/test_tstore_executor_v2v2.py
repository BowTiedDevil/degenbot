"""
Tests for tstore_executor V2-V2 swap execution.

Verifies that:
1. V2→V2 paths settle correctly (flash borrow + direct swap + WETH repayment)
2. The V2 callback resumes payload delivery correctly
3. WETH is transferred to the V2 pair to repay the flash borrow

Token flow (V2_A(zfo=True) → V2_B(zfo=False)):
  V2_A: WETH_in → USDC_out (to executor via flash borrow)
  V2_B: USDC_in → WETH_out (to executor via direct swap, no callback)
  Pay V2_A: WETH transfer inside V2_A's callback

Payload sequence:
  0: V2_A.swap(0, forward_out, executor, data) [will_callback=True]
     → V2_A sends USDC to executor, then calls executor's V2 callback
  1: USDC.transfer(V2_B, amount) [will_callback=False]
     → executor sends USDC to V2_B (delivered inside V2_A callback)
  2: V2_B.swap(WETH_out, 0, executor, b"") [will_callback=False]
     → V2_B sends WETH to executor (no callback needed)
  3: WETH.transfer(V2_A, amount) [will_callback=False]
     → executor sends WETH to V2_A to pay flash borrow
     → (delivered inside V2_A callback, before invariant check)

Uses fake contracts (fake_erc20, fake_weth, fake_uniswap_v2_pair) to
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

# ABI selectors
V2_SWAP_SELECTOR = keccak(text="swap(uint256,uint256,address,bytes)")[:4]
ERC20_TRANSFER_SELECTOR = keccak(text="transfer(address,uint256)")[:4]


# ── Encoding helpers ─────────────────────────────────────────


def encode_v2_swap_calldata(
    zero_for_one: bool,
    amount_out: int,
    recipient: str,
    flash_borrow: bool = True,
) -> bytes:
    """
    Encode a Uniswap V2 pool swap(uint256,uint256,address,bytes) call.

    zfo=True → (0, amount_out) → token1 comes out
    zfo=False → (amount_out, 0) → token0 comes out

    flash_borrow=True passes non-empty data to trigger V2 callback.
    """
    amount0_out, amount1_out = (0, amount_out) if zero_for_one else (amount_out, 0)
    data = b"\x01" if flash_borrow else b""
    return bytes(V2_SWAP_SELECTOR) + eth_abi.encode(
        types=["uint256", "uint256", "address", "bytes"],
        args=[amount0_out, amount1_out, recipient, data],
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
def v2_pair_a(
    project: ProjectManager,
    owner_account: TestAccount,
    usdc: ContractInstance,
    weth: ContractInstance,
) -> ContractInstance:
    """Fake V2 pair A for USDC/WETH swaps (the flash borrow source)."""
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
def v2_pair_b(
    project: ProjectManager,
    owner_account: TestAccount,
    usdc: ContractInstance,
    weth: ContractInstance,
) -> ContractInstance:
    """Fake V2 pair B for USDC/WETH swaps (the direct swap target)."""
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
    # Fund the executor with ETH for WETH wrapping if needed
    contract.balance = 1000 * 10**18
    return contract


# ── Tests ──────────────────────────────────────────────────────


class TestV2V2:
    """
    V2→V2 paths: V2_A flash borrows forward token to executor,
    executor transfers forward to V2_B, V2_B sends WETH to executor,
    executor transfers WETH to V2_A to repay flash borrow.

    v4_swaps=[] — all operations via the generic payload queue.
    """

    def test_v2_v2_flash_borrow_direct_swap_repayment(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v2_pair_a: ContractInstance,
        v2_pair_b: ContractInstance,
    ):
        """
        WETH→USDC at V2_A (flash borrow), USDC→WETH at V2_B (direct swap).

        V2_A sends USDC to executor (optimistic), callbacks executor.
        Inside V2_A callback:
          - Transfer USDC to V2_B
          - V2_B direct swap (no callback) sends WETH to executor
          - Transfer WETH to V2_A to repay flash borrow
        V2_A checks balance after callback — WETH must be there.
        """
        # ── Amounts ──
        v2_a_amount_in = 1 * 10**18  # 1 WETH in to V2_A
        forward_out = 2000 * 10**6  # 2000 USDC out from V2_A
        v2_b_amount_in = forward_out  # 2000 USDC in to V2_B
        v2_b_amount_out = 2 * 10**18  # 2 WETH out from V2_B

        # ── Set up V2_A swap: WETH→USDC ──
        v2_a_token0 = v2_pair_a.token0()
        v2_a_zfo = v2_a_token0 == weth.address  # sending WETH (token0?) → zfo

        # Pre-fund V2_A with USDC for the swap output
        usdc.mint(v2_pair_a.address, forward_out, sender=owner_account)

        v2_pair_a.set_next_swap(
            v2_a_amount_in,
            forward_out,
            v2_a_zfo,
            sender=owner_account,
        )

        # ── Set up V2_B swap: USDC→WETH ──
        v2_b_token0 = v2_pair_b.token0()
        v2_b_zfo = v2_b_token0 == usdc.address  # selling USDC (token0?) → zfo

        # Pre-fund V2_B with WETH for the swap output
        weth.mint(v2_pair_b.address, v2_b_amount_out, sender=owner_account)

        v2_pair_b.set_next_swap(
            v2_b_amount_in,
            v2_b_amount_out,
            v2_b_zfo,
            sender=owner_account,
        )

        # ── Build payloads ──
        # 0: V2_A flash swap (sends USDC to executor, callbacks executor)
        v2_a_swap_data = encode_v2_swap_calldata(
            zero_for_one=v2_a_zfo,
            amount_out=forward_out,
            recipient=executor.address,
            flash_borrow=True,
        )

        # 1: Transfer USDC from executor to V2_B
        transfer_usdc_to_b = encode_erc20_transfer_calldata(
            recipient=v2_pair_b.address,
            amount=forward_out,
        )

        # 2: V2_B direct swap (sends WETH to executor, no callback)
        v2_b_swap_data = encode_v2_swap_calldata(
            zero_for_one=v2_b_zfo,
            amount_out=v2_b_amount_out,
            recipient=executor.address,
            flash_borrow=False,  # Direct swap — no callback
        )

        # 3: Transfer WETH from executor to V2_A (repay flash borrow)
        weth_transfer_to_a = encode_erc20_transfer_calldata(
            recipient=v2_pair_a.address,
            amount=v2_a_amount_in,
        )

        payloads = [
            (v2_pair_a.address, v2_a_swap_data, 0, True),          # V2_A flash swap (callback)
            (usdc.address, transfer_usdc_to_b, 0, False),          # Transfer USDC to V2_B
            (v2_pair_b.address, v2_b_swap_data, 0, False),         # V2_B direct swap
            (weth.address, weth_transfer_to_a, 0, False),          # Pay WETH to V2_A
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
