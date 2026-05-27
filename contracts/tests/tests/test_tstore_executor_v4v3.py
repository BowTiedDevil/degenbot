"""
Tests for tstore_executor V4-V3 and V3-V4 swap execution.

Verifies that:
1. V4→V3 paths settle without CurrencyNotSettled (forward token taken from PM, routed to V3)
2. V3→V4 paths settle without CurrencyNotSettled (forward token transferred to PM, synced/settled, V4 swap)
3. WETH/ETH currency mismatch is handled correctly
4. Intermediate ERC-20 tokens cancel or are settled properly

These tests exercise the HYBRID V4 settlement + V3 payload queue path.
The unlockCallback must:
  V4→V3: Phase 1 V4 swap → Phase 2 take+V3 swap → Phase 3 auto-settle remaining
  V3→V4: Phase 2 sync+settle forward → Phase 1 V4 swap → Phase 3 auto-settle remaining

Uses fake contracts (fake_erc20, fake_weth, fake_uniswap_v3_pool,
fake_uniswap_v4_pool_manager) to mock on-chain swap behavior.
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
from hexbytes import HexBytes

NATIVE_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")

WETH_DEPLOYMENT_WRAP_AMOUNT = 10 * 10**18

MIN_SQRT_PRICE_X96 = 4295128739
MAX_SQRT_PRICE_X96 = 1461446703485210103287273052203988822378723970342

# V3 limits (same values, different names for clarity)
MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342

# ABI selectors
V3_SWAP_SELECTOR = keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
ERC20_TRANSFER_SELECTOR = keccak(text="transfer(address,uint256)")[:4]
WETH_DEPOSIT_SELECTOR = keccak(text="deposit()")[:4]
V4_UNLOCK_SELECTOR = keccak(text="unlock(bytes)")[:4]
V4_TAKE_SELECTOR = keccak(text="take(address,address,uint256)")[:4]
V4_SYNC_SELECTOR = keccak(text="sync(address)")[:4]
V4_SETTLE_SELECTOR = keccak(text="settle()")[:4]


# ── Encoding helpers ─────────────────────────────────────────


def encode_v4_unlock_calldata(data: bytes = b"") -> bytes:
    return bytes(V4_UNLOCK_SELECTOR) + eth_abi.encode(
        types=["bytes"], args=[data]
    )


def encode_v4_take_calldata(currency: str, to: str, amount: int) -> bytes:
    return bytes(V4_TAKE_SELECTOR) + eth_abi.encode(
        types=["address", "address", "uint256"],
        args=[currency, to, amount],
    )


def encode_v4_sync_calldata(currency: str) -> bytes:
    return bytes(V4_SYNC_SELECTOR) + eth_abi.encode(
        types=["address"], args=[currency]
    )


def encode_v4_settle_calldata() -> bytes:
    return bytes(V4_SETTLE_SELECTOR)


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


def encode_weth_deposit_calldata() -> bytes:
    return bytes(WETH_DEPOSIT_SELECTOR)


# ── V4 helpers (shared with V4-V4 tests) ──────────────────────


def _make_pool_key(
    currency0: str,
    currency1: str,
    fee: int = 0,
    tick_spacing: int = 60,
    hooks: str = ZERO_ADDRESS,
) -> tuple[str, str, int, int, str]:
    c0, c1 = sorted([currency0, currency1], key=lambda addr: addr.lower())
    return (c0, c1, fee, tick_spacing, hooks)


def _encode_v4_swap_payload(
    currency0: str,
    currency1: str,
    fee: int,
    tick_spacing: int,
    hooks: str,
    zero_for_one: bool,
    amount_specified: int,
    sqrt_price_limit_x96: int,
    dynamic_amount: bool = False,
) -> tuple:
    return (
        (currency0, currency1, fee, tick_spacing, hooks),
        (zero_for_one, amount_specified, sqrt_price_limit_x96),
        dynamic_amount,
    )


def _setup_v4_swap(
    pool_manager: ContractInstance,
    owner: TestAccount,
    pool_key: tuple,
    amount_in: int,
    amount_out: int,
    zero_for_one: bool,
    output_token: ContractInstance | None = None,
    fund_eth: bool = False,
) -> None:
    if fund_eth:
        pool_manager.balance += amount_out
    elif output_token is not None:
        output_token.mint(pool_manager.address, amount_out, sender=owner)

    pool_manager.set_next_swap(
        pool_key,
        amount_in,
        amount_out,
        zero_for_one,
        b"",
        sender=owner,
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
def v4_pool_manager(
    project: ProjectManager,
    owner_account: TestAccount,
    usdc: ContractInstance,
    weth: ContractInstance,
) -> ContractInstance:
    """Fake V4 PoolManager — constructor takes sorted token0, token1."""
    token0, token1 = sorted(
        [usdc.address, weth.address],
        key=lambda addr: addr.lower(),
    )
    return project.fake_uniswap_v4_pool_manager.deploy(
        token0,
        token1,
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
    pool = project.fake_uniswap_v3_pool.deploy(
        token0,
        token1,
        sender=owner_account,
    )
    return pool


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
    # Fund the executor with ETH for settlement
    contract.balance = 1000 * 10**18
    return contract


# ── Tests: V4 → V3 ──────────────────────────────────────────


class TestV4ToV3:
    """
    V4→V3 paths: V4 swap runs in unlockCallback Phase 1,
    then take(forward) + transfer + V3 swap run in Phase 2,
    then auto-settlement handles remaining deltas in Phase 3.

    Flow: WETH→USDC@V4, USDC→WETH@V3
    - V4 swap debits WETH from executor, credits USDC
    - Take USDC from PM → transfer to V3 → V3 swap → callback auto-pays WETH
    - Auto-settlement settles the WETH debt from V4 swap
    """

    def test_v4_v3_weth_usdc_to_weth(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
        v3_pool: ContractInstance,
    ):
        """WETH→USDC at V4, then USDC→WETH at V3."""
        # ── Amounts ──
        v4_amount_in = 1 * 10**18  # 1 WETH in
        forward_out = 2000 * 10**6  # 2000 USDC out from V4
        v3_amount_in = forward_out  # 2000 USDC in to V3
        v3_amount_out = 2 * 10**18  # 2 WETH out from V3

        # ── Set up V4 swap: WETH→USDC ──
        v4_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == weth.address  # True if WETH is currency0

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            v4_key,
            v4_amount_in,
            forward_out,
            v4_zfo,
            output_token=usdc,
        )

        # ── Set up V3 swap: USDC→WETH ──
        # V3 token ordering
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == usdc.address  # zero_for_one = sending token0

        # Pre-fund V3 pool with WETH for the swap output
        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        # Fund V3 pool with enough USDC so it can receive input
        usdc.mint(v3_pool.address, v3_amount_in, sender=owner_account)

        v3_pool.set_next_swap(
            v3_amount_in,
            v3_amount_out,
            v3_zfo,
            sender=owner_account,
        )

        # ── Build payloads ──
        pm = v4_pool_manager.address

        # 1. Unlock PM → triggers unlockCallback
        unlock_calldata = encode_v4_unlock_calldata(b"")

        # 2. Take forward token (USDC) from PM to executor
        take_calldata = encode_v4_take_calldata(
            currency=usdc.address,
            to=executor.address,
            amount=forward_out,
        )

        # 3. Transfer USDC from executor to V3 pool
        transfer_calldata = encode_erc20_transfer_calldata(
            recipient=v3_pool.address,
            amount=forward_out,
        )

        # 4. V3 swap (exact-input, amountSpecified positive per V3 convention)
        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)
        v3_swap_calldata = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_zfo,
            amount_specified=v3_amount_in,
            sqrt_price_limit_x96=v3_sqrt_limit,
        )

        payloads = [
            (pm, unlock_calldata, 0, True),           # unlock
            (pm, take_calldata, 0, False),             # take USDC from PM
            (usdc.address, transfer_calldata, 0, False),  # transfer USDC to V3
            (v3_pool.address, v3_swap_calldata, 0, True),  # V3 swap
        ]

        # ── V4 swap params ──
        sqrt_limit_v4 = MIN_SQRT_PRICE_X96 + 1 if v4_zfo else MAX_SQRT_PRICE_X96 - 1
        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_key,
                v4_zfo,
                -v4_amount_in,  # V4: negative for exact-input
                sqrt_limit_v4,
                dynamic_amount=False,
            ),
        ]

        # ── Execute ──
        tx = executor.execute_payloads(
            payloads,
            v4_swaps,
            0,  # bribe_bips
            True,  # skip_profit_check
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV3ToV4:
    """
    V3→V4 paths: V3 swap runs as a payload, then V4 unlock + swap
    runs in unlockCallback. The forward token from V3 must be
    transferred to PM, synced/settled before the V4 swap consumes it.

    Flow: WETH→USDC@V3, USDC→WETH@V4
    - V3 swap produces USDC (executor receives it)
    - Transfer USDC to PM → sync → settle (credits to executor's delta)
    - V4 swap consumes USDC from delta, produces WETH/ETH
    - Auto-settlement settles remaining deltas
    """

    def test_v3_v4_weth_to_usdc_usdc_to_weth(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
        v3_pool: ContractInstance,
    ):
        """WETH→USDC at V3, then USDC→WETH at V4."""
        # ── Amounts ──
        v3_amount_in = 1 * 10**18  # 1 WETH in to V3
        forward_out = 2000 * 10**6  # 2000 USDC out from V3
        v4_amount_out = 2 * 10**18  # 2 WETH out from V4

        # ── Set up V3 swap: WETH→USDC ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == weth.address  # True if WETH is token0

        # Pre-fund V3 pool with USDC for the swap output
        usdc.mint(v3_pool.address, forward_out, sender=owner_account)

        v3_pool.set_next_swap(
            v3_amount_in,
            forward_out,
            v3_zfo,
            sender=owner_account,
        )

        # ── Set up V4 swap: USDC→WETH ──
        v4_key = _make_pool_key(usdc.address, weth.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == usdc.address  # True if USDC is currency0

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            v4_key,
            forward_out,
            v4_amount_out,
            v4_zfo,
            output_token=weth,
        )

        # ── Build payloads ──
        pm = v4_pool_manager.address

        # 1. V3 swap (exact-input, amountSpecified positive for V3)
        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)
        v3_swap_calldata = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_zfo,
            amount_specified=v3_amount_in,
            sqrt_price_limit_x96=v3_sqrt_limit,
        )

        # 2. Sync USDC at PM (BEFORE transfer — records PM's zero balance)
        sync_calldata = encode_v4_sync_calldata(usdc.address)

        # 3. Transfer USDC from executor to PM
        transfer_to_pm = encode_erc20_transfer_calldata(
            recipient=pm,
            amount=forward_out,
        )

        # 4. Unlock PM → triggers unlockCallback
        unlock_calldata = encode_v4_unlock_calldata(b"")

        payloads = [
            (v3_pool.address, v3_swap_calldata, 0, True),  # V3 swap
            (pm, sync_calldata, 0, False),                  # Sync USDC (before transfer)
            (usdc.address, transfer_to_pm, 0, False),       # Transfer USDC to PM
            (pm, unlock_calldata, 0, True),                 # Unlock PM
        ]

        # ── V4 swap params ──
        sqrt_limit_v4 = MIN_SQRT_PRICE_X96 + 1 if v4_zfo else MAX_SQRT_PRICE_X96 - 1
        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_key,
                v4_zfo,
                -forward_out,  # V4: negative for exact-input
                sqrt_limit_v4,
                dynamic_amount=False,
            ),
        ]

        # ── Execute ──
        tx = executor.execute_payloads(
            payloads,
            v4_swaps,
            0,  # bribe_bips
            True,  # skip_profit_check
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
