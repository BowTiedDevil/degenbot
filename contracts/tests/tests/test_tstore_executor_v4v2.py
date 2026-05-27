"""
Tests for tstore_executor V4-V2 and V2-V4 swap execution.

Verifies that:
1. V4→V2 paths settle without CurrencyNotSettled (forward token taken from PM, routed to V2)
2. V2→V4 paths settle without CurrencyNotSettled (forward token transferred to PM, synced/settled, V4 swap)
3. WETH/ETH currency mismatch is handled correctly
4. Intermediate ERC-20 tokens cancel or are settled properly

These tests exercise the HYBRID V4 settlement + V2 payload queue path.
The unlockCallback must:
  V4→V2: Phase 1 V4 swap → Phase 2 take+V2 swap → Phase 3 auto-settle remaining
  V2→V4: Phase 2 V2 swap+transfer+sync → Phase 0 settle → Phase 1 V4 swap → Phase 3 auto-settle

Uses fake contracts (fake_erc20, fake_weth, fake_uniswap_v2_pair,
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

# ABI selectors
V2_SWAP_SELECTOR = keccak(text="swap(uint256,uint256,address,bytes)")[:4]
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


def encode_weth_deposit_calldata() -> bytes:
    return bytes(WETH_DEPOSIT_SELECTOR)


# ── V4 helpers (shared with V4-V4/V4-V3 tests) ──────────────


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
    # Fund the executor with ETH for settlement
    contract.balance = 1000 * 10**18
    return contract


# ── Tests: V4 → V2 ──────────────────────────────────────────


class TestV4ToV2:
    """
    V4→V2 paths: V4 swap runs in unlockCallback Phase 1,
    then take(forward) + transfer + V2 flash swap run in Phase 2,
    then auto-settlement handles remaining deltas in Phase 3.

    Flow: WETH→USDC@V4, USDC→WETH@V2
    - V4 swap debits WETH from executor, credits USDC
    - Take USDC from PM → transfer to V2 pair → V2 swap (callback)
    - V2 callback resumes payloads (nothing left), then V2 checks input paid
    - Auto-settlement settles the WETH debt from V4 swap
    """

    def test_v4_v2_weth_usdc_to_weth(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
        v2_pair: ContractInstance,
    ):
        """WETH→USDC at V4, then USDC→WETH at V2."""
        # ── Amounts ──
        v4_amount_in = 1 * 10**18  # 1 WETH in
        forward_out = 2000 * 10**6  # 2000 USDC out from V4
        v2_amount_in = forward_out  # 2000 USDC in to V2
        v2_amount_out = 2 * 10**18  # 2 WETH out from V2

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

        # ── Set up V2 swap: USDC→WETH ──
        # V2 token ordering
        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == usdc.address  # selling USDC (token0) → zfo=True

        # Pre-fund V2 pair with WETH for the swap output
        weth.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        # Pre-fund V2 pair with USDC so balance check works after transfer
        usdc.mint(v2_pair.address, v2_amount_in, sender=owner_account)

        v2_pair.set_next_swap(
            v2_amount_in,
            v2_amount_out,
            v2_zfo,
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

        # 3. Transfer USDC from executor to V2 pair
        transfer_calldata = encode_erc20_transfer_calldata(
            recipient=v2_pair.address,
            amount=forward_out,
        )

        # 4. V2 flash swap (sends WETH to executor, callbacks, then checks USDC paid)
        v2_swap_data = encode_v2_swap_calldata(
            zero_for_one=v2_zfo,
            amount_out=v2_amount_out,
            recipient=executor.address,
            flash_borrow=True,
        )

        payloads = [
            (pm, unlock_calldata, 0, True),                 # unlock
            (pm, take_calldata, 0, False),                  # take USDC from PM
            (usdc.address, transfer_calldata, 0, False),   # transfer USDC to V2
            (v2_pair.address, v2_swap_data, 0, True),      # V2 flash swap (callback)
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


class TestV4ToV2WrongAmountOut:
    """
    Regression test: the Python encode_v4v2_payloads() used to pass
    forward_out (intermediate token amount) as amount_out to the V2 swap,
    instead of weth_out (the actual WETH output from V2).

    The V2 swap(amount0Out, amount1Out, ...) specifies how much of each
    token V2 sends to the recipient. For USDC→WETH@V2, V2 sends WETH,
    so amount_out must be the WETH amount, not the USDC amount.

    This test verifies that passing the wrong amount_out causes the
    transaction to revert, catching the encoding bug.
    """

    def test_v4_v2_with_wrong_amount_out_reverts(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
        v2_pair: ContractInstance,
    ):
        """Pass forward_out (USDC amount) to V2 swap instead of weth_out — must revert."""
        # ── Amounts ──
        v4_amount_in = 1 * 10**18  # 1 WETH in
        forward_out = 2000 * 10**6  # 2000 USDC out from V4
        v2_amount_in = forward_out  # 2000 USDC in to V2
        v2_amount_out = 2 * 10**18  # 2 WETH out from V2 (the CORRECT amount)

        # ── Set up V4 swap: WETH→USDC ──
        v4_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            v4_key,
            v4_amount_in,
            forward_out,
            v4_zfo,
            output_token=usdc,
        )

        # ── Set up V2 swap: USDC→WETH ──
        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == usdc.address

        weth.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        usdc.mint(v2_pair.address, v2_amount_in, sender=owner_account)

        v2_pair.set_next_swap(
            v2_amount_in,
            v2_amount_out,  # Configures V2 to output 2 WETH
            v2_zfo,
            sender=owner_account,
        )

        # ── Build payloads with WRONG amount_out ──
        pm = v4_pool_manager.address

        unlock_calldata = encode_v4_unlock_calldata(b"")
        take_calldata = encode_v4_take_calldata(
            currency=usdc.address,
            to=executor.address,
            amount=forward_out,
        )
        transfer_calldata = encode_erc20_transfer_calldata(
            recipient=v2_pair.address,
            amount=forward_out,
        )

        # BUG: amount_out=forward_out (2000 USDC) instead of v2_amount_out (2 WETH)
        # V2 expects amount1Out == v2_amount_out but gets forward_out → assertion fails
        v2_swap_data_wrong = encode_v2_swap_calldata(
            zero_for_one=v2_zfo,
            amount_out=forward_out,  # WRONG! Should be v2_amount_out
            recipient=executor.address,
            flash_borrow=True,
        )

        payloads = [
            (pm, unlock_calldata, 0, True),
            (pm, take_calldata, 0, False),
            (usdc.address, transfer_calldata, 0, False),
            (v2_pair.address, v2_swap_data_wrong, 0, True),
        ]

        sqrt_limit_v4 = MIN_SQRT_PRICE_X96 + 1 if v4_zfo else MAX_SQRT_PRICE_X96 - 1
        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_key,
                v4_zfo,
                -v4_amount_in,
                sqrt_limit_v4,
                dynamic_amount=False,
            ),
        ]

        # ── Execute — must revert ──
        tx = executor.execute_payloads(
            payloads,
            v4_swaps,
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        # The transaction MUST revert with the wrong amount_out
        assert tx.status == 0, "Transaction should revert with wrong amount_out"


class TestV2ToV4:
    """
    V2→V4 paths: V2 flash swap runs as a payload (callback),
    then transfer forward to PM, sync, unlock, then V4 swap runs
    in unlockCallback.

    Flow: WETH→USDC@V2, USDC→WETH@V4
    - V2 flash swap: V2 sends USDC to executor (optimistic), callbacks
    - Transfer USDC to PM → sync → unlock()
    - unlockCallback Phase 0: settle USDC (pre-settle)
    - Phase 1: V4 swap consumes USDC, produces WETH/ETH
    - Phase 3: settle remaining deltas
    """

    def test_v2_v4_weth_to_usdc_usdc_to_weth(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
        v2_pair: ContractInstance,
    ):
        """WETH→USDC at V2, then USDC→WETH at V4."""
        # ── Amounts ──
        v2_amount_in = 1 * 10**18  # 1 WETH in to V2
        forward_out = 2000 * 10**6  # 2000 USDC out from V2
        v4_amount_out = 2 * 10**18  # 2 WETH out from V4

        # ── Set up V2 swap: WETH→USDC ──
        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == weth.address  # True if WETH is token0

        # Pre-fund V2 pair with USDC for the swap output
        usdc.mint(v2_pair.address, forward_out, sender=owner_account)

        v2_pair.set_next_swap(
            v2_amount_in,
            forward_out,
            v2_zfo,
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

        # 1. V2 flash swap (sends USDC to executor, callbacks)
        v2_swap_data = encode_v2_swap_calldata(
            zero_for_one=v2_zfo,
            amount_out=forward_out,
            recipient=executor.address,
            flash_borrow=True,
        )

        # 2. Transfer USDC from executor to PM
        transfer_to_pm = encode_erc20_transfer_calldata(
            recipient=pm,
            amount=forward_out,
        )

        # 3. Sync USDC at PM (BEFORE transfer — records PM's zero balance)
        sync_calldata = encode_v4_sync_calldata(usdc.address)

        # 4. Unlock PM → triggers unlockCallback
        unlock_calldata = encode_v4_unlock_calldata(b"")

        # 5. Transfer WETH from executor to V2 pair (pay for flash borrow)
        #    V4 swap produced WETH via take() in Phase 3. Now pay V2.
        weth_transfer_to_v2 = encode_erc20_transfer_calldata(
            recipient=v2_pair.address,
            amount=v2_amount_in,
        )

        payloads = [
            (v2_pair.address, v2_swap_data, 0, True),      # V2 flash swap
            (pm, sync_calldata, 0, False),                  # Sync USDC (BEFORE transfer — records zero balance)
            (usdc.address, transfer_to_pm, 0, False),      # Transfer USDC to PM
            (pm, unlock_calldata, 0, True),                 # Unlock PM
            (weth.address, weth_transfer_to_v2, 0, False), # Pay WETH to V2
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
