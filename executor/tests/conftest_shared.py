"""
Shared encoding helpers and constants for cmd_executor tests.
"""

from eth_utils.address import to_checksum_address
from pathlib import Path

NATIVE_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")

Q96 = 79228162514264337593543950336  # 2^96


def _isqrt(n):
    """Integer square root using Newton's method."""
    if n < 2:
        return n
    x = n
    y = (x + 1) // 2
    while y < x:
        x = y
        y = (x + n // x) // 2
    return x

WETH_DEPLOYMENT_WRAP_AMOUNT = 10 * 10**18

# Default V2 liquidity provision for runtime K-invariant verification.
# 100x the typical swap amount with a 1:2000 WETH:USDC price ratio.
V2_LIQUIDITY_WETH = 100 * 10**18
V2_LIQUIDITY_USDC = 200_000 * 10**6

MIN_SQRT_PRICE_X96 = 4295128739
MAX_SQRT_PRICE_X96 = 1461446703485210103287273052203988822378723970342

# ── Command opcodes (grouped by protocol at 0x10 boundaries) ──

# Control / Preprocessing: 0x00–0x0F
CMD_SET_ADDRESS = b"\x00"
# 0x01, 0x02, 0x03 reserved — were preprocessing commands, now in config param

# ERC20 / ETH / Native: 0x10–0x1F
CMD_ERC20_TRANSFER = b"\x10"
CMD_ERC20_XFER_BALANCE = b"\x11"
CMD_WETH_DEPOSIT = b"\x12"
CMD_WETH_WITHDRAW = b"\x13"
CMD_WETH_DEPOSIT_ALL = b"\x14"
CMD_WETH_WITHDRAW_ALL = b"\x15"
CMD_SEND_ETH = b"\x16"
CMD_SEND_ETH_ALL = b"\x17"

# V2: 0x20–0x2F
CMD_V2_SWAP_COMPACT = b"\x20"
CMD_V2_SWAP_CALC = b"\x21"
CMD_V2_SWAP_DIRECT = b"\x22"

# V3: 0x30–0x3F
CMD_V3_SWAP_COMPACT = b"\x30"
CMD_V3_SWAP_DELTA = b"\x31"

# V4 Swaps: 0x40–0x4F
CMD_V4_SWAP_COMPACT = b"\x40"
CMD_V4_SWAP_DYNAMIC = b"\x41"
CMD_V4_BATCH = b"\x42"

# V4 Settlement / ERC6909: 0x50–0x5F
CMD_V4_UNLOCK = b"\x50"
CMD_V4_TAKE = b"\x51"
CMD_V4_TAKE_COMPACT = b"\x52"
CMD_V4_TAKE_DELTA = b"\x53"
CMD_V4_SYNC = b"\x54"
# 0x55 = V4_SETTLE — see enc_v4_settle() below
CMD_V4_SETTLE_DELTA = b"\x56"
CMD_V4_SETTLE_ALL = b"\x57"
CMD_V4_MINT_COMPACT = b"\x58"
CMD_V4_BURN_COMPACT = b"\x59"

# Stream separators
BEGIN_PREPROCESSING = b"\xfe"  # First byte: signals a preprocessing section follows
BEGIN_EXECUTION = b"\xff"       # End of preprocessing / start of execution

# Address table sentinels — reserved index values that bypass TLOAD for common addresses
# 0xFF = NATIVE_ADDRESS (address(0) / no hooks / native ETH)
# 0xFE = WETH address
# Backward-compatible: old encodings with these addresses in the table still work.
WETH_SENTINEL = 0xFE
NATIVE_SENTINEL = 0xFF
SELF_SENTINEL = 0xFD
PM_SENTINEL = 0xFC
USER0_SENTINEL = None  # DEPRECATED — user sentinels removed from the contract.
USER1_SENTINEL = None  # USDC/WBTC are now regular t_addresses entries.

# Terminators — 0xFF is the section separator (end of preprocessing / start of execution)

# ── Encoding helpers ──


def _e(v: int, n: int = 32, signed: bool = False) -> bytes:
    """Encode an integer as n big-endian bytes."""
    return v.to_bytes(n, "big", signed=signed)


def enc_v2_swap_compact(pool_idx, zfo, amount_out, recipient_idx, fee=30, forward_data=b""):
    """
    V2_SWAP_COMPACT: [0x20][pool_idx:1][zfo:1][amount_out:12][recipient_idx:1][fee:2]
    [forward_len:1][forward_data:N]
    = 19 + N bytes

    fee is a fraction of 10000 (30 = 0.3% UniswapV2, 25 = 0.25% PancakeSwap).
    Written to t_v2_pair_fee[pool] before swap() for correct auto-pay.
    """
    return b"".join([
        CMD_V2_SWAP_COMPACT, _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_out, 12), _e(recipient_idx, 1), _e(fee, 2),
        _e(len(forward_data), 1), forward_data,
    ])


def enc_v3_swap_compact(pool_idx, zfo, amount_specified, recipient_idx, forward_data=b""):
    """
    V3_SWAP_COMPACT: [0x30][pool_idx:1][zfo:1][amount_specified:12][recipient_idx:1]
    [forward_len:1][forward_data:N]
    = 17 + N bytes
    """
    return b"".join([
        CMD_V3_SWAP_COMPACT, _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_specified, 12), _e(recipient_idx, 1),
        _e(len(forward_data), 1), forward_data,
    ])


def enc_v4_swap_compact(c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo, amount_u128):
    """
    V4_SWAP_COMPACT: [0x40][c0_idx:1][c1_idx:1][fee:2][ts:2][hooks_idx:1][zfo:1]
    [amount:12] = 21 bytes
    """
    return b"".join([
        CMD_V4_SWAP_COMPACT,
        _e(c0_idx, 1), _e(c1_idx, 1), _e(fee, 2), _e(tick_spacing, 2, signed=True),
        _e(hooks_idx, 1), b"\x01" if zfo else b"\x00",
        _e(amount_u128, 12),
    ])


def enc_v4_swap_dynamic(c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo):
    """
    V4_SWAP_DYNAMIC: [0x41][c0_idx:1][c1_idx:1][fee:2][ts:2][hooks_idx:1][zfo:1]
     = 9 bytes (no amount, default sqrt_price_limit)
    """
    return b"".join([
        CMD_V4_SWAP_DYNAMIC,
        _e(c0_idx, 1), _e(c1_idx, 1), _e(fee, 2), _e(tick_spacing, 2, signed=True),
        _e(hooks_idx, 1), b"\x01" if zfo else b"\x00",
    ])


def enc_v4_batch(swaps):
    """
    V4_BATCH: [0x42][num_swaps:1][entry_1:20]...[entry_N:20]
    
    Each 20-byte entry: [c0_idx:1][c1_idx:1][fee:2][ts:2][hooks_idx:1][zfo:1][amount:12]
    amount = 0 → dynamic (read from PM exttload delta of input currency)
    amount > 0 → explicit exact-input amount
    
    After all swaps, auto-settles all nonzero deltas (take positive, settle negative).
    This replaces separate V4_TAKE/V4_TAKE_DELTA + V4_SETTLE_DELTA commands.
    
    Args:
        swaps: list of (c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo, amount_u128) tuples
    
    Returns:
        Encoded command bytes
    """
    inner = b"".join([CMD_V4_BATCH, _e(len(swaps), 1)])
    for c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo, amount_u128 in swaps:
        inner += b"".join([
            _e(c0_idx, 1), _e(c1_idx, 1),
            _e(fee, 2), _e(tick_spacing, 2, signed=True),
            _e(hooks_idx, 1), b"\x01" if zfo else b"\x00",
            _e(amount_u128, 12),
        ])
    return inner


def enc_v3_swap_delta(pool_idx, zfo, recipient_idx):
    """
    V3_SWAP_DELTA: [0x31][pool_idx:1][zfo:1][recipient_idx:1]
    = 4 bytes (amount from PM exttload + default sqrt + auto-pay)
    """
    return b"".join([CMD_V3_SWAP_DELTA, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(recipient_idx, 1)])


def enc_v2_swap_calc(pool_idx, zfo, recipient_idx, fee=30):
    """
    V2_SWAP_CALC: [0x21][pool_idx:1][zfo:1][recipient_idx:1][fee:2]
    fee is a fraction of 10000 (30 = 0.3% UniswapV2, 25 = 0.25% PancakeSwap).
    """
    return b"".join([CMD_V2_SWAP_CALC, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(recipient_idx, 1), _e(fee, 2)])


def enc_v2_swap_direct(pool_idx, zfo, amount_out, recipient_idx):
    """
    V2_SWAP_DIRECT: [0x22][pool_idx:1][zfo:1][amount_out:16][recipient_idx:1]
    = 20 bytes. V2 swap with explicit amount_out and no callback (data=b"").

    The V2 pair must already hold the input tokens (excess balance) from a
    prior ERC20_TRANSFER or V4_TAKE. The K-invariant check inside the
    pair's swap() function verifies correctness.

    Unlike V2_SWAP_CALC which computes amount_out on-chain (4 staticcalls),
    this uses the explicit amount from calldata — saving ~10K gas on cold
    slots at the cost of 14 extra calldata bytes.

    No fee field needed — the V2 pair knows its own fee and enforces K.
    """
    return b"".join([
        CMD_V2_SWAP_DIRECT, _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_out, 12), _e(recipient_idx, 1),
    ])


def enc_v4_take(currency_idx, recipient_idx, amount):
    return b"".join([CMD_V4_TAKE, _e(currency_idx, 1), _e(recipient_idx, 1), _e(amount)])


def enc_v4_take_delta(currency_idx, recipient_idx):
    return b"".join([CMD_V4_TAKE_DELTA, _e(currency_idx, 1), _e(recipient_idx, 1)])


def enc_v4_take_compact(currency_idx, recipient_idx, amount_u128):
    """
    V4_TAKE_COMPACT: [0x52][currency_idx:1][recipient_idx:1][amount:12]
    = 15 bytes (vs 35 for V4_TAKE, saves 20 bytes)
    """
    return b"".join([CMD_V4_TAKE_COMPACT, _e(currency_idx, 1), _e(recipient_idx, 1), _e(amount_u128, 12)])


def enc_v4_mint_compact(currency_idx, recipient_idx, amount_u128):
    """
    V4_MINT_COMPACT: [0x58][currency_idx:1][recipient_idx:1][amount:12]
    = 15 bytes. Converts positive PM delta → ERC6909 balance. No transfer out.
    """
    return b"".join([CMD_V4_MINT_COMPACT, _e(currency_idx, 1), _e(recipient_idx, 1), _e(amount_u128, 12)])


def enc_v4_burn_compact(currency_idx, amount_u128):
    """
    V4_BURN_COMPACT: [0x59][currency_idx:1][amount:12]
    = 14 bytes. Converts executor's own ERC6909 balance → payable PM delta.
    """
    return b"".join([CMD_V4_BURN_COMPACT, _e(currency_idx, 1), _e(amount_u128, 12)])


def enc_v4_sync(currency_idx):
    return b"".join([CMD_V4_SYNC, _e(currency_idx, 1)])


def enc_v4_settle():
    return b"".join([b"\x55"])


def enc_v4_settle_delta(currency_idx):
    return b"".join([CMD_V4_SETTLE_DELTA, _e(currency_idx, 1)])


def enc_v4_settle_all():
    return b"".join([CMD_V4_SETTLE_ALL])


def enc_erc20_transfer(token_idx, recipient_idx, amount):
    return b"".join([CMD_ERC20_TRANSFER, _e(token_idx, 1), _e(recipient_idx, 1), _e(amount, 12)])


def enc_erc20_xfer_balance(token_idx, recipient_idx):
    return b"".join([CMD_ERC20_XFER_BALANCE, _e(token_idx, 1), _e(recipient_idx, 1)])


def enc_weth_deposit(amount):
    return b"".join([CMD_WETH_DEPOSIT, _e(amount)])


def enc_weth_deposit_all():
    return b"".join([CMD_WETH_DEPOSIT_ALL])


def enc_weth_withdraw(amount):
    return b"".join([CMD_WETH_WITHDRAW, _e(amount)])


def enc_weth_withdraw_all():
    return b"".join([CMD_WETH_WITHDRAW_ALL])


def make_bribe_config(bips, recipient_idx=0):
    """Pack bribe configuration for the config parameter.

    Args:
        bips:          Basis points (0-10000). 0 = no bribe.
        recipient_idx: 0 = coinbase (default), 1-31 = address table index.
    """
    return (recipient_idx << 24) | (bips << 8)


def enc_set_address(addr):
    """SET_ADDRESS: [0x00][address:20] — append address to lookup table by insertion order"""
    addr_str = addr if isinstance(addr, str) else addr.address
    addr_bytes = bytes.fromhex(addr_str[2:])
    assert len(addr_bytes) == 20, f"Invalid address length: {len(addr_bytes)}"
    return CMD_SET_ADDRESS + addr_bytes


def enc_set_addresses(address_table):
    """Encode SET_ADDRESS commands for all addresses in the table (preserves insertion-order indices)."""
    result = b""
    for addr in address_table._addresses:
        result += enc_set_address(addr)
    return result


def make_config(check_mode=0, bribe_bips=0, bribe_recipient_idx=0, expected_value=0):
    """Pack configuration into a single uint256 for the ABI config parameter.

    Layout:
      bits 0-7:    check_mode (0=skip, 1=WETH+ETH, 2=ERC6909)
      bits 8-23:   bribe_bips (0=no bribe, 1-10000=basis points)
      bits 24-31:  bribe_recipient_idx (0=coinbase, 1-31=address table index)
      bits 32-255: expected_value (pre-tx balance for the selected mode)
    """
    assert 0 <= check_mode <= 2
    assert 0 <= bribe_bips <= 10000
    assert 0 <= bribe_recipient_idx <= 31
    return (expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | check_mode


def enc_preamble(address_table, skip_profit=False):
    """Encode the preprocessing section (just SET_ADDRESS commands) + separator.

    Builds: [SET_ADDRESS commands][0xFF]
    The 0xFF separator marks the end of preprocessing and the start of
    execution commands. All other configuration (profit check, bribes)
    goes in the ABI config parameter.

    Args:
        address_table:    AddressTable instance with all needed addresses
        skip_profit:      Ignored (kept for API compatibility). Profit check
                          is now controlled by the config parameter.
    """
    # NOTE: 0xFE (BEGIN_PREPROCESSING) prefix is omitted — _preprocess
    # unconditionally starts at offset 0, reading SET_ADDRESS commands directly.
    # Saves 1 byte of calldata + the preprocessing check in execute().
    #
    return enc_set_addresses(address_table) + BEGIN_EXECUTION


def enc_send_eth(recipient_idx, amount):
    """SEND_ETH: [0x16][recipient_idx:1][amount:16] — send uint128 ETH to address"""
    return b"".join([CMD_SEND_ETH, _e(recipient_idx, 1), _e(amount, 12)])


def enc_send_eth_all(recipient_idx):
    """SEND_ETH_ALL: [0x17][recipient_idx:1] — send all ETH to address"""
    return b"".join([CMD_SEND_ETH_ALL, _e(recipient_idx, 1)])


def enc_v4_unlock(forward_data):
    return b"".join([CMD_V4_UNLOCK, _e(len(forward_data), 1), forward_data])


# ── Shared test utilities ──


def _make_pool_key(currency0, currency1, fee=0, tick_spacing=60, hooks=ZERO_ADDRESS):
    c0, c1 = sorted([currency0, currency1], key=lambda addr: addr.lower())
    return (c0, c1, fee, tick_spacing, hooks)


def v2_get_amount_out(amount_in, reserve_in, reserve_out, fee):
    """Compute V2 swap output using the (10000 - fee) formula.

    Matches the real UniswapV2Pair getAmountOut:
        feeMultiplier = 10000 - fee
        amountOut = (amountIn * feeMultiplier * reserveOut) / (reserveIn * 10000 + amountIn * feeMultiplier)
    """
    fm = 10000 - fee
    return (amount_in * fm * reserve_out) // (reserve_in * 10000 + amount_in * fm)


def _v2_reserves_for_canned_swap(amount_in, amount_out, fee=30, liquidity_factor=100):
    """Compute minimum V2 reserves that produce the specified amount_out for amount_in.

    Given desired amount_in and amount_out, computes R_in and R_out such that
    v2_get_amount_out(amount_in, R_in, R_out, fee) == amount_out (plus minor
    integer rounding). Useful for gas benchmarks and tests that need specific
    "canned" amounts to be K-invariant-consistent.

    R_out is set to amount_out * liquidity_factor for ample liquidity.
    R_in is derived from the getAmountOut formula.

    Args:
        amount_in:       Desired swap input amount
        amount_out:      Desired swap output amount
        fee:             Swap fee as fraction of 10000 (default 30 = 0.3%)
        liquidity_factor: Multiplier on output reserve (default 100)

    Returns:
        (reserve_in, reserve_out): Reserve amounts to mint to the pair
    """
    fm = 10000 - fee
    reserve_out = amount_out * liquidity_factor
    # From: amount_out = amount_in * fm * R_out / (R_in * 10000 + amount_in * fm)
    # => R_in = amount_in * fm * (R_out - amount_out) / (amount_out * 10000)
    reserve_in = amount_in * fm * (reserve_out - amount_out) // (amount_out * 10000)
    return reserve_in, reserve_out


def _setup_v2_pair(v2_pair, input_token, output_token, owner, amount_in, fee=30):
    """Set up a V2 pair with ample liquidity for runtime K-invariant verification.

    Mints both tokens to the pair, calls sync() to snapshot reserves, then
    computes the swap output using the V2 constant-product formula with fees.

    The pair must be freshly deployed (no prior state).

    Args:
        v2_pair:       The fake V2 pair contract
        input_token:   The token being sold to the pair
        output_token:  The token being bought from the pair
        owner:         The account performing the minting
        amount_in:     The amount of input_token being sold
        fee:           Swap fee as fraction of 10000 (default 30 = 0.3%)

    Returns:
        (zfo, amount_out): zero_for_one flag and computed output amount
    """
    # Provide ample liquidity — 100x the swap amount for minimal price impact
    liquidity_input = amount_in * 100
    liquidity_output = amount_in * 100  # rough scale; reserves after sync give real ratio

    input_token.mint(v2_pair.address, liquidity_input, sender=owner)
    output_token.mint(v2_pair.address, liquidity_output, sender=owner)
    v2_pair.sync(sender=owner)

    zfo = v2_pair.token0() == input_token.address
    reserve_in = input_token.balanceOf(v2_pair.address)
    reserve_out = output_token.balanceOf(v2_pair.address)
    amount_out = v2_get_amount_out(amount_in, reserve_in, reserve_out, fee)
    return zfo, amount_out


def _setup_v3(pool, input_token, output_token, amount_in, amount_out, owner, liquidity_factor=100):
    """Set up a V3 pool with ample liquidity at the desired price.

    Computes sqrtPriceX96 from the amount_in/amount_out ratio (implied price),
    then initializes the pool and provides liquidity_factor * amount of each
    token as liquidity. With liquidity_factor=100 (default), price impact on the
    first swap is ~1%, making the output close to the canned amount_out.

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

    liq_input = amount_in * liquidity_factor
    liq_output = amount_out * liquidity_factor
    input_token.mint(pool.address, liq_input, sender=owner)
    output_token.mint(pool.address, liq_output, sender=owner)
    pool.add_liquidity(sender=owner)

    actual_out = pool.get_amount_out(amount_in, zfo)

    return zfo, actual_out


def _setup_v4_swap(pool_manager, owner, pool_key, amount_in, amount_out, zfo, output_token=None, fund_eth=False):
    if fund_eth:
        pool_manager.balance += amount_out
    elif output_token is not None:
        output_token.mint(pool_manager.address, amount_out, sender=owner)
    pool_manager.set_next_swap(pool_key, amount_in, amount_out, zfo, b"", sender=owner)


class AddressTable:
    """Address lookup table with sentinel support.

    Reserves sentinel index values that skip TLOAD and SET_ADDRESS in the contract:
    0xFC = POOL_MANAGER_ADDR, 0xFD = executor (self), 0xFE = WETH_ADDR, 0xFF = ZERO_ADDRESS / native ETH.
    Regular addresses are assigned indices 0-0xFB via SET_ADDRESS commands.

    No path-specific tokens are baked into the contract — user0_addr/user1_addr
    kwargs are accepted for backward compat but treated as *regular table entries*
    (they get a SET_ADDRESS + TLOAD index like every other address), NOT as
    sentinel bytes.
    """
    def __init__(self, weth_addr=None, executor_addr=None, pm_addr=None,
                 user0_addr=None, user1_addr=None):
        self._addresses = []
        self._index_map = {}
        self._weth_addr = weth_addr
        self._executor_addr = executor_addr
        self._pm_addr = pm_addr
        # user0_addr / user1_addr are NOT sentinels anymore — pre-register them
        # as ordinary table entries so callers that pass them get a stable index
        # and an emitted SET_ADDRESS, identical to any other address.
        for a in (user0_addr, user1_addr):
            if a is not None:
                self.add(a)

    def add(self, addr):
        addr_str = addr if isinstance(addr, str) else addr.address
        if addr_str == ZERO_ADDRESS:
            return NATIVE_SENTINEL
        if self._weth_addr is not None and addr_str == self._weth_addr:
            return WETH_SENTINEL
        if self._executor_addr is not None and addr_str == self._executor_addr:
            return SELF_SENTINEL
        if self._pm_addr is not None and addr_str == self._pm_addr:
            return PM_SENTINEL
        if addr in self._index_map:
            return self._index_map[addr]
        idx = len(self._addresses)
        self._addresses.append(addr)
        self._index_map[addr] = idx
        return idx

    def to_list(self):
        return list(self._addresses)


# ── Access list & gas benchmark helpers ──


def snapshot_state():
    """Take an EVM snapshot and return the snapshot ID.

    Requires an active Anvil/Foundry provider. The snapshot captures
    the full EVM state (storage, balances, nonce) and can be restored
    with revert_to_snapshot().

    Returns:
        int: The snapshot ID for use with revert_to_snapshot().
    """
    from ape import chain
    return chain.provider.web3.provider.make_request("evm_snapshot", [])


def revert_to_snapshot(snapshot_id):
    """Revert the EVM state to a previously taken snapshot.

    After reverting, a new snapshot must be taken if further reverts
    are needed (Anvil consumes the snapshot on revert).

    Args:
        snapshot_id: The snapshot ID returned by snapshot_state().
    """
    from ape import chain
    chain.provider.web3.provider.make_request("evm_revert", [snapshot_id])


def compute_access_list(cmd_ex, owner, commands):
    """
    Call eth_createAccessList to compute the optimal EIP-2930 access list.

    Traces the transaction against current chain state and returns the
    set of (address, storageKeys) that the execution will touch. In
    production, MEV searchers include this access list when submitting
    transactions — pre-warming storage slots saves 800–18,200 gas
    depending on the path.

    Args:
        cmd_ex:    The cmd_executor contract instance
        owner:     The sender account
        commands:  Encoded command bytes (including SET_ADDRESS prefix)

    Returns:
        list[dict]: Access list entries in the format accepted by Ape's
                    access_list= kwarg: [{"address": ..., "storageKeys": [...]}]
    """
    from ape import chain
    w3 = chain.provider.web3
    calldata = cmd_ex.execute.encode_input(commands)
    tx_dict = {
        'from': owner.address,
        'to': cmd_ex.address,
        'data': calldata,
        'value': 0,
    }
    al_result = w3.eth.create_access_list(tx_dict)
    return al_result['accessList']


def run_gas_benchmark(
    cmd_ex, owner, commands,
    setup_fn,
    label="",
):
    """
    Run a gas benchmark with and without an EIP-2930 access list.

    Automates the standard benchmark flow:
      1. Call setup_fn() to provision pool state
      2. Execute WITHOUT access list → baseline gas
      3. Call setup_fn() again to reset state (consumed by step 2)
      4. Compute optimal access list via eth_createAccessList
         (Note: on Anvil, this simulation DOES consume set_next_swap
         state on fake pools, so setup_fn must be called again after)
      5. Call setup_fn() a third time to reset state consumed by step 4
      6. Execute WITH access list → optimized gas

    Args:
        cmd_ex:    The cmd_executor contract instance
        owner:     The sender account
        commands:  Encoded command bytes (including SET_ADDRESS prefix)
        setup_fn:  Zero-arg callable that provisions pool state.
                   Must be idempotent — safe to call multiple times.
        label:     Human-readable label for the print output.

    Returns:
        dict with keys: gas_no_al, gas_with_al, access_list, gas_saved
    """
    # Step 1+2: Execute WITHOUT access list
    setup_fn()
    tx_no_al = cmd_ex.execute(commands, sender=owner)
    gas_no_al = tx_no_al.gas_used

    # Step 3+4: Reset state, compute access list
    setup_fn()
    access_list = compute_access_list(cmd_ex, owner, commands)

    # Step 5+6: Reset state consumed by eth_createAccessList trace,
    # then execute WITH access list
    setup_fn()
    tx_with_al = cmd_ex.execute(commands, sender=owner,
                                access_list=access_list)
    gas_with_al = tx_with_al.gas_used
    gas_saved = gas_no_al - gas_with_al

    if label:
        print(f"\n  {label}:")
        print(f"    No AL:  {gas_no_al:>8,} gas")
        print(f"    w/ AL:  {gas_with_al:>8,} gas  (saves {gas_saved:+,}, {len(access_list)} AL entries)")

    return {
        'gas_no_al': gas_no_al,
        'gas_with_al': gas_with_al,
        'access_list': access_list,
        'gas_saved': gas_saved,
    }



# ── Gas result recording ──
# Each gas-marked test appends one line to .gas-results (project root).
# On Linux, small append-mode writes (<4096 bytes) are atomic, so
# xdist workers can safely write concurrently without a lock.

_GAS_RESULTS_PATH = Path(__file__).resolve().parent.parent / ".gas-results"


def record_gas(label: str, gas_used: int) -> None:
    """Append a GAS result line to the shared results file.

    Format: ``GAS <label> <gas_used>\\n``
    Example: ``GAS TestV2V2V2 187849``
    """
    line = f"GAS {label} {gas_used}\n"
    with open(_GAS_RESULTS_PATH, "a") as f:
        f.write(line)
