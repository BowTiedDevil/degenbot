"""
Tests for tstore_executor edge cases:
- Callback variant selectors (hook, pancakeCall, pancakeV3SwapCallback)
- Settlement branches (native ETH output, V2 direct swap, V3 non-WETH no-pay)
- Encoding regression tests (sign convention, amount_out mismatches)

These tests validate that the executor's per-selector callback handlers
(t_allowed_callback_addresses guards) and per-currency settlement branches
(_v4_settle_currency native/WETH/ERC-20 paths) work correctly.

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

MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342
MIN_SQRT_PRICE_X96 = 4295128739
MAX_SQRT_PRICE_X96 = 1461446703485210103287273052203988822378723970342

# ABI selectors
V2_SWAP_SELECTOR = keccak(text="swap(uint256,uint256,address,bytes)")[:4]
V3_SWAP_SELECTOR = keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
ERC20_TRANSFER_SELECTOR = keccak(text="transfer(address,uint256)")[:4]
V4_UNLOCK_SELECTOR = keccak(text="unlock(bytes)")[:4]
V4_TAKE_SELECTOR = keccak(text="take(address,address,uint256)")[:4]
V4_SYNC_SELECTOR = keccak(text="sync(address)")[:4]
V4_SETTLE_SELECTOR = keccak(text="settle()")[:4]


# ── Encoding helpers ─────────────────────────────────────────


def encode_v2_swap_calldata(
    zero_for_one: bool,
    amount_out: int,
    recipient: str,
    flash_borrow: bool = True,
) -> bytes:
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
    return bytes(V3_SWAP_SELECTOR) + eth_abi.encode(
        types=["address", "bool", "int256", "uint160", "bytes"],
        args=[recipient, zero_for_one, amount_specified, sqrt_price_limit_x96, b""],
    )


def encode_erc20_transfer_calldata(recipient: str, amount: int) -> bytes:
    return bytes(ERC20_TRANSFER_SELECTOR) + eth_abi.encode(
        types=["address", "uint256"], args=[recipient, amount]
    )


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


# ── V4 helpers ──────────────────────────────────────────────


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


# ── Tests: Callback Variants ─────────────────────────────────


class TestHookCallback:
    """
    Verify that the executor's hook() callback entry point works.
    Velodrome/Aerodrome V2 pairs use hook() instead of uniswapV2Call().
    The executor's hook() calls _deliver_remaining_payloads() just like
    uniswapV2Call(), but the t_allowed_callback_addresses guard
    must be checked against the correct msg.sender.
    """

    def test_hook_callback_settles(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        V2→V2 path using hook() callback selector.

        Uses fake_uniswap_v2_pair with callback_variant=1 (hook).
        Everything else is identical to the standard V2→V2 test.
        """
        # Deploy V2 pair A with hook callback variant
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        v2_pair_a = project.fake_uniswap_v2_pair.deploy(
            token0, token1,
            1,  # callback_variant: 1=hook
            sender=owner_account,
        )
        v2_pair_b = project.fake_uniswap_v2_pair.deploy(
            token0, token1,
            0,  # callback_variant: 0=uniswapV2Call (doesn't matter, no callback)
            sender=owner_account,
        )

        # ── Amounts ──
        v2_a_amount_in = 1 * 10**18
        forward_out = 2000 * 10**6
        v2_b_amount_out = 2 * 10**18

        v2_a_token0 = v2_pair_a.token0()
        v2_a_zfo = v2_a_token0 == weth.address

        usdc.mint(v2_pair_a.address, forward_out, sender=owner_account)
        v2_pair_a.set_next_swap(v2_a_amount_in, forward_out, v2_a_zfo, sender=owner_account)

        v2_b_token0 = v2_pair_b.token0()
        v2_b_zfo = v2_b_token0 == usdc.address

        weth.mint(v2_pair_b.address, v2_b_amount_out, sender=owner_account)
        usdc.mint(v2_pair_b.address, forward_out, sender=owner_account)
        v2_pair_b.set_next_swap(forward_out, v2_b_amount_out, v2_b_zfo, sender=owner_account)

        payloads = [
            (v2_pair_a.address, encode_v2_swap_calldata(
                zero_for_one=v2_a_zfo,
                amount_out=forward_out,
                recipient=executor.address,
                flash_borrow=True,
            ), 0, True),
            (usdc.address, encode_erc20_transfer_calldata(
                recipient=v2_pair_b.address,
                amount=forward_out,
            ), 0, False),
            (v2_pair_b.address, encode_v2_swap_calldata(
                zero_for_one=v2_b_zfo,
                amount_out=v2_b_amount_out,
                recipient=executor.address,
                flash_borrow=False,
            ), 0, False),
            (weth.address, encode_erc20_transfer_calldata(
                recipient=v2_pair_a.address,
                amount=v2_a_amount_in,
            ), 0, False),
        ]

        tx = executor.execute_payloads(
            payloads, [], 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestPancakeCallCallback:
    """
    Verify that the executor's pancakeCall() callback entry point works.
    PancakeSwap V2 pairs use pancakeCall() instead of uniswapV2Call().
    """

    def test_pancake_call_settles(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        V2→V2 path using pancakeCall() callback selector.

        Uses fake_uniswap_v2_pair with callback_variant=2 (pancakeCall).
        """
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        v2_pair_a = project.fake_uniswap_v2_pair.deploy(
            token0, token1,
            2,  # callback_variant: 2=pancakeCall
            sender=owner_account,
        )
        v2_pair_b = project.fake_uniswap_v2_pair.deploy(
            token0, token1,
            0,  # callback_variant: 0=uniswapV2Call
            sender=owner_account,
        )

        v2_a_amount_in = 1 * 10**18
        forward_out = 2000 * 10**6
        v2_b_amount_out = 2 * 10**18

        v2_a_token0 = v2_pair_a.token0()
        v2_a_zfo = v2_a_token0 == weth.address

        usdc.mint(v2_pair_a.address, forward_out, sender=owner_account)
        v2_pair_a.set_next_swap(v2_a_amount_in, forward_out, v2_a_zfo, sender=owner_account)

        v2_b_token0 = v2_pair_b.token0()
        v2_b_zfo = v2_b_token0 == usdc.address

        weth.mint(v2_pair_b.address, v2_b_amount_out, sender=owner_account)
        usdc.mint(v2_pair_b.address, forward_out, sender=owner_account)
        v2_pair_b.set_next_swap(forward_out, v2_b_amount_out, v2_b_zfo, sender=owner_account)

        payloads = [
            (v2_pair_a.address, encode_v2_swap_calldata(
                zero_for_one=v2_a_zfo,
                amount_out=forward_out,
                recipient=executor.address,
                flash_borrow=True,
            ), 0, True),
            (usdc.address, encode_erc20_transfer_calldata(
                recipient=v2_pair_b.address,
                amount=forward_out,
            ), 0, False),
            (v2_pair_b.address, encode_v2_swap_calldata(
                zero_for_one=v2_b_zfo,
                amount_out=v2_b_amount_out,
                recipient=executor.address,
                flash_borrow=False,
            ), 0, False),
            (weth.address, encode_erc20_transfer_calldata(
                recipient=v2_pair_a.address,
                amount=v2_a_amount_in,
            ), 0, False),
        ]

        tx = executor.execute_payloads(
            payloads, [], 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestPancakeV3Callback:
    """
    Verify that the executor's pancakeV3SwapCallback() entry point works.
    PancakeSwap V3 pools use this selector instead of uniswapV3SwapCallback.
    """

    def test_pancake_v3_callback_settles(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        V3→V3 path using pancakeV3SwapCallback() selector.

        Uses fake_uniswap_v3_pool with callback_variant=1 (pancakeV3SwapCallback).
        """
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        v3_pool_a = project.fake_uniswap_v3_pool.deploy(
            token0, token1,
            1,  # callback_variant: 1=pancakeV3SwapCallback
            sender=owner_account,
        )
        v3_pool_b = project.fake_uniswap_v3_pool.deploy(
            token0, token1,
            1,  # callback_variant: 1=pancakeV3SwapCallback
            sender=owner_account,
        )

        v3_a_amount_in = 1 * 10**18
        forward_out = 2000 * 10**6
        v3_b_amount_out = 2 * 10**18

        v3_a_token0 = v3_pool_a.token0()
        v3_a_zfo = v3_a_token0 == weth.address

        usdc.mint(v3_pool_a.address, forward_out, sender=owner_account)
        v3_pool_a.set_next_swap(v3_a_amount_in, forward_out, v3_a_zfo, sender=owner_account)

        v3_b_token0 = v3_pool_b.token0()
        v3_b_zfo = v3_b_token0 == usdc.address

        weth.mint(v3_pool_b.address, v3_b_amount_out, sender=owner_account)
        v3_pool_b.set_next_swap(forward_out, v3_b_amount_out, v3_b_zfo, sender=owner_account)

        v3_a_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_a_zfo else (MAX_SQRT_RATIO - 1)
        v3_b_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_b_zfo else (MAX_SQRT_RATIO - 1)

        payloads = [
            (v3_pool_a.address, encode_v3_swap_calldata(
                recipient=executor.address,
                zero_for_one=v3_a_zfo,
                amount_specified=v3_a_amount_in,
                sqrt_price_limit_x96=v3_a_sqrt_limit,
            ), 0, True),
            (v3_pool_b.address, encode_v3_swap_calldata(
                recipient=executor.address,
                zero_for_one=v3_b_zfo,
                amount_specified=forward_out,
                sqrt_price_limit_x96=v3_b_sqrt_limit,
            ), 0, True),
            (usdc.address, encode_erc20_transfer_calldata(
                recipient=v3_pool_b.address,
                amount=forward_out,
            ), 0, False),
        ]

        tx = executor.execute_payloads(
            payloads, [], 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


# ── Tests: Settlement Branches ───────────────────────────────


class TestV4V3NativeEthOutput:
    """
    V4→V3 path where the V4 swap produces native ETH (not WETH).
    Tests the unwrap branch in _v4_settle_currency (Phase 3).
    """

    def test_v4_v3_native_eth_settlement(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        ETH→USDC at V4, USDC→WETH at V3.

        V4 swap sends native ETH from PoolManager to executor.
        Phase 3: executor has positive native ETH delta → take() receives ETH.
        But V3 swap needs WETH → executor wraps some ETH if needed.
        V3 callback auto-pays WETH to V3 pool.

        The key insight: V4 uses NATIVE_ADDRESS for ETH, but V3 uses WETH.
        Phase 3 settle: native_delta > 0 → take() ETH from PM.
        V3 callback: auto-pays WETH to V3 pool.
        """
        # Deploy V4 PM and V3 pool
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        pm = project.fake_uniswap_v4_pool_manager.deploy(
            token0, token1,
            sender=owner_account,
        )
        v3_pool = project.fake_uniswap_v3_pool.deploy(
            token0, token1,
            0,  # callback_variant: 0=uniswapV3SwapCallback
            sender=owner_account,
        )

        # ── Amounts ──
        v4_amount_in = 1 * 10**18  # 1 ETH in (native)
        forward_out = 2000 * 10**6  # 2000 USDC out from V4
        v3_amount_in = forward_out
        v3_amount_out = 2 * 10**18  # 2 WETH out from V3

        # ── Set up V4 swap: ETH→USDC ──
        # V4 pool key with NATIVE_ADDRESS as one of the currencies
        v4_key = _make_pool_key(NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == NATIVE_ADDRESS  # True if ETH is currency0

        _setup_v4_swap(
            pm, owner_account, v4_key,
            v4_amount_in, forward_out, v4_zfo,
            output_token=usdc,
        )

        # ── Set up V3 swap: USDC→WETH ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == usdc.address

        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        usdc.mint(v3_pool.address, v3_amount_in, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Build payloads ──
        unlock_calldata = encode_v4_unlock_calldata(b"")
        take_calldata = encode_v4_take_calldata(
            currency=usdc.address,
            to=executor.address,
            amount=forward_out,
        )
        transfer_calldata = encode_erc20_transfer_calldata(v3_pool.address, forward_out)

        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)
        v3_swap_calldata = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_zfo,
            amount_specified=v3_amount_in,
            sqrt_price_limit_x96=v3_sqrt_limit,
        )

        payloads = [
            (pm.address, unlock_calldata, 0, True),
            (pm.address, take_calldata, 0, False),
            (usdc.address, transfer_calldata, 0, False),
            (v3_pool.address, v3_swap_calldata, 0, True),
        ]

        sqrt_limit_v4 = MIN_SQRT_PRICE_X96 + 1 if v4_zfo else MAX_SQRT_PRICE_X96 - 1
        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_key, v4_zfo,
                -v4_amount_in,
                sqrt_limit_v4,
                dynamic_amount=False,
            ),
        ]

        tx = executor.execute_payloads(
            payloads, v4_swaps, 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV2DirectSwapNoCallback:
    """
    V3→V2 path where V2 is called with data=b"" (direct swap, no callback).
    This verifies the executor delivers the V2 swap payload without
    will_callback=True — no callback registration needed.
    """

    def test_v2_direct_swap_no_callback(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """V3→V2 with V2 direct swap (data=b"", no flash borrow)."""
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        v3_pool = project.fake_uniswap_v3_pool.deploy(
            token0, token1,
            0, sender=owner_account,
        )
        v2_pair = project.fake_uniswap_v2_pair.deploy(
            token0, token1,
            0, sender=owner_account,
        )

        v3_amount_in = 1 * 10**18
        forward_out = 2000 * 10**6
        v2_amount_out = 2 * 10**18

        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == weth.address

        usdc.mint(v3_pool.address, forward_out, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, forward_out, v3_zfo, sender=owner_account)

        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == usdc.address

        weth.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        usdc.mint(v2_pair.address, forward_out, sender=owner_account)
        v2_pair.set_next_swap(forward_out, v2_amount_out, v2_zfo, sender=owner_account)

        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)

        payloads = [
            (v3_pool.address, encode_v3_swap_calldata(
                recipient=executor.address,
                zero_for_one=v3_zfo,
                amount_specified=v3_amount_in,
                sqrt_price_limit_x96=v3_sqrt_limit,
            ), 0, True),
            (usdc.address, encode_erc20_transfer_calldata(
                recipient=v2_pair.address, amount=forward_out,
            ), 0, False),
            # V2 direct swap — flash_borrow=False means data=b"" → no callback
            (v2_pair.address, encode_v2_swap_calldata(
                zero_for_one=v2_zfo,
                amount_out=v2_amount_out,
                recipient=executor.address,
                flash_borrow=False,
            ), 0, False),  # will_callback=False — no callback registration
        ]

        tx = executor.execute_payloads(
            payloads, [], 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV3NoAutopayForNonWeth:
    """
    Verify that V3 callback auto-pay does NOT fire when the V3 pool
    is owed a non-WETH ERC-20 token.

    The executor's v3_swap_callback() checks token0()/token1() == WETH_ADDR
    before auto-paying. If the pool is owed USDC instead of WETH, no
    auto-transfer should happen — the payment must come from explicit
    payloads or other mechanisms.
    """

    def test_v3_no_autopay_for_non_weth_debt(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        V3→V3 path where V3_B is owed USDC (not WETH)

        If auto-pay incorrectly fires for USDC (transferring WETH instead),
        the V3 pool balance check would fail because it receives the wrong token.

        V3_A: WETH→USDC (V3_A is owed WETH → auto-pay fires)
        V3_B: USDC→WETH (V3_B is owed USDC → auto-pay must NOT fire)
        """
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        v3_pool_a = project.fake_uniswap_v3_pool.deploy(
            token0, token1,
            0, sender=owner_account,
        )
        v3_pool_b = project.fake_uniswap_v3_pool.deploy(
            token0, token1,
            0, sender=owner_account,
        )

        v3_a_amount_in = 1 * 10**18
        forward_out = 2000 * 10**6
        v3_b_amount_out = 2 * 10**18

        v3_a_token0 = v3_pool_a.token0()
        v3_a_zfo = v3_a_token0 == weth.address

        usdc.mint(v3_pool_a.address, forward_out, sender=owner_account)
        v3_pool_a.set_next_swap(v3_a_amount_in, forward_out, v3_a_zfo, sender=owner_account)

        v3_b_token0 = v3_pool_b.token0()
        v3_b_zfo = v3_b_token0 == usdc.address

        weth.mint(v3_pool_b.address, v3_b_amount_out, sender=owner_account)
        v3_pool_b.set_next_swap(forward_out, v3_b_amount_out, v3_b_zfo, sender=owner_account)

        v3_a_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_a_zfo else (MAX_SQRT_RATIO - 1)
        v3_b_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_b_zfo else (MAX_SQRT_RATIO - 1)

        payloads = [
            (v3_pool_a.address, encode_v3_swap_calldata(
                recipient=executor.address,
                zero_for_one=v3_a_zfo,
                amount_specified=v3_a_amount_in,
                sqrt_price_limit_x96=v3_a_sqrt_limit,
            ), 0, True),
            (v3_pool_b.address, encode_v3_swap_calldata(
                recipient=executor.address,
                zero_for_one=v3_b_zfo,
                amount_specified=forward_out,
                sqrt_price_limit_x96=v3_b_sqrt_limit,
            ), 0, True),
            # Explicit USDC transfer to V3_B — auto-pay must NOT handle this
            (usdc.address, encode_erc20_transfer_calldata(
                recipient=v3_pool_b.address, amount=forward_out,
            ), 0, False),
        ]

        tx = executor.execute_payloads(
            payloads, [], 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV2V4WrongSignConvention:
    """
    Regression test: V4 amountSpecified uses the OPPOSITE sign from V3.
    For exact-input mode:
      V3: amountSpecified > 0
      V4: amountSpecified < 0

    Using V3 sign convention (positive) for V4 amountSpecified would make
    V4 interpret it as exact-output mode, likely causing a revert.
    """

    def test_v2_v4_wrong_sign_convention_reverts(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        V2→V4 path with positive amountSpecified (V3 convention) in V4 swap.
        Must revert because V4 interprets positive as exact-output.
        """
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        pm = project.fake_uniswap_v4_pool_manager.deploy(
            token0, token1, sender=owner_account,
        )
        v2_pair = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, sender=owner_account,
        )

        v2_amount_in = 1 * 10**18
        forward_out = 2000 * 10**6
        v4_amount_out = 2 * 10**18

        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == weth.address

        usdc.mint(v2_pair.address, forward_out, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, forward_out, v2_zfo, sender=owner_account)

        v4_key = _make_pool_key(usdc.address, weth.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == usdc.address

        _setup_v4_swap(
            pm, owner_account, v4_key,
            forward_out, v4_amount_out, v4_zfo,
            output_token=weth,
        )

        v2_swap_data = encode_v2_swap_calldata(
            zero_for_one=v2_zfo,
            amount_out=forward_out,
            recipient=executor.address,
            flash_borrow=True,
        )
        transfer_to_pm = encode_erc20_transfer_calldata(pm.address, forward_out)
        sync_calldata = encode_v4_sync_calldata(usdc.address)
        unlock_calldata = encode_v4_unlock_calldata(b"")
        weth_transfer_to_v2 = encode_erc20_transfer_calldata(v2_pair.address, v2_amount_in)

        payloads = [
            (v2_pair.address, v2_swap_data, 0, True),
            (pm.address, sync_calldata, 0, False),
            (usdc.address, transfer_to_pm, 0, False),
            (pm.address, unlock_calldata, 0, True),
            (weth.address, weth_transfer_to_v2, 0, False),
        ]

        # BUG: Using V3 sign convention (positive) instead of V4 (negative)
        sqrt_limit_v4 = MIN_SQRT_PRICE_X96 + 1 if v4_zfo else MAX_SQRT_PRICE_X96 - 1
        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_key, v4_zfo,
                forward_out,  # WRONG! Should be -forward_out for V4 exact-input
                sqrt_limit_v4,
                dynamic_amount=False,
            ),
        ]

        tx = executor.execute_payloads(
            payloads, v4_swaps, 0, True,
            sender=owner_account, raise_on_revert=False,
        )

        # The transaction MUST revert with the wrong sign convention
        assert tx.status == 0, "Transaction should revert with wrong V4 sign convention"
