"""
Tests for cmd_executor V4-V4 swap execution.

Verifies that:
1. V4-V4 same-currency paths (WETH→WBTC→WETH) settle without CurrencyNotSettled
2. V4-V4 different-currency paths (WETH→WBTC→ETH) settle without CurrencyNotSettled
3. All settlement amounts pre-computed off-chain — no delta ledger needed

Uses fake contracts to mock on-chain swap behavior.
The profit check is disabled (skip_profit_check=True) since we are testing
settlement correctness, not profitability.
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

# ── Command opcodes ──

CMD_V2_SWAP = b"\x00"
CMD_V3_SWAP = b"\x01"
CMD_V4_SWAP = b"\x02"
CMD_V4_TAKE = b"\x03"
CMD_V4_SYNC = b"\x04"
CMD_V4_SETTLE = b"\x05"
CMD_V4_SETTLE_NATIVE = b"\x06"
CMD_ERC20_TRANSFER = b"\x07"
CMD_WETH_DEPOSIT = b"\x08"
CMD_WETH_WITHDRAW = b"\x09"
CMD_V4_UNLOCK = b"\x0A"
SEP = b"\xff"


# ── Command encoding helpers ──


def encode_uint24(v: int) -> bytes:
    """Encode a uint24 as 3 big-endian bytes."""
    return v.to_bytes(3, "big")


def encode_int24(v: int) -> bytes:
    """Encode an int24 as 3 big-endian bytes (sign-extended)."""
    return v.to_bytes(3, "big", signed=True)


def encode_uint160(v: int) -> bytes:
    """Encode a uint160 as 20 big-endian bytes."""
    return v.to_bytes(20, "big")


def encode_uint256(v: int) -> bytes:
    """Encode a uint256 as 32 big-endian bytes."""
    return v.to_bytes(32, "big")


def encode_int256(v: int) -> bytes:
    """Encode an int256 as 32 big-endian bytes (signed)."""
    return v.to_bytes(32, "big", signed=True)


def encode_idx(v: int) -> bytes:
    """Encode an address index as 1 byte."""
    return v.to_bytes(1, "big")


def encode_forward_len(v: int) -> bytes:
    """Encode forward_data length as 2 big-endian bytes."""
    return v.to_bytes(2, "big")


def enc_v4_swap(
    c0_idx: int,
    c1_idx: int,
    fee: int,
    tick_spacing: int,
    hooks_idx: int,
    zero_for_one: bool,
    amount_specified: int,
    sqrt_price_limit_x96: int,
) -> bytes:
    """
    Encode V4_SWAP command.
    [0x02][c0_idx:1][c1_idx:1][fee:3][ts:3][hooks_idx:1][zfo:1]
    [amount_specified:32][sqrt_limit:20][0xFF]
    """
    return b"".join([
        CMD_V4_SWAP,
        encode_idx(c0_idx),
        encode_idx(c1_idx),
        encode_uint24(fee),
        encode_int24(tick_spacing),
        encode_idx(hooks_idx),
        b"\x01" if zero_for_one else b"\x00",
        encode_int256(amount_specified),
        encode_uint160(sqrt_price_limit_x96),
        SEP,
    ])


def enc_v4_take(
    currency_idx: int,
    recipient_idx: int,
    amount: int,
) -> bytes:
    """
    Encode V4_TAKE command.
    [0x03][currency_idx:1][recipient_idx:1][amount:32][0xFF]
    """
    return b"".join([
        CMD_V4_TAKE,
        encode_idx(currency_idx),
        encode_idx(recipient_idx),
        encode_uint256(amount),
        SEP,
    ])


def enc_v4_sync(currency_idx: int) -> bytes:
    """
    Encode V4_SYNC command.
    [0x04][currency_idx:1][0xFF]
    """
    return b"".join([
        CMD_V4_SYNC,
        encode_idx(currency_idx),
        SEP,
    ])


def enc_v4_settle() -> bytes:
    """
    Encode V4_SETTLE command.
    [0x05][0xFF]
    """
    return b"".join([CMD_V4_SETTLE, SEP])


def enc_v4_settle_native(amount: int) -> bytes:
    """
    Encode V4_SETTLE_NATIVE command.
    [0x06][amount:32][0xFF]
    """
    return b"".join([
        CMD_V4_SETTLE_NATIVE,
        encode_uint256(amount),
        SEP,
    ])


def enc_erc20_transfer(
    token_idx: int,
    recipient_idx: int,
    amount: int,
) -> bytes:
    """
    Encode ERC20_TRANSFER command.
    [0x07][token_idx:1][recipient_idx:1][amount:32][0xFF]
    """
    return b"".join([
        CMD_ERC20_TRANSFER,
        encode_idx(token_idx),
        encode_idx(recipient_idx),
        encode_uint256(amount),
        SEP,
    ])


def enc_weth_deposit(amount: int) -> bytes:
    """
    Encode WETH_DEPOSIT command.
    [0x08][amount:32][0xFF]
    """
    return b"".join([
        CMD_WETH_DEPOSIT,
        encode_uint256(amount),
        SEP,
    ])


def enc_weth_withdraw(amount: int) -> bytes:
    """
    Encode WETH_WITHDRAW command.
    [0x09][amount:32][0xFF]
    """
    return b"".join([
        CMD_WETH_WITHDRAW,
        encode_uint256(amount),
        SEP,
    ])


def enc_v4_unlock(
    pm_idx: int,
    forward_data: bytes,
) -> bytes:
    """
    Encode V4_UNLOCK command.
    [0x0A][pm_idx:1][forward_len:2][forward_data:N][0xFF]
    """
    return b"".join([
        CMD_V4_UNLOCK,
        encode_idx(pm_idx),
        encode_forward_len(len(forward_data)),
        forward_data,
        SEP,
    ])


def enc_v2_swap(
    pool_idx: int,
    zero_for_one: bool,
    amount_out: int,
    recipient_idx: int,
    forward_data: bytes = b"",
) -> bytes:
    """
    Encode V2_SWAP command.
    [0x00][pool_idx:1][zfo:1][amount_out:32][recipient_idx:1]
    [forward_len:2][forward_data:N][0xFF]
    """
    return b"".join([
        CMD_V2_SWAP,
        encode_idx(pool_idx),
        b"\x01" if zero_for_one else b"\x00",
        encode_uint256(amount_out),
        encode_idx(recipient_idx),
        encode_forward_len(len(forward_data)),
        forward_data,
        SEP,
    ])


def enc_v3_swap(
    pool_idx: int,
    zero_for_one: bool,
    amount_specified: int,
    sqrt_price_limit_x96: int,
    recipient_idx: int,
    forward_data: bytes = b"",
) -> bytes:
    """
    Encode V3_SWAP command.
    [0x01][pool_idx:1][zfo:1][amount_specified:32][sqrt_limit:20]
    [recipient_idx:1][forward_len:2][forward_data:N][0xFF]
    """
    return b"".join([
        CMD_V3_SWAP,
        encode_idx(pool_idx),
        b"\x01" if zero_for_one else b"\x00",
        encode_int256(amount_specified),
        encode_uint160(sqrt_price_limit_x96),
        encode_idx(recipient_idx),
        encode_forward_len(len(forward_data)),
        forward_data,
        SEP,
    ])


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


# ── Fixtures ──


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
) -> ContractInstance:
    contract = project.cmd_executor.deploy(
        weth.address,
        value=WETH_DEPLOYMENT_WRAP_AMOUNT,
        sender=owner_account,
    )
    # Fund the executor with ETH for settlement
    contract.balance = 1000 * 10**18
    return contract


# ── Address table builder ──


class AddressTable:
    """Manages an indexed address lookup table for command encoding."""

    def __init__(self):
        self._addresses: list[str] = []
        self._index_map: dict[str, int] = {}

    def add(self, addr: str) -> int:
        """Add an address and return its index. Deduplicates."""
        if addr in self._index_map:
            return self._index_map[addr]
        idx = len(self._addresses)
        self._addresses.append(addr)
        self._index_map[addr] = idx
        return idx

    def to_list(self) -> list[str]:
        return list(self._addresses)


# ── Tests ──


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

        All settlement is pre-computed off-chain. The command stream
        encodes: V4_SWAP(A) → V4_SWAP(B) → V4_TAKE(WETH) →
        V4_SYNC(WETH) → V4_TRANSFER(WETH to PM) → V4_SETTLE
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

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        weth_idx = at.add(weth.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)  # for hooks

        # ── Build the inner command stream (runs inside unlockCallback) ──
        # V4 swap A: WETH→WBTC (V4 exact-input: negative amountSpecified)
        inner_commands = enc_v4_swap(
            c0_idx=weth_idx if pool_a_key[0] == weth.address else wbtc_idx,
            c1_idx=wbtc_idx if pool_a_key[1] == wbtc.address else weth_idx,
            fee=pool_a_key[2],
            tick_spacing=pool_a_key[3],
            hooks_idx=zero_idx,
            zero_for_one=pool_a_zfo,
            amount_specified=-pool_a_amount_in,
            sqrt_price_limit_x96=MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # V4 swap B: WBTC→WETH (specified amount, V4 negative for exact-input)
        inner_commands += enc_v4_swap(
            c0_idx=weth_idx if pool_b_key[0] == weth.address else wbtc_idx,
            c1_idx=wbtc_idx if pool_b_key[1] == wbtc.address else weth_idx,
            fee=pool_b_key[2],
            tick_spacing=pool_b_key[3],
            hooks_idx=zero_idx,
            zero_for_one=pool_b_zfo,
            amount_specified=-pool_b_amount_in,
            sqrt_price_limit_x96=MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # V4 take: profit WETH from PM to executor
        profit_weth = pool_b_amount_out - pool_a_amount_in
        inner_commands += enc_v4_take(
            currency_idx=weth_idx,
            recipient_idx=executor_idx,
            amount=profit_weth,
        )

        # V4 settle: sync WETH, transfer owed WETH to PM, settle
        # We owe PM: pool_a_amount_in WETH (from swap A)
        # PM owes us: pool_b_amount_out WETH (from swap B)
        # Net: PM owes us (pool_b_amount_out - pool_a_amount_in) = profit
        # We took profit already via V4_TAKE, so PM's WETH delta should now balance.
        # But we also need to settle the WETH we owe PM from swap A.
        # Swap A: owe 10 WETH to PM
        # Swap B: PM owes us 20 WETH
        # So after swaps: PM net delta[WETH] = -20 + 10 = -10 (PM owes us 10)
        # After V4_TAKE(10): PM delta[WETH] = 0
        # So we DON'T need additional WETH settlement — the take covered the surplus.
        # But we DO need to settle the WETH debt from swap A? No — swap A's debt is
        # netted against swap B's credit within PM's internal delta tracking.
        #
        # Actually, let's think about this from PM's perspective:
        # Swap A sends WETH from user → PM: PM.WETH += 10 (owe less to user)
        #   Actually no: swap A is WETH→WBTC.
        #   In V4: swap means we call PM.swap(), which:
        #   - If zero_for_one: user sends currency0 (WETH), receives currency1 (WBTC)
        #   - PM internal delta: delta[WETH] += amount_in, delta[WBTC] -= amount_out
        #
        # After swap A (WETH→WBTC, zfo=True):
        #   PM deltas: delta[WETH] = +10_eth, delta[WBTC] = -1_wbtc
        # After swap B (WBTC→WETH, zfo depends):
        #   pool_b_zfo = (pool_b_key[0] == wbtc.address)
        #   If zfo=True (WBTC is currency0): user sends WBTC, receives WETH
        #     PM deltas: delta[WETH] = +10 - 20 = -10, delta[WBTC] = -1 + 1 = 0
        #   If zfo=False (WETH is currency0): user sends WETH, receives WBTC
        #     PM deltas: delta[WETH] = +10 + 20 = +30, delta[WBTC] = -1 - 1 = -2
        #
        # For our path: WBTC→WETH at Pool B, so zero_for_one should be True
        # if WBTC is currency0 (i.e., WBTC address < WETH address).
        #
        # After both swaps: delta[WETH] = -10 (PM owes us), delta[WBTC] = 0
        # We take 10 WETH: delta[WETH] = 0
        # All deltas are zero. No settlement needed.
        #
        # Wait, but that's only true if the WBTC intermediate cancels exactly.
        # Swap A: delta[WBTC] = -1_wbtc (PM owes us 1 WBTC)
        # Swap B: delta[WBTC] = +1_wbtc (we owe PM 1 WBTC)
        # Net: delta[WBTC] = 0 — intermediate cancels ✓
        #
        # So the command stream is just:
        # V4_SWAP(A) → V4_SWAP(B) → V4_TAKE(WETH, 10) → done
        # No additional settlement needed!

        # ── Build the outer command stream ──
        commands = enc_v4_unlock(
            pm_idx=pm_idx,
            forward_data=inner_commands,
        )

        # ── Execute ──
        tx = executor.execute(
            at.to_list(),
            commands,
            0,  # bribe_bips
            True,  # skip_profit_check
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


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

        Requires explicit WETH settlement to PM (sync + transfer + settle)
        since WETH is not the output currency.
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
            fund_eth=True,
        )

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        weth_idx = at.add(weth.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        # ── Build inner command stream ──
        # After swap A: PM delta[WETH]=+10, delta[WBTC]=-1
        # After swap B: PM delta[WETH]=+10, delta[WBTC]=-1+1=0, delta[NATIVE]=-20
        # We owe PM 10 WETH (delta>0 means PM has credit) and PM owes us 20 ETH (delta<0 means PM has debit)
        # Wait, I need to check sign convention more carefully.
        #
        # V4 BalanceDelta sign convention (from the PM's internal tracking):
        # - Positive delta = PM owes user (user should take)
        # - Negative delta = user owes PM (user should settle)
        #
        # Swap A (WETH→WBTC, zfo=True): user sends WETH, receives WBTC
        #   delta[WETH] += amount_in  (+10_eth, PM owes us... no, WE owe PM? )
        #
        # Actually, in V4, the BalanceDelta returned by swap() uses this convention:
        #   amount0 = positive if pool gained token0, negative if pool lost token0
        #   amount1 = positive if pool gained token1, negative if pool lost token1
        #
        # So for swap A (zfo=True, WETH→WBTC):
        #   Pool gains WETH (currency0 if WETH < WBTC) -> amount0 > 0
        #   Pool loses WBTC (currency1) -> amount1 < 0
        #   delta[WETH] += +10 (pool gained WETH), delta[WBTC] += -1 (pool lost WBTC)
        #
        # From the user's perspective (the executor):
        #   delta[WETH] > 0 means we OWE PM WETH (need to settle)
        #   delta[WBTC] < 0 means PM OWES us WBTC (need to take)
        #
        # After swap B (WBTC→ETH):
        #   If zfo=True (WBTC is currency0): pool gains WBTC, loses ETH
        #     delta[WBTC] += +1 (was -1, now 0), delta[ETH] += -20
        #   If zfo=False (ETH is currency0): pool gains ETH, loses WBTC
        #     This doesn't make sense for WBTC→ETH direction
        #
        # Wait, let me re-check the fake PM's delta tracking.
        # The fake PM does:
        #   self.t_deltas[msg.sender][key.currency0] += amount0_delta
        #   self.t_deltas[msg.sender][key.currency1] += amount1_delta
        # where amount0_delta and amount1_delta are computed from swap amounts.
        #
        # For swap A (WETH→WBTC, zfo=True):
        #   currency0 = WETH (sorted: WETH address < WBTC address based on actual values)
        #   amount_in = 10 WETH (sent by user to PM)
        #   amount_out = 1 WBTC (sent by PM to user)
        #   amount0_delta = +10_eth (currency0=WETH, pool gained)
        #   amount1_delta = -1_wbtc (currency1=WBTC, pool lost)
        #   delta[WETH] += +10, delta[WBTC] += -1
        #
        # For swap B (WBTC→ETH):
        #   pool_b_key = (NATIVE_ADDRESS, WBTC)
        #   zfo depends on which is currency0 and which is currency1
        #   NATIVE_ADDRESS is 0x0...0 which sorts before any real address
        #   So currency0 = NATIVE, currency1 = WBTC
        #   WBTC→ETH means user sends WBTC (currency1), receives ETH (currency0)
        #   This is zfo=False (one_for_zero: input is currency1, output is currency0)
        #   Wait, pool_b_zfo is set above as: pool_b_zfo = pool_b_key[0] == wbtc.address
        #   pool_b_key[0] = NATIVE_ADDRESS, wbtc.address != NATIVE_ADDRESS, so zfo = False
        #   
        #   With zfo=False:
        #     amount_in = 1 WBTC (sent by user, this is currency1)
        #     amount_out = 20 ETH (sent by PM, this is currency0)
        #   In the fake PM:
        #     amount1_delta = +1_wbtc (pool gained WBTC)  [for zfo=False, amount1 is +amount_in]
        #     amount0_delta = -20_eth (pool lost ETH)  [for zfo=False, amount0 is -amount_out]
        #
        # Wait, let me re-read the fake PM's delta computation carefully.
        # 
        # The fake PM does:
        #   amount0_delta: int128 = convert(swap_amounts[pool_id][key.currency0], int128)
        #   amount1_delta: int128 = convert(swap_amounts[pool_id][key.currency1], int128)
        #   if params.zero_for_one:
        #       amount0_delta = -amount0_delta
        #   else:
        #       amount1_delta = -amount1_delta
        #
        # For swap B (zfo=False):
        #   amount0 = 20 ETH (currency0 is NATIVE), amount1 = 1 WBTC (currency1)
        #   zfo=False → amount1_delta is negated
        #   amount0_delta = +20_eth (pool gained ETH), amount1_delta = -1_wbtc (pool lost WBTC)
        #   
        #   Wait, that's wrong. Let me re-think.
        #   amount_in = 1 WBTC (user sends WBTC to pool)
        #   amount_out = 20 ETH (pool sends ETH to user)
        #   
        #   The swap amounts map stores:
        #     swap_amounts[pool_id][currency0=NATIVE] = 20 (ETH)
        #     swap_amounts[pool_id][currency1=WBTC] = 1 (WBTC)
        #   
        #   Wait no, the map stores amount_in and amount_out, not per-currency.
        #   Let me re-read set_next_swap:
        #     currency_in = self._input_currency(pool_key, zero_for_one)
        #     currency_out = self._output_currency(pool_key, zero_for_one)
        #     self.swap_amounts[pool_id][currency_in] = amount_in
        #     self.swap_amounts[pool_id][currency_out] = amount_out
        #
        #   For zfo=False, currency_in = currency1 = WBTC, currency_out = currency0 = NATIVE
        #   swap_amounts[pool_id][WBTC] = 1, swap_amounts[pool_id][NATIVE] = 20
        #
        #   Then in swap(), the deltas are:
        #     amount0_delta = swap_amounts[pool_id][currency0] = swap_amounts[pool_id][NATIVE] = 20
        #     amount1_delta = swap_amounts[pool_id][currency1] = swap_amounts[pool_id][WBTC] = 1
        #     zfo=False → amount1_delta = -1
        #
        #   Final: amount0_delta = +20, amount1_delta = -1
        #   delta[NATIVE] += +20, delta[WBTC] += -1
        #
        # Wait, that means delta[NATIVE] += +20 (pool gained ETH? That doesn't make sense,
        # the pool is sending 20 ETH to the user.)
        #
        # Hmm, I think the sign convention in the fake PM is:
        #   Positive = pool gained (user owes settlement)
        #   Negative = pool lost (PM owes user a take)
        #
        # So after both swaps:
        #   delta[WETH] = +10 (pool gained 10 WETH from swap A)
        #   delta[WBTC] = -1 + (-1) = -2? No...
        #
        # After swap A: delta[WBTC] += -1 (PM's sign: pool lost 1 WBTC → take)
        # After swap B: delta[WBTC] += -1 (pool lost 1 WBTC → take)
        # Total delta[WBTC] = -2
        #
        # Wait, that can't be right. WBTC is the intermediate — swap A input is WETH,
        # swap A output is WBTC. Swap B input is WBTC, swap B output is ETH.
        # WBTC should approximately cancel.
        #
        # I think the issue is my sign convention confusion. Let me just look at the
        # actual deltas computed by the fake PM. The important thing is that at the end,
        # all deltas must be zero for unlock to succeed.
        #
        # The test just needs to succeed — if I get the settlement commands wrong, it will
        # revert with CurrencyNotSettled. Let me just try the minimal approach and see.

        # After swap A (WETH→WBTC): we owe PM 10 WETH, PM owes us 1 WBTC
        # After swap B (WBTC→ETH): we owe PM 1 WBTC, PM owes us 20 ETH
        # Net: we owe PM 10 WETH (settle), PM owes us 20 ETH (take) + WBTC cancels

        inner_commands = b""

        # V4 swap A
        inner_commands += enc_v4_swap(
            c0_idx=weth_idx if pool_a_key[0] == weth.address else wbtc_idx,
            c1_idx=wbtc_idx if pool_a_key[1] == wbtc.address else weth_idx,
            fee=pool_a_key[2],
            tick_spacing=pool_a_key[3],
            hooks_idx=zero_idx,
            zero_for_one=pool_a_zfo,
            amount_specified=-pool_a_amount_in,
            sqrt_price_limit_x96=MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # V4 swap B
        inner_commands += enc_v4_swap(
            c0_idx=native_idx if pool_b_key[0] == NATIVE_ADDRESS else wbtc_idx,
            c1_idx=wbtc_idx if pool_b_key[1] == wbtc.address else native_idx,
            fee=pool_b_key[2],
            tick_spacing=pool_b_key[3],
            hooks_idx=zero_idx,
            zero_for_one=pool_b_zfo,
            amount_specified=-pool_b_amount_in,
            sqrt_price_limit_x96=MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # Take 20 ETH (profit) from PM
        inner_commands += enc_v4_take(
            currency_idx=native_idx,
            recipient_idx=executor_idx,
            amount=pool_b_amount_out,
        )

        # Settle 10 WETH: sync, transfer to PM, settle
        inner_commands += enc_v4_sync(weth_idx)
        inner_commands += enc_erc20_transfer(weth_idx, pm_idx, pool_a_amount_in)
        inner_commands += enc_v4_settle()

        # ── Build outer command stream ──
        commands = enc_v4_unlock(
            pm_idx=pm_idx,
            forward_data=inner_commands,
        )

        # ── Execute ──
        tx = executor.execute(
            at.to_list(),
            commands,
            0,
            True,
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

        Similar to the tstore_executor's USDC intermediate test, but
        without a delta ledger — settlement amounts are pre-computed.
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

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        # ── Build inner command stream ──
        inner_commands = b""

        # V4 swap A
        inner_commands += enc_v4_swap(
            c0_idx=weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            c1_idx=usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            fee=pool_a_key[2],
            tick_spacing=pool_a_key[3],
            hooks_idx=zero_idx,
            zero_for_one=pool_a_zfo,
            amount_specified=-pool_a_amount_in,
            sqrt_price_limit_x96=MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # V4 swap B
        inner_commands += enc_v4_swap(
            c0_idx=native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
            c1_idx=usdc_idx if pool_b_key[1] == usdc.address else native_idx,
            fee=pool_b_key[2],
            tick_spacing=pool_b_key[3],
            hooks_idx=zero_idx,
            zero_for_one=pool_b_zfo,
            amount_specified=-pool_b_amount_in,
            sqrt_price_limit_x96=MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # Take ETH profit
        inner_commands += enc_v4_take(
            currency_idx=native_idx,
            recipient_idx=executor_idx,
            amount=pool_b_amount_out,
        )

        # Settle WETH owed to PM
        inner_commands += enc_v4_sync(weth_idx)
        inner_commands += enc_erc20_transfer(weth_idx, pm_idx, pool_a_amount_in)
        inner_commands += enc_v4_settle()

        # ── Build outer command stream ──
        commands = enc_v4_unlock(
            pm_idx=pm_idx,
            forward_data=inner_commands,
        )

        # ── Execute ──
        tx = executor.execute(
            at.to_list(),
            commands,
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted — likely CurrencyNotSettled")
