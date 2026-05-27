"""
Tests for tstore_executor three-hop swap execution.

Verifies that:
1. V4→V4→V4 paths settle correctly (3 V4 swaps with dynamic_amount chaining)
2. Two intermediate tokens cancel exactly via the delta ledger
3. V4→V3→V2 hybrid three-hop paths settle correctly
4. V2→V3→V4 hybrid three-hop paths settle correctly (pre-settle + post-unlock payment)
5. V4→V4→V3 paths settle correctly (2 V4 swaps + V3 payload in Phase 2)

The contract already supports 3+ hops (MAX_V4_SWAPS=4, MAX_PAYLOADS=16).
These tests validate that the 4-phase unlockCallback correctly handles
multi-hop settlement with multiple intermediate currencies.

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
from hexbytes import HexBytes

NATIVE_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")

WETH_DEPLOYMENT_WRAP_AMOUNT = 10 * 10**18

MIN_SQRT_PRICE_X96 = 4295128739
MAX_SQRT_PRICE_X96 = 1461446703485210103287273052203988822378723970342
MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342

# ABI selectors
V2_SWAP_SELECTOR = keccak(text="swap(uint256,uint256,address,bytes)")[:4]
V3_SWAP_SELECTOR = keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
ERC20_TRANSFER_SELECTOR = keccak(text="transfer(address,uint256)")[:4]
V4_UNLOCK_SELECTOR = keccak(text="unlock(bytes)")[:4]
V4_TAKE_SELECTOR = keccak(text="take(address,address,uint256)")[:4]
V4_SYNC_SELECTOR = keccak(text="sync(address)")[:4]


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


def _pool_id_from_key(pool_key: tuple) -> bytes:
    return keccak(
        eth_abi.encode(
            types=["address", "address", "uint24", "int24", "address"],
            args=pool_key,
        )
    )


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
        pool_key, amount_in, amount_out, zero_for_one, b"",
        sender=owner,
    )


# ── Fixtures ──────────────────────────────────────────────────


@pytest.fixture
def wbtc(
    project: ProjectManager,
    owner_account: TestAccount,
) -> ContractInstance:
    return project.fake_erc20.deploy(
        "Fake Wrapped Bitcoin", "WBTC", 8, 100_000_000,
        sender=owner_account,
    )


@pytest.fixture
def weth(
    project: ProjectManager,
    owner_account: TestAccount,
) -> ContractInstance:
    return project.fake_weth.deploy(
        "Fake Wrapped Ether", "WETH", 18, 100_000_000,
        sender=owner_account,
    )


@pytest.fixture
def usdc(
    project: ProjectManager,
    owner_account: TestAccount,
) -> ContractInstance:
    return project.fake_erc20.deploy(
        "Fake USD Coin", "USDC", 6, 100_000_000,
        sender=owner_account,
    )


@pytest.fixture
def owner_account(accounts: list[TestAccountAPI]) -> TestAccount:
    return accounts[0]


@pytest.fixture
def v4_pool_manager(
    project: ProjectManager,
    owner_account: TestAccount,
    wbtc: ContractInstance,
    weth: ContractInstance,
) -> ContractInstance:
    token0, token1 = sorted(
        [wbtc.address, weth.address],
        key=lambda addr: addr.lower(),
    )
    return project.fake_uniswap_v4_pool_manager.deploy(
        token0, token1, sender=owner_account,
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


# ── Tests: V4-only three-hop ──────────────────────────────────


class TestThreeHopV4Only:
    """
    V4→V4→V4 paths: three V4 swaps in v4_swaps, one unlock payload.
    Swap 1 is kickstart, swaps 2 & 3 use dynamic_amount=True.
    Two intermediate tokens (USDC, WBTC) must cancel exactly.
    """

    def test_v4_v4_v4_three_pool(
        self,
        wbtc: ContractInstance,
        weth: ContractInstance,
        usdc: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
    ):
        """
        WETH→USDC@V4_A → USDC→WBTC@V4_B (dynamic) → WBTC→WETH@V4_C (dynamic).

        Three V4 swaps with two intermediate currencies. After Phase 1:
          t_v4_deltas[WETH] = -amount_in + amount_out_C (net WETH debit/credit)
          t_v4_deltas[USDC] = +amount_out_A - amount_in_B (should cancel exactly)
          t_v4_deltas[WBTC] = +amount_out_B - amount_in_C (should cancel exactly)
        Phase 3 settles the net WETH delta only.
        """
        # ── Set up V4 Pool A: WETH→USDC ──
        pool_a_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_a_amount_in = 1 * 10**18  # 1 WETH
        pool_a_amount_out = 2000 * 10**6  # 2000 USDC
        pool_a_zfo = pool_a_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_a_key,
            pool_a_amount_in, pool_a_amount_out, pool_a_zfo,
            output_token=usdc,
        )

        # ── Set up V4 Pool B: USDC→WBTC ──
        pool_b_key = _make_pool_key(usdc.address, wbtc.address, fee=500, tick_spacing=10)
        pool_b_amount_in = pool_a_amount_out  # USDC from Pool A
        pool_b_amount_out = 100 * 10**8  # 100 WBTC
        pool_b_zfo = pool_b_key[0] == usdc.address

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_b_key,
            pool_b_amount_in, pool_b_amount_out, pool_b_zfo,
            output_token=wbtc,
        )

        # ── Set up V4 Pool C: WBTC→WETH ──
        pool_c_key = _make_pool_key(wbtc.address, weth.address, fee=10000, tick_spacing=200)
        pool_c_amount_in = pool_b_amount_out  # WBTC from Pool B
        pool_c_amount_out = 2 * 10**18  # 2 WETH (profitable)
        pool_c_zfo = pool_c_key[0] == wbtc.address

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_c_key,
            pool_c_amount_in, pool_c_amount_out, pool_c_zfo,
            output_token=weth,
        )

        # Pool IDs must all differ
        assert _pool_id_from_key(pool_a_key) != _pool_id_from_key(pool_b_key)
        assert _pool_id_from_key(pool_b_key) != _pool_id_from_key(pool_c_key)
        assert _pool_id_from_key(pool_a_key) != _pool_id_from_key(pool_c_key)

        # ── Encode V4 swaps ──
        # Swap A: kickstart (exact-input, amountSpecified < 0 for V4)
        v4_swap_a = _encode_v4_swap_payload(
            *pool_a_key, pool_a_zfo,
            -pool_a_amount_in,
            MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=False,
        )

        # Swap B: dynamic (reads USDC delta from swap A)
        v4_swap_b = _encode_v4_swap_payload(
            *pool_b_key, pool_b_zfo,
            0,
            MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=True,
        )

        # Swap C: dynamic (reads WBTC delta from swap B)
        v4_swap_c = _encode_v4_swap_payload(
            *pool_c_key, pool_c_zfo,
            0,
            MIN_SQRT_PRICE_X96 + 1 if pool_c_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=True,
        )

        # ── Execute ──
        unlock_selector = HexBytes(keccak(text="unlock(bytes)")[:4])
        unlock_calldata = unlock_selector + eth_abi.encode(types=["bytes"], args=[b""])

        payloads = [
            (v4_pool_manager.address, unlock_calldata, 0, True),
        ]

        v4_swaps = [v4_swap_a, v4_swap_b, v4_swap_c]

        tx = executor.execute_payloads(
            payloads, v4_swaps, 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


# ── Tests: Hybrid three-hop ──────────────────────────────────


class TestThreeHopHybrid:
    """
    Three-hop hybrid paths mixing V4, V3, and V2 swaps.
    These exercise the most complex Phase 0/1/2/3 interactions.
    """

    def test_v4_v3_v2_three_hop(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        WETH→USDC@V4 → USDC→WETH@V3 → WETH→USDC@V2 (direct).

        Phase 1: V4 swap produces USDC.
        Phase 2: take USDC from PM → V3 swap → auto-pay WETH → V2 direct swap.
        Phase 3: settle V4 deltas.

        V2 uses direct swap (flash_borrow=False): transfer WETH to V2 first,
        then V2.swap() sends USDC to executor. This avoids the V2 callback
        entirely — V3's auto-pay delivers WETH before the V2 swap payload.
        """
        # Deploy V4 PM, V3 pool, V2 pair
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        pm = project.fake_uniswap_v4_pool_manager.deploy(
            token0, token1, sender=owner_account,
        )
        v3_pool = project.fake_uniswap_v3_pool.deploy(
            token0, token1, 0, sender=owner_account,
        )
        v2_pair = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, sender=owner_account,
        )

        # ── Amounts ──
        # V4: 1 WETH → 2000 USDC
        # V3: 2000 USDC → 2 WETH (V3 callback auto-pays WETH)
        # V2: 1.5 WETH → 3000 USDC (explicit WETH transfer, flash borrow)
        v4_amount_in = 1 * 10**18
        forward_out = 2000 * 10**6
        v3_amount_out = 2 * 10**18  # WETH from V3
        v2_weth_in = 1 * 10**18  # WETH into V2
        v2_usdc_out = 2500 * 10**6  # USDC from V2 (profit)

        # ── Set up V4 swap: WETH→USDC ──
        v4_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == weth.address

        _setup_v4_swap(
            pm, owner_account, v4_key,
            v4_amount_in, forward_out, v4_zfo,
            output_token=usdc,
        )

        # ── Set up V3 swap: USDC→WETH ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == usdc.address

        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        usdc.mint(v3_pool.address, forward_out, sender=owner_account)
        v3_pool.set_next_swap(forward_out, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Set up V2 swap: WETH→USDC ──
        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == weth.address  # selling WETH → zfo

        usdc.mint(v2_pair.address, v2_usdc_out, sender=owner_account)
        weth.mint(v2_pair.address, v2_weth_in, sender=owner_account)
        v2_pair.set_next_swap(v2_weth_in, v2_usdc_out, v2_zfo, sender=owner_account)

        # ── Build payloads ──
        unlock_calldata = encode_v4_unlock_calldata(b"")
        take_calldata = encode_v4_take_calldata(
            currency=usdc.address, to=executor.address, amount=forward_out,
        )

        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)
        v3_swap_calldata = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_zfo,
            amount_specified=forward_out,
            sqrt_price_limit_x96=v3_sqrt_limit,
        )

        # V3 auto-pay delivers WETH before these payloads execute:
        weth_transfer_to_v2 = encode_erc20_transfer_calldata(
            recipient=v2_pair.address, amount=v2_weth_in,
        )
        v2_swap_direct = encode_v2_swap_calldata(
            zero_for_one=v2_zfo,
            amount_out=v2_usdc_out,
            recipient=executor.address,
            flash_borrow=False,
        )

        payloads = [
            (pm.address, unlock_calldata, 0, True),             # Unlock PM
            (pm.address, take_calldata, 0, False),             # Take USDC from PM
            (usdc.address, encode_erc20_transfer_calldata(     # Transfer USDC to V3
                recipient=v3_pool.address, amount=forward_out,
            ), 0, False),
            (v3_pool.address, v3_swap_calldata, 0, True),      # V3 swap (callback, auto-pays WETH)
            (weth.address, weth_transfer_to_v2, 0, False),    # Transfer WETH to V2
            (v2_pair.address, v2_swap_direct, 0, False),      # V2 direct swap → USDC profit
        ]

        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_key, v4_zfo,
                -v4_amount_in,
                MIN_SQRT_PRICE_X96 + 1 if v4_zfo else MAX_SQRT_PRICE_X96 - 1,
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

    def test_v4_v2_v3_three_hop(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        WETH→USDC@V4 → USDC→WETH@V2 (flash) → WETH→USDC@V3.

        Phase 1: V4 swap produces USDC
        Phase 2: take USDC from PM → V2 flash swap (sends WETH, callbacks)
          Inside V2 callback: V3 swap (WETH→USDC, auto-pay WETH)
          After V3 swap: transfer USDC to V2 to repay flash borrow
        Phase 3: settle V4 deltas (WETH debit)

        This tests the V2 callback + nested V3 callback combination.
        """
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        pm = project.fake_uniswap_v4_pool_manager.deploy(
            token0, token1, sender=owner_account,
        )
        v3_pool = project.fake_uniswap_v3_pool.deploy(
            token0, token1, 0, sender=owner_account,
        )
        v2_pair = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, sender=owner_account,
        )

        # ── Amounts ──
        # V4: 1 WETH → 2000 USDC
        # V2: 2000 USDC → 1.5 WETH (flash borrow — V2 sends WETH, expects USDC back)
        # V3: 1 WETH → 2000 USDC (auto-pay WETH to V3)
        v4_amount_in = 1 * 10**18
        v4_amount_out = 2000 * 10**6  # USDC from V4
        v2_amount_in = v4_amount_out  # USDC into V2 (repayment)
        v2_amount_out = 1 * 10**18  # WETH from V2
        v3_amount_in = 1 * 10**18  # WETH into V3
        v3_amount_out = 2000 * 10**6  # USDC from V3

        # ── Set up V4 swap: WETH→USDC ──
        v4_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == weth.address

        _setup_v4_swap(
            pm, owner_account, v4_key,
            v4_amount_in, v4_amount_out, v4_zfo,
            output_token=usdc,
        )

        # ── Set up V2 swap: USDC→WETH (flash borrow) ──
        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == usdc.address  # selling USDC → zfo

        weth.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        usdc.mint(v2_pair.address, v2_amount_in, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Set up V3 swap: WETH→USDC ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == weth.address  # selling WETH → zfo

        usdc.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        weth.mint(v3_pool.address, v3_amount_in, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Build payloads ──
        # Phase 1: V4 swap produces USDC
        # Phase 2: take USDC → V2 flash swap → (in V2 callback) V3 swap → USDC to V2
        unlock_calldata = encode_v4_unlock_calldata(b"")
        take_usdc = encode_v4_take_calldata(
            currency=usdc.address, to=executor.address, amount=v4_amount_out,
        )

        v2_swap = encode_v2_swap_calldata(
            zero_for_one=v2_zfo,
            amount_out=v2_amount_out,
            recipient=executor.address,
            flash_borrow=True,
        )

        # Inside V2 callback:
        # V3 swap: WETH→USDC. V3 callback auto-pays WETH.
        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)
        v3_swap = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_zfo,
            amount_specified=v3_amount_in,
            sqrt_price_limit_x96=v3_sqrt_limit,
        )

        # After V3: transfer USDC to V2 to repay flash borrow
        usdc_to_v2 = encode_erc20_transfer_calldata(v2_pair.address, v2_amount_in)

        payloads = [
            (pm.address, unlock_calldata, 0, True),
            (pm.address, take_usdc, 0, False),
            (v2_pair.address, v2_swap, 0, True),
            (v3_pool.address, v3_swap, 0, True),
            (usdc.address, usdc_to_v2, 0, False),
        ]

        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_key, v4_zfo,
                -v4_amount_in,
                MIN_SQRT_PRICE_X96 + 1 if v4_zfo else MAX_SQRT_PRICE_X96 - 1,
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

    def test_v2_v3_v4_three_hop(
        self,
        wbtc: ContractInstance,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        WETH→USDC@V2 (flash) → USDC→WBTC@V3 → WBTC→WETH@V4.

        V2 flash swap sends USDC to executor, callbacks.
        Inside V2 callback: V3 swap (USDC→WBTC) → transfer WBTC to PM →
          sync WBTC → unlock PM → V4 swap produces WETH →
          WETH to V2 to repay flash borrow.

        This tests the full V2→V3→V4 pipeline with 3 different token pairs.
        """
        # Deploy V2 pair (WETH/USDC), V3 pool (USDC/WBTC), V4 PM (WBTC/WETH)
        token0_v2, token1_v2 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        v2_pair = project.fake_uniswap_v2_pair.deploy(
            token0_v2, token1_v2, 0, sender=owner_account,
        )

        token0_v3, token1_v3 = sorted(
            [usdc.address, wbtc.address],
            key=lambda addr: addr.lower(),
        )
        v3_pool = project.fake_uniswap_v3_pool.deploy(
            token0_v3, token1_v3, 0, sender=owner_account,
        )

        token0_v4, token1_v4 = sorted(
            [wbtc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        pm = project.fake_uniswap_v4_pool_manager.deploy(
            token0_v4, token1_v4, sender=owner_account,
        )

        # ── Amounts ──
        # V2: 1 WETH in → 2000 USDC out (flash borrow)
        # V3: 2000 USDC in → 100 WBTC out
        # V4: 100 WBTC in → 2 WETH out
        v2_amount_in = 1 * 10**18  # WETH into V2
        v2_amount_out = 2000 * 10**6  # USDC from V2

        v3_amount_in = 2000 * 10**6  # USDC into V3
        v3_amount_out = 100 * 10**8  # WBTC from V3

        v4_amount_in = 100 * 10**8  # WBTC into V4
        v4_amount_out = 2 * 10**18  # WETH from V4

        # ── Set up V2 swap: WETH→USDC (flash borrow) ──
        v2_token0 = v2_pair.token0()
        v2_zfo = v2_token0 == weth.address  # selling WETH → zfo

        usdc.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Set up V3 swap: USDC→WBTC ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == usdc.address  # selling USDC → zfo

        wbtc.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        usdc.mint(v3_pool.address, v3_amount_in, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Set up V4 swap: WBTC→WETH ──
        v4_key = _make_pool_key(wbtc.address, weth.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == wbtc.address

        _setup_v4_swap(
            pm, owner_account, v4_key,
            v4_amount_in, v4_amount_out, v4_zfo,
            output_token=weth,
        )

        # ── Build payloads ──
        # 1. V2 flash swap (sends USDC, callbacks)
        v2_swap = encode_v2_swap_calldata(
            zero_for_one=v2_zfo,
            amount_out=v2_amount_out,
            recipient=executor.address,
            flash_borrow=True,
        )

        # Inside V2 callback: deliver V3 swap → WBTC to PM → unlock
        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)
        v3_swap = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_zfo,
            amount_specified=v3_amount_in,
            sqrt_price_limit_x96=v3_sqrt_limit,
        )

        # V3 is owed USDC — need explicit transfer. V3 is owed USDC, not WETH.
        # The executor has USDC from V2. Transfer USDC to V3.
        usdc_to_v3 = encode_erc20_transfer_calldata(v3_pool.address, v3_amount_in)

        # Transfer WBTC from executor to PM
        wbtc_to_pm = encode_erc20_transfer_calldata(pm.address, v4_amount_in)

        # Sync WBTC at PM (before transfer — records zero balance)
        sync_calldata = encode_v4_sync_calldata(wbtc.address)

        # Unlock PM → V4 swap runs, produces WETH
        unlock_calldata = encode_v4_unlock_calldata(b"")

        # Post-unlock: WETH to V2 to repay flash borrow
        weth_to_v2 = encode_erc20_transfer_calldata(v2_pair.address, v2_amount_in)

        payloads = [
            (v2_pair.address, v2_swap, 0, True),       # V2 flash → callback
            (v3_pool.address, v3_swap, 0, True),        # V3 swap → callback
            (usdc.address, usdc_to_v3, 0, False),      # Pay USDC to V3
            (pm.address, sync_calldata, 0, False),     # Sync WBTC at PM (before transfer)
            (wbtc.address, wbtc_to_pm, 0, False),      # Transfer WBTC to PM
            (pm.address, unlock_calldata, 0, True),    # Unlock PM → V4 swap
            (weth.address, weth_to_v2, 0, False),     # Repay WETH to V2
        ]

        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_key, v4_zfo,
                -v4_amount_in,
                MIN_SQRT_PRICE_X96 + 1 if v4_zfo else MAX_SQRT_PRICE_X96 - 1,
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

    def test_v4_v4_v3_three_hop(
        self,
        wbtc: ContractInstance,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        project: ProjectManager,
    ):
        """
        WETH→USDC@V4_A → USDC→WBTC@V4_B (dynamic) → WBTC→WETH@V3.

        Two V4 swaps in v4_swaps + V3 swap payload in Phase 2.
        Phase 2 zeroes both USDC and WBTC intermediate deltas.
        Phase 3 settles the net WETH delta.
        """
        token0, token1 = sorted(
            [usdc.address, weth.address],
            key=lambda addr: addr.lower(),
        )
        pm = project.fake_uniswap_v4_pool_manager.deploy(
            token0, token1, sender=owner_account,
        )
        v3_pool = project.fake_uniswap_v3_pool.deploy(
            wbtc.address, weth.address, 0,  # Different token pair for V3
            sender=owner_account,
        )

        # ── Amounts ──
        # V4_A: 1 WETH → 2000 USDC
        # V4_B: 2000 USDC → 100 WBTC (dynamic)
        # V3: 100 WBTC → 2 WETH
        v4_a_amount_in = 1 * 10**18
        v4_a_amount_out = 2000 * 10**6
        v4_b_amount_in = v4_a_amount_out
        v4_b_amount_out = 100 * 10**8
        v3_amount_in = v4_b_amount_out
        v3_amount_out = 2 * 10**18

        # ── Set up V4_A swap: WETH→USDC ──
        v4_a_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_a_zfo = v4_a_key[0] == weth.address

        _setup_v4_swap(
            pm, owner_account, v4_a_key,
            v4_a_amount_in, v4_a_amount_out, v4_a_zfo,
            output_token=usdc,
        )

        # ── Set up V4_B swap: USDC→WBTC ──
        v4_b_key = _make_pool_key(usdc.address, wbtc.address, fee=500, tick_spacing=10)
        v4_b_zfo = v4_b_key[0] == usdc.address

        _setup_v4_swap(
            pm, owner_account, v4_b_key,
            v4_b_amount_in, v4_b_amount_out, v4_b_zfo,
            output_token=wbtc,
        )

        # ── Set up V3 swap: WBTC→WETH ──
        v3_token0 = v3_pool.token0()
        v3_zfo = v3_token0 == wbtc.address  # selling WBTC → zfo

        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        wbtc.mint(v3_pool.address, v3_amount_in, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Build payloads ──
        # Phase 1: V4_A swap + V4_B swap (dynamic_amount)
        # Phase 2: take WBTC from PM → transfer to V3 → V3 swap → auto-pay WETH
        # Phase 3: settle WETH (net debits/credits from V4_A, V4_B, and take)
        unlock_calldata = encode_v4_unlock_calldata(b"")
        take_wbtc = encode_v4_take_calldata(
            currency=wbtc.address, to=executor.address, amount=v4_b_amount_out,
        )
        transfer_wbtc_to_v3 = encode_erc20_transfer_calldata(v3_pool.address, v3_amount_in)

        v3_sqrt_limit = (MIN_SQRT_RATIO + 1) if v3_zfo else (MAX_SQRT_RATIO - 1)
        v3_swap_calldata = encode_v3_swap_calldata(
            recipient=executor.address,
            zero_for_one=v3_zfo,
            amount_specified=v3_amount_in,
            sqrt_price_limit_x96=v3_sqrt_limit,
        )

        payloads = [
            (pm.address, unlock_calldata, 0, True),
            (pm.address, take_wbtc, 0, False),
            (wbtc.address, transfer_wbtc_to_v3, 0, False),
            (v3_pool.address, v3_swap_calldata, 0, True),
        ]

        v4_swaps = [
            _encode_v4_swap_payload(
                *v4_a_key, v4_a_zfo,
                -v4_a_amount_in,
                MIN_SQRT_PRICE_X96 + 1 if v4_a_zfo else MAX_SQRT_PRICE_X96 - 1,
                dynamic_amount=False,
            ),
            _encode_v4_swap_payload(
                *v4_b_key, v4_b_zfo,
                0,
                MIN_SQRT_PRICE_X96 + 1 if v4_b_zfo else MAX_SQRT_PRICE_X96 - 1,
                dynamic_amount=True,
            ),
        ]

        tx = executor.execute_payloads(
            payloads, v4_swaps, 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
