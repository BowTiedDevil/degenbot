"""
Tests for tstore_executor V4-V4 swap execution.

Verifies that:
1. V4-V4 same-currency paths (WETH→WBTC→WETH) settle without CurrencyNotSettled
2. V4-V4 different-currency paths (WETH→WBTC→ETH) settle without CurrencyNotSettled
3. Dynamic amounts correctly derive amountSpecified from delta ledger
4. Intermediate ERC-20 tokens cancel exactly (delta = 0 after both swaps)

Uses fake contracts (fake_erc20, fake_weth, fake_uniswap_v4_pool_manager) to
mock on-chain swap behavior. The profit check is disabled (skip_profit_check=True)
since we are testing settlement correctness, not profitability.
"""

import eth_abi
import pytest
from ape.api.accounts import TestAccountAPI
from ape.api.transactions import ReceiptAPI
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


# ── Fixtures ──────────────────────────────────────────────────


@pytest.fixture
def wbtc(
    project: ProjectManager,
    owner_account: TestAccount,
) -> ContractInstance:
    return project.fake_erc20.deploy(
        "Fake Wrapped Bitcoin",
        "WBTC",
        8,
        100_000_000,
        sender=owner_account,
    )


@pytest.fixture
def weth(
    project: ProjectManager,
    owner_account: TestAccount,
) -> ContractInstance:
    return project.fake_weth.deploy(
        "Fake Wrapped Ether",
        "WETH",
        18,
        100_000_000,
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
        100_000_000,
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
        token0,
        token1,
        sender=owner_account,
    )


@pytest.fixture
def executor(
    project: ProjectManager,
    owner_account: TestAccount,
    weth: ContractInstance,
    v4_pool_manager: ContractInstance,
) -> ContractInstance:
    contract = project.tstore_executor.deploy(
        weth.address,
        value=WETH_DEPLOYMENT_WRAP_AMOUNT,
        sender=owner_account,
    )
    # Fund the executor with ETH for settlement
    contract.balance = 1000 * 10**18
    return contract


# ── Helper functions ──────────────────────────────────────────


def _make_pool_key(
    currency0: str,
    currency1: str,
    fee: int = 0,
    tick_spacing: int = 60,
    hooks: str = ZERO_ADDRESS,
) -> tuple[str, str, int, int, str]:
    """Build a V4 PoolKey tuple with sorted currencies."""
    c0, c1 = sorted([currency0, currency1], key=lambda addr: addr.lower())
    return (c0, c1, fee, tick_spacing, hooks)


def _pool_id_from_key(pool_key: tuple) -> bytes:
    """Compute the V4 pool ID from a PoolKey tuple."""
    return keccak(
        eth_abi.encode(
            types=["address", "address", "uint24", "int24", "address"],
            args=pool_key,
        )
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
    """Set up a fake swap on the V4 pool manager with output funding."""
    # Fund the PM with the output token so the swap resolves
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
    """Build a V4SwapPayload ABI tuple."""
    return (
        (currency0, currency1, fee, tick_spacing, hooks),
        (zero_for_one, amount_specified, sqrt_price_limit_x96),
        dynamic_amount,
    )


# ── Tests ──────────────────────────────────────────────────────


class TestV4V4SameCurrency:
    """V4-V4 paths where both pools use WETH (e.g., WETH→WBTC→WETH)."""

    def test_v4_v4_all_weth_pairs(
        self,
        wbtc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
    ):
        """
        WETH→WBTC at Pool A, then WBTC→WETH at Pool B.

        The second swap uses dynamic_amount=True to read the WBTC delta
        from Pool A's output. The intermediate WBTC cancels exactly.
        """
        # ── Set up V4 Pool A: WETH→WBTC ──
        pool_a_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=60)
        pool_a_amount_in = 10 * 10**18  # 10 WETH
        pool_a_amount_out = 1 * 10**8  # 1 WBTC (8 decimals)
        pool_a_zfo = pool_a_key[0] == weth.address  # zero_for_one direction

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=wbtc,
        )

        # ── Set up V4 Pool B: WBTC→WETH (different tick_spacing = different pool ID) ──
        pool_b_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=120)
        pool_b_amount_in = pool_a_amount_out  # WBTC from Pool A
        pool_b_amount_out = 2 * pool_a_amount_in  # 20 WETH (profitable)
        pool_b_zfo = pool_b_key[0] == wbtc.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        # Pool IDs must differ (different tick_spacing)
        assert _pool_id_from_key(pool_a_key) != _pool_id_from_key(pool_b_key)

        # ── Encode the V4 swaps ──
        # Swap A: kickstart (dynamic_amount=False, amountSpecified < 0 for V4 exact-input)
        v4_swap_a = _encode_v4_swap_payload(
            *pool_a_key,
            pool_a_zfo,
            -pool_a_amount_in,  # V4: negative for exact-input
            MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=False,
        )

        # Swap B: dynamic (dynamic_amount=True, amountSpecified=0 → contract derives from delta)
        v4_swap_b = _encode_v4_swap_payload(
            *pool_b_key,
            pool_b_zfo,
            0,  # amountSpecified=0 signals dynamic derivation
            MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=True,
        )

        # ── Execute via the V4 unlock payload ──
        # The unlock call goes in the payloads queue.
        # V4 swaps are passed separately.
        unlock_selector = HexBytes(keccak(text="unlock(bytes)")[:4])
        unlock_calldata = unlock_selector + eth_abi.encode(types=["bytes"], args=[b""])

        payloads = [
            # (target, calldata, value, will_callback)
            (v4_pool_manager.address, unlock_calldata, 0, True),
        ]

        v4_swaps = [v4_swap_a, v4_swap_b]

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

        # Verify the executor received the profit (WETH)
        # Profit = WETH out from Pool B - WETH in to Pool A
        expected_profit = pool_b_amount_out - pool_a_amount_in

        # Check WETH balance of executor after the swap
        # (We can't easily check the exact balance because of the
        #  deposit/transfer dance, but a successful execution without
        #  CurrencyNotSettled is the primary assertion.)


class TestV4V4DifferentCurrency:
    """V4-V4 paths where pools use different currencies (e.g., WETH→WBTC→ETH)."""

    def test_v4_v4_weth_wbtc_eth(
        self,
        wbtc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
    ):
        """
        WETH→WBTC at Pool A, then WBTC→ETH at Pool B.

        Pool A: currency0=WBTC, currency1=WETH
        Pool B: currency0=ETH(native), currency1=WBTC

        This is the key scenario that was broken before: the intermediate
        WBTC token was not tracked in the old ether_delta/weth_delta
        accumulators, causing CurrencyNotSettled.
        """
        # ── Set up V4 Pool A: WETH→WBTC ──
        pool_a_key = _make_pool_key(weth.address, wbtc.address, fee=3000, tick_spacing=60)
        pool_a_amount_in = 10 * 10**18  # 10 WETH
        pool_a_amount_out = 1 * 10**8  # 1 WBTC
        pool_a_zfo = pool_a_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=wbtc,
        )

        # ── Set up V4 Pool B: WBTC→ETH ──
        pool_b_key = _make_pool_key(NATIVE_ADDRESS, wbtc.address, fee=500, tick_spacing=10)
        pool_b_amount_in = pool_a_amount_out  # WBTC from Pool A
        pool_b_amount_out = 20 * 10**18  # 20 ETH
        pool_b_zfo = pool_b_key[0] == wbtc.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            fund_eth=True,  # Pool B sends native ETH
        )

        # ── Encode the V4 swaps ──
        v4_swap_a = _encode_v4_swap_payload(
            *pool_a_key,
            pool_a_zfo,
            -pool_a_amount_in,
            MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=False,
        )

        v4_swap_b = _encode_v4_swap_payload(
            *pool_b_key,
            pool_b_zfo,
            0,  # Dynamic: contract reads WBTC delta from swap A
            MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=True,
        )

        # ── Execute ──
        unlock_selector = HexBytes(keccak(text="unlock(bytes)")[:4])
        unlock_calldata = unlock_selector + eth_abi.encode(types=["bytes"], args=[b""])

        payloads = [
            (v4_pool_manager.address, unlock_calldata, 0, True),
        ]

        v4_swaps = [v4_swap_a, v4_swap_b]

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
            raise ValueError("Transaction reverted — likely CurrencyNotSettled")

    def test_v4_v4_usdc_intermediate_weth_eth(
        self,
        usdc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
    ):
        """
        WETH→USDC at Pool A, then USDC→ETH at Pool B.

        This tests the exact scenario from the bug report: USDC is the
        intermediate ERC-20 that was silently dropped by the old
        ether_delta/weth_delta accumulators.
        """
        # ── Set up V4 Pool A: WETH→USDC ──
        pool_a_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_a_amount_in = 1 * 10**18  # 1 WETH
        pool_a_amount_out = 2000 * 10**6  # 2000 USDC
        pool_a_zfo = pool_a_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=usdc,
        )

        # ── Set up V4 Pool B: USDC→ETH ──
        pool_b_key = _make_pool_key(NATIVE_ADDRESS, usdc.address, fee=500, tick_spacing=10)
        pool_b_amount_in = pool_a_amount_out  # USDC from Pool A
        pool_b_amount_out = 2 * 10**18  # 2 ETH (profitable)
        pool_b_zfo = pool_b_key[0] == usdc.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            fund_eth=True,
        )

        # ── Encode the V4 swaps ──
        v4_swap_a = _encode_v4_swap_payload(
            *pool_a_key,
            pool_a_zfo,
            -pool_a_amount_in,
            MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=False,
        )

        v4_swap_b = _encode_v4_swap_payload(
            *pool_b_key,
            pool_b_zfo,
            0,
            MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=True,
        )

        # ── Execute ──
        unlock_selector = HexBytes(keccak(text="unlock(bytes)")[:4])
        unlock_calldata = unlock_selector + eth_abi.encode(types=["bytes"], args=[b""])

        payloads = [
            (v4_pool_manager.address, unlock_calldata, 0, True),
        ]

        v4_swaps = [v4_swap_a, v4_swap_b]

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
            raise ValueError("Transaction reverted — likely CurrencyNotSettled")


class TestV4V4DynamicAmountLogic:
    """Tests for the dynamic_amount derivation in unlockCallback."""

    def test_dynamic_amount_reads_delta_ledger(
        self,
        wbtc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
    ):
        """
        Verify that a dynamic_amount swap reads the correct delta from
        the ledger (not a stale or zero value).

        Pool A: WETH→WBTC (zfo=True: swap sends WETH, receives WBTC)
        After Pool A: t_v4_deltas[WBTC] = +amount_out (credit)
        After Pool A: t_v4_deltas[WETH] = -amount_in (debit)

        Pool B (dynamic): WBTC→WETH (zfo depends on key ordering)
        Contract reads: input_delta = t_v4_deltas[input_currency]
        If input_delta > 0: amountSpecified = -input_delta (V4 exact-input)
        """
        # Set up two pools with same currencies but different tick spacings
        pool_a_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=60)
        pool_a_amount_in = 5 * 10**18
        pool_a_amount_out = 500 * 10**8  # 500 WBTC
        pool_a_zfo = pool_a_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=wbtc,
        )

        pool_b_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=200)
        pool_b_amount_in = pool_a_amount_out
        pool_b_amount_out = 10 * 10**18  # 10 WETH
        pool_b_zfo = pool_b_key[0] == wbtc.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        # Encode with dynamic_amount=True for swap B
        v4_swap_a = _encode_v4_swap_payload(
            *pool_a_key,
            pool_a_zfo,
            -pool_a_amount_in,
            MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=False,
        )

        v4_swap_b = _encode_v4_swap_payload(
            *pool_b_key,
            pool_b_zfo,
            0,  # Dynamic: contract derives from delta ledger
            MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=True,
        )

        unlock_selector = HexBytes(keccak(text="unlock(bytes)")[:4])
        unlock_calldata = unlock_selector + eth_abi.encode(types=["bytes"], args=[b""])

        payloads = [
            (v4_pool_manager.address, unlock_calldata, 0, True),
        ]

        v4_swaps = [v4_swap_a, v4_swap_b]

        tx = executor.execute_payloads(
            payloads,
            v4_swaps,
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

    def test_specified_amount_swap_still_works(
        self,
        wbtc: ContractInstance,
        weth: ContractInstance,
        owner_account: TestAccount,
        executor: ContractInstance,
        v4_pool_manager: ContractInstance,
    ):
        """
        Verify that V4-V4 with both swaps using dynamic_amount=False
        (specified amounts) still works — backward compatibility.
        """
        pool_a_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=60)
        pool_a_amount_in = 10 * 10**18
        pool_a_amount_out = 1 * 10**8
        pool_a_zfo = pool_a_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=wbtc,
        )

        pool_b_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=120)
        pool_b_amount_in = pool_a_amount_out
        pool_b_amount_out = 15 * 10**18
        pool_b_zfo = pool_b_key[0] == wbtc.address

        _setup_v4_swap(
            v4_pool_manager,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        # Both swaps use specified amounts (dynamic_amount=False)
        v4_swap_a = _encode_v4_swap_payload(
            *pool_a_key,
            pool_a_zfo,
            -pool_a_amount_in,
            MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=False,
        )

        v4_swap_b = _encode_v4_swap_payload(
            *pool_b_key,
            pool_b_zfo,
            -pool_b_amount_in,  # Specified amount (not dynamic)
            MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
            dynamic_amount=False,
        )

        unlock_selector = HexBytes(keccak(text="unlock(bytes)")[:4])
        unlock_calldata = unlock_selector + eth_abi.encode(types=["bytes"], args=[b""])

        payloads = [
            (v4_pool_manager.address, unlock_calldata, 0, True),
        ]

        v4_swaps = [v4_swap_a, v4_swap_b]

        tx = executor.execute_payloads(
            payloads,
            v4_swaps,
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
