import dataclasses
import typing

from degenbot.arbitrage.cmd_stream import (
    ZERO_ADDRESS as CMD_ZERO_ADDRESS,
)
from degenbot.arbitrage.cmd_stream import (
    AddressTable,
    enc_erc20_transfer,
    enc_preamble,
    enc_v2_swap_calc,
    enc_v2_swap_compact,
    enc_v2_swap_direct,
    enc_v3_swap_compact,
    enc_v4_settle,
    enc_v4_settle_all,
    enc_v4_settle_delta,
    enc_v4_swap_compact,
    enc_v4_swap_dynamic,
    enc_v4_sync,
    enc_v4_take,
    enc_v4_take_delta,
    enc_v4_unlock,
    enc_weth_deposit,
    enc_weth_withdraw,
)
from degenbot.arbitrage.encoding import fits_int128
from degenbot.logging import logger as bot_logger
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS


@dataclasses.dataclass(frozen=True)
class V2HopInfo:
    pool_key: int
    pool_address: str
    token0_address: str
    token1_address: str
    fee: int  # fee as fraction of 10000 (e.g. 30 for 0.3%)
    zfo: bool


@dataclasses.dataclass(frozen=True)
class V3HopInfo:
    pool_key: int
    pool_address: str
    token0_address: str
    token1_address: str
    fee: int
    zfo: bool


@dataclasses.dataclass(frozen=True)
class V4HopInfo:
    pool_key: int
    pool_manager_address: str
    pool_id_hex: str
    currency0_address: str
    currency1_address: str
    fee: int
    tick_spacing: int
    hook_address: str
    zfo: bool


HopInfo = V2HopInfo | V3HopInfo | V4HopInfo


@dataclasses.dataclass
class PathInfo:
    hops: list[HopInfo]

    @property
    def path_type(self) -> str:
        """Combined pool types: 'V3-V2', 'V3-V3', 'V2-V2', 'V4-V3', etc."""
        type_names = []
        for h in self.hops:
            if isinstance(h, V2HopInfo):
                type_names.append("V2")
            elif isinstance(h, V3HopInfo):
                type_names.append("V3")
            elif isinstance(h, V4HopInfo):
                type_names.append("V4")
        return "-".join(type_names)


# ──────────────────────────────────────────────────────────────────
# Payload encoding
# ──────────────────────────────────────────────────────────────────

# V4 swap parameters: (currency0, currency1, fee, tick_spacing, hooks, zero_for_one, amount_specified, sqrt_price_limit_x96, dynamic_amount)
V4SwapParam = tuple[str, str, int, int, str, bool, int, int, bool]


def encode_cmd_stream(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode an arbitrage path as a cmd_executor command stream.

    Produces a bytes payload for execute(commands) on the cmd_executor contract.
    Uses compact command encoding (V2_SWAP_COMPACT, V2_SWAP_CALC, etc.)
    with an address table for minimal calldata size.

    Returns None if encoding fails for this path type.
    """
    num_hops = len(path_info.hops)

    # Generalized N-hop V2 (2+ hops): flash borrow + chained V2_SWAP_CALC
    if all(isinstance(h, V2HopInfo) for h in path_info.hops):
        return _encode_cmd_v2_n_hop(path_info, optimal_input, hop_outputs, executor_address)

    # 2-hop paths
    if num_hops == 2:
        hop0, hop1 = path_info.hops[0], path_info.hops[1]
        # V4-hybrid paths
        if isinstance(hop0, V4HopInfo) and isinstance(hop1, V4HopInfo):
            return _encode_cmd_v4_v4(
                path_info,
                optimal_input,
                hop_outputs,
                executor_address,
                pool_manager_address,
                weth_address,
            )
        if isinstance(hop0, V4HopInfo) and isinstance(hop1, V3HopInfo):
            return _encode_cmd_v4_v3(
                path_info,
                optimal_input,
                hop_outputs,
                executor_address,
                pool_manager_address,
                weth_address,
            )
        if isinstance(hop0, V3HopInfo) and isinstance(hop1, V4HopInfo):
            return _encode_cmd_v3_v4(
                path_info,
                optimal_input,
                hop_outputs,
                executor_address,
                pool_manager_address,
                weth_address,
            )
        if isinstance(hop0, V4HopInfo) and isinstance(hop1, V2HopInfo):
            return _encode_cmd_v4_v2(
                path_info,
                optimal_input,
                hop_outputs,
                executor_address,
                pool_manager_address,
                weth_address,
            )
        if isinstance(hop0, V2HopInfo) and isinstance(hop1, V4HopInfo):
            return _encode_cmd_v2_v4(
                path_info,
                optimal_input,
                hop_outputs,
                executor_address,
                pool_manager_address,
                weth_address,
            )
        # V3/V2 mixed paths
        if isinstance(hop0, V3HopInfo) and isinstance(hop1, V3HopInfo):
            return _encode_cmd_v3_v3(
                path_info,
                optimal_input,
                hop_outputs,
                executor_address,
                pool_manager_address,
                weth_address,
            )
        if isinstance(hop0, V2HopInfo) and isinstance(hop1, V3HopInfo):
            return _encode_cmd_v2_v3(
                path_info, optimal_input, hop_outputs, executor_address, weth_address
            )
        if isinstance(hop0, V3HopInfo) and isinstance(hop1, V2HopInfo):
            return _encode_cmd_v3_v2(
                path_info, optimal_input, hop_outputs, executor_address, weth_address
            )

    # 3-hop paths (optimized patterns from ~/code/executor tests)
    if num_hops == 3:
        return _encode_cmd_3_hop(path_info, optimal_input, hop_outputs, executor_address, pool_manager_address, weth_address)

    # Unsupported path type for cmd_executor
    return None


def v4_input_is_native(hop: V4HopInfo) -> bool:
    """True if the V4 swap's input currency is native ETH (address(0))."""
    input_currency = hop.currency0_address if hop.zfo else hop.currency1_address
    return input_currency == NATIVE_CURRENCY_ADDRESS


def v4_output_is_native(hop: V4HopInfo) -> bool:
    """True if the V4 swap's output currency is native ETH (address(0))."""
    output_currency = hop.currency1_address if hop.zfo else hop.currency0_address
    return output_currency == NATIVE_CURRENCY_ADDRESS


def _encode_cmd_v2_n_hop(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
) -> bytes | None:
    """Encode N-hop V2 arbitrage as cmd_executor command stream (N >= 2).

    Pattern (approach 2 from test_cmd_executor_three_pool_v2.py):
      Flash borrow from pool A, V2_SWAP_CALC for pools B..N-1 with direct
      custody (recipient = next pool), V2_SWAP_CALC pool N → executor,
      then repay pool A flash borrow.

    Direct custody: V2 pairs send output optimistically to any recipient.
    When recipient is the next pool, the receiving pair accumulates excess
    balance. V2_SWAP_CALC reads that excess as swap input — no executor
    custody, no extra callback, no extra transfer.

    Inside pool A callback (forward_data):
      1. ERC20_TRANSFER forward token to pool B (creates excess balance)
      2. V2_SWAP_CALC pool B (excess → intermediate, recipient=pool C)
      ...
      N. V2_SWAP_CALC pool N (excess → WETH, recipient=executor)
      N+1. ERC20_TRANSFER WETH to pool A (flash repayment)

    Command stream budget (512 byte limit):
      Preamble: ~1 + N_addrs*21 + 2 bytes
      Callback: 35 + (N-1)*6 + 35 bytes
      Top-level: 22 + callback_len bytes
      For 4 hops: ~260 bytes, well within limit.
    """
    num_hops = len(path_info.hops)
    if num_hops < 2:
        return None

    # All hops must be V2 (caller should have already checked, but be safe)
    for hop in path_info.hops:
        if not isinstance(hop, V2HopInfo):
            return None

    if any(o <= 0 for o in hop_outputs):
        return None

    try:
        at = AddressTable()
        executor_idx = at.add(executor_address)

        # Register all pool addresses (preserves insertion order)
        pool_indices = [at.add(hop.pool_address) for h in path_info.hops]

        # Forward token from pool A (the intermediate sent to pool B)
        hop_a = path_info.hops[0]
        zfo_a = hop_a.zfo
        forward_addr = hop_a.token1_address if zfo_a else hop_a.token0_address
        forward_idx = at.add(forward_addr)

        # WETH repayment token — the input token to pool A (output of last pool)
        hop_last = path_info.hops[-1]
        weth_addr = hop_last.token1_address if hop_last.zfo else hop_last.token0_address
        weth_idx = at.add(weth_addr)

        # Build callback commands:
        # 1. Transfer forward token to pool B (creates excess balance)
        callback_cmds = enc_erc20_transfer(forward_idx, pool_indices[1], hop_outputs[0])

        # 2..N. V2_SWAP_CALC for each subsequent pool
        for i in range(1, num_hops):
            hop = path_info.hops[i]
            # Intermediate pools → next pool (direct custody); last pool → executor
            recipient_idx = pool_indices[i + 1] if i < num_hops - 1 else executor_idx
            callback_cmds += enc_v2_swap_calc(
                pool_idx=pool_indices[i],
                zfo=hop.zfo,
                recipient_idx=recipient_idx,
                fee=hop.fee,
            )

        # N+1. Flash repayment: WETH back to pool A
        callback_cmds += enc_erc20_transfer(weth_idx, pool_indices[0], optimal_input)

        # Top-level: V2_SWAP_COMPACT on pool A (flash borrow, sends output to executor)
        commands = enc_v2_swap_compact(
            pool_idx=pool_indices[0],
            zfo=zfo_a,
            amount_out=hop_outputs[0],
            recipient_idx=executor_idx,
            forward_data=callback_cmds,
        )

        return enc_preamble(at) + commands
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v2-n-hop] {type(e).__name__}: {e}")
        return None


def _encode_cmd_v4_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode V4-V4 2-hop arbitrage as cmd_executor command stream.

    Both swaps inside a single V4_UNLOCK. Strategy depends on whether
    the intermediate currencies match or require WETH↔ETH conversion:

    Case 1 — Same intermediate currency (WETH↔WETH or ETH↔ETH):
      Pool A: V4_SWAP_COMPACT (explicit amount)
      Pool B: V4_SWAP_DYNAMIC (reads amount from PM delta ledger)
      Settlement: V4_TAKE profit, V4_SETTLE

    Case 2 — Pool A outputs WETH, Pool B needs native ETH:
      Pool A: V4_SWAP_COMPACT → V4_TAKE(WETH to executor)
      WETH_WITHDRAW (unwrap WETH→ETH)
      Pool B: V4_SWAP_COMPACT (explicit ETH amount)
      V4_SETTLE_DELTA(native) + V4_TAKE_DELTA(profit)

    Case 3 — Pool A outputs native ETH, Pool B needs WETH:
      Pool A: V4_SWAP_COMPACT → V4_TAKE(ETH to executor)
      WETH_DEPOSIT (wrap ETH→WETH)
      Pool B: V4_SWAP_COMPACT (explicit WETH amount)
      V4_SETTLE_DELTA(WETH) + V4_TAKE_DELTA(profit)

    V4 sign convention: amountSpecified < 0 means exact-input.
    """
    hop_a = path_info.hops[0]
    hop_b = path_info.hops[1]
    if not isinstance(hop_a, V4HopInfo) or not isinstance(hop_b, V4HopInfo):
        return None

    try:
        forward_out = hop_outputs[0]
        weth_out = hop_outputs[1]
        if forward_out <= 0 or weth_out <= 0:
            return None

        # Int128 overflow guard (needed for all explicit amount swaps)
        if not fits_int128(optimal_input) or not fits_int128(forward_out):
            return None

        # Determine intermediate currencies
        mid_currency_a = hop_a.currency1_address if hop_a.zfo else hop_a.currency0_address
        mid_currency_b = hop_b.currency0_address if hop_b.zfo else hop_b.currency1_address
        input_currency_a = hop_a.currency0_address if hop_a.zfo else hop_a.currency1_address
        output_currency_b = hop_b.currency1_address if hop_b.zfo else hop_b.currency0_address

        a_outputs_native = mid_currency_a == NATIVE_CURRENCY_ADDRESS
        b_needs_native = mid_currency_b == NATIVE_CURRENCY_ADDRESS
        needs_wrap = a_outputs_native and not b_needs_native  # ETH → WETH_DEPOSIT → WETH
        needs_unwrap = not a_outputs_native and b_needs_native  # WETH → WETH_WITHDRAW → ETH
        currency_gap = needs_wrap or needs_unwrap

        at = AddressTable()
        _pm_idx = at.add(pool_manager_address)
        executor_idx = at.add(executor_address)
        zero_idx = at.add(CMD_ZERO_ADDRESS)

        c0_a_idx = at.add(hop_a.currency0_address)
        c1_a_idx = at.add(hop_a.currency1_address)
        c0_b_idx = at.add(hop_b.currency0_address)
        c1_b_idx = at.add(hop_b.currency1_address)
        weth_idx = at.add(weth_address)
        if (
            a_outputs_native
            or b_needs_native
            or input_currency_a == NATIVE_CURRENCY_ADDRESS
            or output_currency_b == NATIVE_CURRENCY_ADDRESS
        ):
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        # 1. V4_SWAP_COMPACT for pool A (always explicit amount)
        inner = enc_v4_swap_compact(
            c0_idx=c0_a_idx,
            c1_idx=c1_a_idx,
            fee=hop_a.fee,
            tick_spacing=hop_a.tick_spacing,
            hooks_idx=zero_idx,
            zfo=hop_a.zfo,
            amount_u128=optimal_input,
        )

        if currency_gap:
            # Take intermediate token out of PM, convert, then second swap with
            # explicit amount. Pattern verified in test_cmd_executor_v4v4_wrap_unwrap.py.
            # Intermediate token must leave PM because PM tracks WETH and native
            # ETH deltas separately — V4_SWAP_DYNAMIC would read the wrong delta type.

            # 2. Take intermediate token to executor
            if a_outputs_native:
                # Pool A output is ETH
                inner += enc_v4_take(native_idx, executor_idx, forward_out)
                # 3. Wrap ETH → WETH
                inner += enc_weth_deposit(forward_out)
            else:
                # Pool A output is WETH
                inner += enc_v4_take(weth_idx, executor_idx, forward_out)
                # 3. Unwrap WETH → ETH
                inner += enc_weth_withdraw(forward_out)

            # 4. V4_SWAP_COMPACT for pool B (explicit amount, not dynamic)
            inner += enc_v4_swap_compact(
                c0_idx=c0_b_idx,
                c1_idx=c1_b_idx,
                fee=hop_b.fee,
                tick_spacing=hop_b.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_b.zfo,
                amount_u128=forward_out,
            )

            # 5. Settle pool B's input currency
            if b_needs_native:
                # Pool B consumed ETH — settle the native ETH delta
                inner += enc_v4_settle_delta(native_idx)
            else:
                # Pool B consumed WETH — settle the WETH delta
                inner += enc_v4_settle_delta(weth_idx)

            # 6. Take profit (output currency of pool B via delta ledger)
            if output_currency_b == NATIVE_CURRENCY_ADDRESS:
                inner += enc_v4_take_delta(native_idx, executor_idx)
            elif output_currency_b == weth_address:
                inner += enc_v4_take_delta(weth_idx, executor_idx)
            else:
                # ERC-20 profit (e.g., USDC)
                inner += enc_v4_take_delta(c1_b_idx if hop_b.zfo else c0_b_idx, executor_idx)

            # 7. Auto-settle remaining deltas (handles rounding residuals)
            inner += enc_v4_settle_all()

        else:
            # Same intermediate currency — use V4_SWAP_DYNAMIC for pool B
            # (intermediate token stays in PM's delta ledger)

            # 2. V4_SWAP_DYNAMIC for pool B (reads amount from PM exttload)
            inner += enc_v4_swap_dynamic(
                c0_idx=c0_b_idx,
                c1_idx=c1_b_idx,
                fee=hop_b.fee,
                tick_spacing=hop_b.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_b.zfo,
            )

            # 3. Settlement: take net profit, auto-settle remaining deltas
            # Use V4_TAKE_DELTA (reads actual PM delta) + V4_SETTLE_ALL
            # (auto-settles all nonzero deltas). V4_TAKE with explicit amount
            # and V4_SETTLE (raw settle) fail because V4 swap math produces
            # rounding residuals that the explicit amounts don't cover.
            if output_currency_b == NATIVE_CURRENCY_ADDRESS:
                inner += enc_v4_take_delta(native_idx, executor_idx)
            elif output_currency_b == weth_address:
                inner += enc_v4_take_delta(weth_idx, executor_idx)
            else:
                # ERC-20 profit (shouldn't happen for ETH/WETH-denominated arb)
                profit_idx = c1_b_idx if hop_b.zfo else c0_b_idx
                inner += enc_v4_take_delta(profit_idx, executor_idx)

            # Auto-settle all remaining nonzero deltas (handles rounding residuals)
            inner += enc_v4_settle_all()

        commands = enc_v4_unlock(inner)
        return enc_preamble(at) + commands
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v4v4] {type(e).__name__}: {e}")
        return None


def _encode_cmd_v4_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode V4-V3 2-hop arbitrage as cmd_executor command stream.

    When V4 outputs native ETH, V3 needs WETH — insert WETH_DEPOSIT
    between V4_TAKE and V3 swap, then V3 uses auto-pay.
    Pattern from test_cmd_executor_inline_wrap_unwrap.py::TestV4ToV3InlineWrap.

    When V4 outputs ERC-20, V3 uses auto-pay. After V3 auto-pay,
    the executor holds WETH (from V3 output) which must be settled
    to PM. If V4's input is native ETH (not WETH), must unwrap
    WETH→ETH before settling (WETH≠ETH currency mismatch).
    Pattern from test_cmd_executor_v4v3.py::TestV4ToV3AutoPay.

    Flow (all inside V4_UNLOCK):
      Native ETH output:
        1. V4_SWAP_COMPACT (sell USDC, buy ETH)
        2. V4_TAKE(ETH→executor)
        3. WETH_DEPOSIT(forward_out)
        4. V3_SWAP_COMPACT (auto-pay, no forward_data)
        5. V4_SETTLE_DELTA(input_currency)
      ERC-20 output:
        1. V4_SWAP_COMPACT (sell ETH/WETH, buy ERC-20)
        2. V4_TAKE(ERC-20→executor)
        3. V3_SWAP_COMPACT (auto-pay)
        4. [if V4 input is native ETH] WETH_WITHDRAW + V4_SETTLE_DELTA(native_idx)
           [if V4 input is WETH]       V4_SETTLE_DELTA(weth_idx)
    """
    hop_v4 = path_info.hops[0]
    hop_v3 = path_info.hops[1]
    if not isinstance(hop_v4, V4HopInfo) or not isinstance(hop_v3, V3HopInfo):
        return None

    try:
        forward_out = hop_outputs[0]
        weth_out = hop_outputs[1]
        if forward_out <= 0 or weth_out <= 0:
            return None

        if not fits_int128(optimal_input):
            return None

        v4_output_is_native = v4_output_is_native(hop_v4)

        at = AddressTable()
        _pm_idx = at.add(pool_manager_address)
        executor_idx = at.add(executor_address)
        zero_idx = at.add(CMD_ZERO_ADDRESS)

        c0_v4_idx = at.add(hop_v4.currency0_address)
        c1_v4_idx = at.add(hop_v4.currency1_address)
        v3_idx = at.add(hop_v3.pool_address)
        weth_idx = at.add(weth_address)
        if v4_output_is_native:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        # 1. V4 swap (input→forward)
        inner = enc_v4_swap_compact(
            c0_idx=c0_v4_idx,
            c1_idx=c1_v4_idx,
            fee=hop_v4.fee,
            tick_spacing=hop_v4.tick_spacing,
            hooks_idx=zero_idx,
            zfo=hop_v4.zfo,
            amount_u128=optimal_input,
        )

        if v4_output_is_native:
            # V4 output is native ETH — take to executor, then wrap
            inner += enc_v4_take(native_idx, executor_idx, forward_out)
            inner += enc_weth_deposit(forward_out)
            # 3. V3 swap with auto-pay (executor has WETH after deposit)
            # Pattern: TestV4ToV3InlineWrap
            inner += enc_v3_swap_compact(
                v3_idx,
                hop_v3.zfo,
                forward_out,
                executor_idx,
            )
            # 4. Settle V4's input currency debt (e.g., USDC)
            # V3 sent USDC to executor; V4_SETTLE_DELTA reads delta and
            # syncs+transfers+settles automatically.
            input_idx = c0_v4_idx if hop_v4.zfo else c1_v4_idx
            inner += enc_v4_settle_delta(input_idx)
            # 5. Settle any rounding residuals (native ETH delta from V4_TAKE)
            inner += enc_v4_settle_all()
        else:
            # V4 output is ERC-20 — take to executor, V3 auto-pay
            # Pattern: TestV4ToV3AutoPay
            forward_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
            inner += enc_v4_take(forward_idx, executor_idx, forward_out)

            # 3. V3 swap with auto-pay — V3 sends WETH to executor,
            #    auto-pays the forward token (e.g., USDC).
            inner += enc_v3_swap_compact(
                v3_idx,
                hop_v3.zfo,
                forward_out,
                executor_idx,
            )

            # 4. Settle V4's input currency debt.
            # V3 auto-pay gave WETH to executor. If V4's input is native ETH,
            # must unwrap WETH→ETH before settling (WETH≠ETH mismatch).
            v4_input_is_native = v4_input_is_native(hop_v4)
            if v4_input_is_native:
                input_idx = c0_v4_idx if hop_v4.zfo else c1_v4_idx
                inner += enc_weth_withdraw(optimal_input)
                inner += enc_v4_settle_delta(input_idx)
            else:
                inner += enc_v4_settle_delta(weth_idx)
            # 5. Settle any rounding residuals (output currency delta from V4_TAKE)
            inner += enc_v4_settle_all()

        commands = enc_v4_unlock(inner)
        return enc_preamble(at) + commands
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v4v3] {type(e).__name__}: {e}")
        return None


def _encode_cmd_v3_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode V3-V4 2-hop arbitrage as cmd_executor command stream.

    V3 swap with forward_data callback. Inside the callback, V4 unlock
    runs the swap (providing WETH to the executor), then WETH is
    transferred from executor to V3 pool to pay V3's debt.

    The executor cannot auto-pay V3's WETH debt (it starts with 0 balance),
    so forward_data with V4 unlock is required to source the WETH.

    When V4 requires native ETH as input, V3's WETH output must be
    unwrapped via WETH_WITHDRAW before the V4 swap.

    Flow:
      V3_SWAP_COMPACT (WETH→forward, forward_data)
        V3 callback processes forward_data:
          [if V4 input is WETH]:
            V4_UNLOCK: sync+transfer+settle + swap + take_delta
            ERC20_TRANSFER WETH to V3 (pay V3's debt)
          [if V4 input is native ETH]:
            WETH_WITHDRAW (unwrap WETH→ETH)
            V4_UNLOCK: swap + settle_delta(ETH) + take
            (no USDC debt to V3 — V3 already sent WETH before callback)
    """
    hop_v3 = path_info.hops[0]
    hop_v4 = path_info.hops[1]
    if not isinstance(hop_v3, V3HopInfo) or not isinstance(hop_v4, V4HopInfo):
        return None

    try:
        forward_out = hop_outputs[0]
        weth_out = hop_outputs[1]
        if forward_out <= 0 or weth_out <= 0:
            return None

        if not fits_int128(forward_out) or not fits_int128(weth_out):
            return None

        v4_input_is_native = v4_input_is_native(hop_v4)

        at = AddressTable()
        pm_idx = at.add(pool_manager_address)
        executor_idx = at.add(executor_address)
        zero_idx = at.add(CMD_ZERO_ADDRESS)

        v3_idx = at.add(hop_v3.pool_address)
        c0_v4_idx = at.add(hop_v4.currency0_address)
        c1_v4_idx = at.add(hop_v4.currency1_address)
        weth_idx = at.add(weth_address)
        if v4_input_is_native:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        if v4_input_is_native:
            # V4 needs native ETH — unwrap WETH→ETH first, then V4 swap
            # Pattern from test_cmd_executor_inline_wrap_unwrap.py::TestV3ToV4InlineUnwrap
            v4_inner = enc_v4_swap_compact(
                c0_idx=c0_v4_idx,
                c1_idx=c1_v4_idx,
                fee=hop_v4.fee,
                tick_spacing=hop_v4.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_v4.zfo,
                amount_u128=forward_out,
            )
            # V4 settle native ETH (executor pays unwrapped ETH to PM)
            v4_inner += enc_v4_settle_delta(native_idx)
            # V4 take profit (WETH or USDC)
            output_currency = hop_v4.currency1_address if hop_v4.zfo else hop_v4.currency0_address
            if output_currency == NATIVE_CURRENCY_ADDRESS:
                v4_inner += enc_v4_take(native_idx, executor_idx, weth_out)
            else:
                # V4 output is ERC-20 — take normally
                output_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
                v4_inner += enc_v4_take(output_idx, executor_idx, weth_out)
            # Settle any rounding residuals inside V4 unlock
            v4_inner += enc_v4_settle_all()

            # V3 callback: unwrap WETH→ETH, V4 unlock, then pay V3's forward-token debt
            # V3 sent WETH before callback, but is owed the forward token (e.g., USDC).
            # V4 swap provides USDC as output, which we transfer to V3.
            v3_callback_cmds = enc_weth_withdraw(forward_out)
            v3_callback_cmds += enc_v4_unlock(v4_inner)
            # Pay V3's input debt with output from V4 swap
            input_currency_v3 = hop_v3.token0_address if hop_v3.zfo else hop_v3.token1_address
            if input_currency_v3 == weth_address or input_currency_v3 == NATIVE_CURRENCY_ADDRESS:
                # V3 is owed WETH — but V4 gave USDC, not WETH.
                # This shouldn't happen for WETH-denominated arb paths with native-ETH V4.
                # The V3 forward token should be USDC, not WETH.
                return None
            forward_v3_idx = at.add(input_currency_v3)
            v3_callback_cmds += enc_erc20_transfer(forward_v3_idx, v3_idx, optimal_input)
        else:
            # V4 needs WETH — standard sync+transfer+settle+swap+take
            # Forward token = output of V3 swap
            forward_addr = hop_v3.token1_address if hop_v3.zfo else hop_v3.token0_address
            forward_idx = at.add(forward_addr)

            v4_inner = enc_v4_sync(forward_idx)
            v4_inner += enc_erc20_transfer(forward_idx, pm_idx, forward_out)
            v4_inner += enc_v4_settle()
            v4_inner += enc_v4_swap_compact(
                c0_idx=c0_v4_idx,
                c1_idx=c1_v4_idx,
                fee=hop_v4.fee,
                tick_spacing=hop_v4.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_v4.zfo,
                amount_u128=forward_out,
            )
            # Take profit: use the V4 output currency (positive delta side)
            # zfo=True → output is currency1; zfo=False → output is currency0
            output_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
            v4_inner += enc_v4_take(output_idx, executor_idx, weth_out)
            # Settle any rounding residuals inside V4 unlock
            v4_inner += enc_v4_settle_all()

            # V3 callback: V4 unlock + pay V3's WETH debt
            v3_callback_cmds = enc_v4_unlock(v4_inner)
            v3_callback_cmds += enc_erc20_transfer(weth_idx, v3_idx, optimal_input)

        # Top-level: V3 swap with forward_data callback
        commands = enc_v3_swap_compact(
            v3_idx,
            hop_v3.zfo,
            optimal_input,
            executor_idx,
            forward_data=v3_callback_cmds,
        )

        return enc_preamble(at) + commands
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v3v4] {type(e).__name__}: {e}")
        return None


def _encode_cmd_v4_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode V4-V2 2-hop arbitrage as cmd_executor command stream.

    V4 swap inside V4_UNLOCK, take forward token to V2 pool
    (direct custody), V2_SWAP_CALC reads excess balance and outputs
    to executor, then settle V4's debt.

    When V4 outputs native ETH, V2 needs WETH — insert WETH_DEPOSIT
    between V4_TAKE and V2 swap to bridge the representation gap.
    In this case the V4 debt is in the input currency (e.g., USDC),
    not WETH, so we settle that instead.

    Flow (all inside V4_UNLOCK):
      [native-ETH output]:
        1. V4_SWAP_COMPACT (sell USDC, buy ETH)
        2. V4_TAKE(ETH → executor)
        3. WETH_DEPOSIT (wrap ETH → WETH)
        4. V2_SWAP_COMPACT(callback=ERC20_TRANSFER(WETH→V2))
           V2 flash-sends USDC to executor, then callback pays WETH to V2.
        5. V4_SETTLE_DELTA(USDC)

      [ERC-20 output — direct custody]:
        1. V4_SWAP_COMPACT (sell WETH, buy USDC)
        2. V4_TAKE(USDC → V2 pair, direct custody)
        3. V2_SWAP_CALC (excess USDC → WETH, recipient=executor)
        4. V4_SETTLE (sync + transfer WETH to PM + settle)
    """
    hop_v4 = path_info.hops[0]
    hop_v2 = path_info.hops[1]
    if not isinstance(hop_v4, V4HopInfo) or not isinstance(hop_v2, V2HopInfo):
        return None

    try:
        forward_out = hop_outputs[0]
        weth_out = hop_outputs[1]
        if forward_out <= 0 or weth_out <= 0:
            return None

        if not fits_int128(optimal_input) or not fits_int128(forward_out):
            return None

        v4_output_is_native = v4_output_is_native(hop_v4)

        at = AddressTable()
        pm_idx = at.add(pool_manager_address)
        executor_idx = at.add(executor_address)
        zero_idx = at.add(CMD_ZERO_ADDRESS)

        c0_v4_idx = at.add(hop_v4.currency0_address)
        c1_v4_idx = at.add(hop_v4.currency1_address)
        v2_idx = at.add(hop_v2.pool_address)
        weth_idx = at.add(weth_address)
        if v4_output_is_native:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        # 1. V4 swap (input→forward)
        inner = enc_v4_swap_compact(
            c0_idx=c0_v4_idx,
            c1_idx=c1_v4_idx,
            fee=hop_v4.fee,
            tick_spacing=hop_v4.tick_spacing,
            hooks_idx=zero_idx,
            zfo=hop_v4.zfo,
            amount_u128=optimal_input,
        )

        if v4_output_is_native:
            # V4 output is native ETH, V2 needs WETH
            # Pattern from test_cmd_executor_inline_wrap_unwrap.py::TestV4ToV2InlineWrap
            # 2. Take ETH to executor, wrap to WETH
            inner += enc_v4_take(native_idx, executor_idx, forward_out)
            inner += enc_weth_deposit(forward_out)
            # 3. V2_SWAP_COMPACT with callback — V2 sends USDC to executor,
            #    then callback pays WETH to V2 from just-wrapped balance.
            #    (Cannot use V2_SWAP_CALC because on-chain output ≠ solver amount)
            v2_callback_cmds = enc_erc20_transfer(weth_idx, v2_idx, forward_out)
            inner += enc_v2_swap_compact(
                pool_idx=v2_idx,
                zfo=hop_v2.zfo,
                amount_out=weth_out,
                recipient_idx=executor_idx,
                forward_data=v2_callback_cmds,
            )
            # 4. Settle V4's input-currency debt (e.g., USDC)
            # V2 sent that currency to executor in step 3.
            # V4_SETTLE_DELTA reads the negative delta from PM exttload,
            # syncs PM's balance, transfers from executor to PM, and settles.
            input_idx = c0_v4_idx if hop_v4.zfo else c1_v4_idx
            inner += enc_v4_settle_delta(input_idx)
            # 5. Settle any rounding residuals inside V4 unlock
            inner += enc_v4_settle_all()
        else:
            # V4 output is ERC-20 — take directly to V2 pool (direct custody)
            forward_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
            inner += enc_v4_take(forward_idx, v2_idx, forward_out)

            # V2_SWAP_CALC reads excess balance in V2 pool, no callback needed.
            # V4_TAKE sent the forward token directly to V2 (direct custody),
            # creating excess balance that V2_SWAP_CALC consumes.
            inner += enc_v2_swap_calc(v2_idx, hop_v2.zfo, executor_idx, fee=hop_v2.fee)

            # Settle V4's input-currency debt.
            # V2 sent the output (WETH or USDC) to the executor.
            v4_input_is_native = v4_input_is_native(hop_v4)
            if v4_input_is_native:
                # V4's input is native ETH. The executor got WETH from V2.
                # Must unwrap WETH→ETH before settling the native ETH delta.
                # Pattern: WETH_WITHDRAW → V4_SETTLE_DELTA(native_idx)
                input_idx = c0_v4_idx if hop_v4.zfo else c1_v4_idx
                inner += enc_weth_withdraw(optimal_input)
                inner += enc_v4_settle_delta(input_idx)
            else:
                # V4's input is WETH. V2 sent WETH to executor.
                # Sync + transfer WETH to PM + settle.
                inner += enc_v4_sync(weth_idx)
                inner += enc_erc20_transfer(weth_idx, pm_idx, optimal_input)
                inner += enc_v4_settle()
            # Settle any rounding residuals inside V4 unlock
            inner += enc_v4_settle_all()

        commands = enc_v4_unlock(inner)
        return enc_preamble(at) + commands
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v4v2] {type(e).__name__}: {e}")
        return None


def _encode_cmd_v2_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode V2-V4 2-hop arbitrage as cmd_executor command stream.

    V2 flash borrow runs first, then V4_UNLOCK wraps sync+transfer+settle+swap+take.

    When V4 requires native ETH as input, V2's WETH output must be
    unwrapped via WETH_WITHDRAW before the V4 swap.

    Flow:
      [V4 input is native ETH]:
        V2_SWAP_COMPACT (flash, callback)
          V2 callback: WETH_WITHDRAW → V4_UNLOCK(swap + settle_delta(ETH) + take(output))
          → ERC20_TRANSFER(forward_token→V2) — pay V2's flash debt
      [V4 input is WETH/ERC-20]:
        V2_SWAP_COMPACT (flash, callback)
          V2 callback: ERC20_TRANSFER(WETH→V2) — pay V2's flash debt
        V4_UNLOCK: sync + transfer + settle + swap + take(output)
    """
    hop_v2 = path_info.hops[0]
    hop_v4 = path_info.hops[1]
    if not isinstance(hop_v2, V2HopInfo) or not isinstance(hop_v4, V4HopInfo):
        return None

    try:
        forward_out = hop_outputs[0]
        weth_out = hop_outputs[1]
        if forward_out <= 0 or weth_out <= 0:
            return None

        if not fits_int128(forward_out) or not fits_int128(weth_out):
            return None

        v4_input_is_native = v4_input_is_native(hop_v4)

        at = AddressTable()
        pm_idx = at.add(pool_manager_address)
        executor_idx = at.add(executor_address)
        zero_idx = at.add(CMD_ZERO_ADDRESS)

        v2_idx = at.add(hop_v2.pool_address)
        c0_v4_idx = at.add(hop_v4.currency0_address)
        c1_v4_idx = at.add(hop_v4.currency1_address)
        weth_idx = at.add(weth_address)
        if v4_input_is_native:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        # Forward token = output of V2 swap
        forward_addr = hop_v2.token1_address if hop_v2.zfo else hop_v2.token0_address
        forward_idx = at.add(forward_addr)

        if v4_input_is_native:
            # V4 needs native ETH — unwrap WETH first, then V4 swap + settle + take
            # Pattern from test_cmd_executor_inline_wrap_unwrap.py::TestV2ToV4InlineUnwrap
            v4_inner = enc_v4_swap_compact(
                c0_idx=c0_v4_idx,
                c1_idx=c1_v4_idx,
                fee=hop_v4.fee,
                tick_spacing=hop_v4.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_v4.zfo,
                amount_u128=forward_out,
            )
            # V4 settle native ETH (executor pays unwrapped ETH to PM)
            v4_inner += enc_v4_settle_delta(native_idx)
            # V4 take profit — use the V4 output currency index
            output_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
            v4_inner += enc_v4_take(output_idx, executor_idx, weth_out)
            # Settle any rounding residuals inside V4 unlock
            v4_inner += enc_v4_settle_all()

            # V2 callback: unwrap WETH→ETH, V4 unlock, pay V2's forward-token debt
            # V2 sent the forward token to executor before callback.
            # V4 swap provides more forward token — pay V2's debt with forward token.
            callback_cmds = enc_weth_withdraw(forward_out)
            callback_cmds += enc_v4_unlock(v4_inner)
            callback_cmds += enc_erc20_transfer(forward_idx, v2_idx, optimal_input)
        else:
            # V4 input is ERC-20 (not native ETH)
            # Check if V4's OUTPUT is native ETH — need WETH_DEPOSIT to wrap
            v4_output_is_native = v4_output_is_native(hop_v4)

            v4_inner = enc_v4_sync(forward_idx)
            v4_inner += enc_erc20_transfer(forward_idx, pm_idx, forward_out)
            v4_inner += enc_v4_settle()
            v4_inner += enc_v4_swap_compact(
                c0_idx=c0_v4_idx,
                c1_idx=c1_v4_idx,
                fee=hop_v4.fee,
                tick_spacing=hop_v4.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_v4.zfo,
                amount_u128=forward_out,
            )
            # Take profit: if V4 outputs native ETH, add native_idx and use it
            if v4_output_is_native:
                native_idx = at.add(NATIVE_CURRENCY_ADDRESS)
                v4_inner += enc_v4_take(native_idx, executor_idx, weth_out)
            else:
                output_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
                v4_inner += enc_v4_take(output_idx, executor_idx, weth_out)
            # Settle any rounding residuals inside V4 unlock
            v4_inner += enc_v4_settle_all()

            # V2 callback: V4 unlock first (executor gets V4 output), then pay V2
            callback_cmds = enc_v4_unlock(v4_inner)
            if v4_output_is_native:
                # V4 gave native ETH — wrap to WETH before paying V2
                callback_cmds += enc_weth_deposit(weth_out)
            callback_cmds += enc_erc20_transfer(weth_idx, v2_idx, optimal_input)

        # Top-level: V2 flash swap, then V4 unlock (or V4 already in callback)
        outer = enc_v2_swap_compact(
            pool_idx=v2_idx,
            zfo=hop_v2.zfo,
            amount_out=forward_out,
            recipient_idx=executor_idx,
            forward_data=callback_cmds,
        )

        return enc_preamble(at) + outer
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v2v4] {type(e).__name__}: {e}")
        return None


def _encode_cmd_v3_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode V3-V3 2-hop arbitrage as cmd_executor command stream.

    Forward-order with explicit WETH payment — matches the verified pattern
    from test_cmd_executor_v2v2_v3v3.py::TestV3V3.

    V3 sends output BEFORE invoking the callback (same as V2), so the
    executor has the forward token during V3a's callback. V3b uses
    auto-pay (empty forward_data) which transfers the owed forward
    token from executor to V3b.

    Flow:
      V3_A.swap(WETH→USDC, recipient=executor, forward_data)
        V3_A sends USDC to executor (before callback)
        V3_A callback (forward_data):
          ERC20_TRANSFER WETH to V3_A (pay V3_A's debt from executor reserve)
          V3_B.swap(USDC→WETH, auto-pay, recipient=executor)
            V3_B sends WETH to executor (before callback)
            V3_B callback (auto-pay): executor pays USDC to V3_B

    The executor needs reserve WETH to pay V3_A explicitly (can't use
    auto-pay for the outer V3 because its forward_data is non-empty).
    V3_A's output (USDC) covers V3_B's auto-pay USDC requirement.
    """
    hop_a = path_info.hops[0]
    hop_b = path_info.hops[1]
    if not isinstance(hop_a, V3HopInfo) or not isinstance(hop_b, V3HopInfo):
        return None

    try:
        forward_out = hop_outputs[0]
        weth_out = hop_outputs[1]
        if forward_out <= 0 or weth_out <= 0:
            return None

        at = AddressTable()
        executor_idx = at.add(executor_address)
        v3_a_idx = at.add(hop_a.pool_address)
        v3_b_idx = at.add(hop_b.pool_address)
        weth_idx = at.add(weth_address)

        # V3_A callback: pay WETH to V3_A, then V3_B swap (auto-pay)
        # Pattern from test_cmd_executor_v2v2_v3v3.py::TestV3V3
        v3_a_callback = enc_erc20_transfer(weth_idx, v3_a_idx, optimal_input)
        v3_a_callback += enc_v3_swap_compact(
            v3_b_idx,
            hop_b.zfo,
            forward_out,  # V3_B's exact-input amount (USDC in)
            executor_idx,  # V3_B sends WETH to executor
        )

        # Top-level: V3_A swap (forward-order)
        # V3_A sends USDC to executor, then callback pays WETH + runs V3_B.
        commands = enc_v3_swap_compact(
            v3_a_idx,
            hop_a.zfo,
            optimal_input,  # V3_A's exact-input amount (WETH in)
            executor_idx,
            forward_data=v3_a_callback,
        )

        return enc_preamble(at) + commands
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v3v3] {type(e).__name__}: {e}")
        return None


def _encode_cmd_v2_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode V2-V3 2-hop arbitrage as cmd_executor command stream.

    V2 flash borrow sends forward to executor. In the callback:
      1. Transfer forward token to V3 pool (so V3 has input)
      2. V3 swap with auto-pay (forward→WETH, no callback needed)
      3. Transfer WETH to V2 (flash repayment)

    Flow:
      V2_SWAP_COMPACT (WETH→forward, flash borrow, recipient=executor)
        callback (forward_data):
          ERC20_TRANSFER forward to V3 pool
          V3_SWAP_COMPACT (forward→WETH, auto-pay, recipient=executor)
          ERC20_TRANSFER WETH to V2 (flash repayment)
    """
    hop_a = path_info.hops[0]
    hop_b = path_info.hops[1]
    if not isinstance(hop_a, V2HopInfo) or not isinstance(hop_b, V3HopInfo):
        return None

    try:
        forward_out = hop_outputs[0]
        weth_out = hop_outputs[1]
        if forward_out <= 0 or weth_out <= 0:
            return None

        at = AddressTable()
        executor_idx = at.add(executor_address)
        v2_idx = at.add(hop_a.pool_address)
        v3_idx = at.add(hop_b.pool_address)
        weth_idx = at.add(weth_address)

        # Forward token from V2
        forward_addr = hop_a.token1_address if hop_a.zfo else hop_a.token0_address
        forward_idx = at.add(forward_addr)

        # V2 callback: V3 swap with forward_data (pay USDC to V3 during V3's callback
        # to satisfy IIA), then repay WETH to V2.
        # CRITICAL: The ERC20_TRANSFER must be inside V3's forward_data, NOT before
        # V3.swap(). If the transfer runs before V3.swap(), the USDC is in V3's
        # balance_before and IIA fails (no new USDC arrives during callback).
        v3_callback_cmds = enc_erc20_transfer(forward_idx, v3_idx, forward_out)
        callback_cmds = enc_v3_swap_compact(
            v3_idx,
            hop_b.zfo,
            forward_out,
            executor_idx,
            forward_data=v3_callback_cmds,
        )
        callback_cmds += enc_erc20_transfer(weth_idx, v2_idx, optimal_input)

        # Top-level: V2 flash swap
        commands = enc_v2_swap_compact(
            pool_idx=v2_idx,
            zfo=hop_a.zfo,
            amount_out=forward_out,
            recipient_idx=executor_idx,
            forward_data=callback_cmds,
        )

        return enc_preamble(at) + commands
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v2v3] {type(e).__name__}: {e}")
        return None


def _encode_cmd_v3_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    weth_address: str,
) -> bytes | None:
    """Encode V3-V2 2-hop arbitrage as cmd_executor command stream.

    V3 swap with forward_data callback, matching the verified pattern from
    test_cmd_executor_v2v3.py::TestV3ToV2.

    V3 sends output BEFORE callback (same as V2). So during V3's callback,
    the executor has V3's USDC output. The callback uses this USDC to:
    1. Pay WETH to V3 (from executor's reserve — V2 hasn't paid yet)
    2. Transfer USDC to V2 pair (pre-fund V2 for direct swap)
    3. V2 swap with V2_SWAP_COMPACT (no flash — USDC already in pair)

    Using V2 direct swap (no flash) avoids the WETH-amount mismatch that
    causes TF failures: V2 computes WETH output on-chain from actual
    reserves, instead of using the solver's predicted weth_out.

    Flow:
      V3_SWAP_COMPACT (WETH→USDC, recipient=executor, forward_data)
        V3 sends USDC to executor (before callback)
        V3 callback (forward_data):
          ERC20_TRANSFER WETH to V3 (pay V3's debt from executor reserve)
          ERC20_TRANSFER USDC to V2 pair (pre-fund for direct swap)
          V2_SWAP_COMPACT (USDC→WETH, no callback, WETH to executor)

    Note: V3 amountSpecified must be optimal_input (WETH input to V3),
    NOT forward_out (V3 output).
    """
    hop_a = path_info.hops[0]
    hop_b = path_info.hops[1]
    if not isinstance(hop_a, V3HopInfo) or not isinstance(hop_b, V2HopInfo):
        return None

    try:
        forward_out = hop_outputs[0]
        weth_out = hop_outputs[1]
        if forward_out <= 0 or weth_out <= 0:
            return None

        at = AddressTable()
        executor_idx = at.add(executor_address)
        v3_idx = at.add(hop_a.pool_address)
        v2_idx = at.add(hop_b.pool_address)
        weth_idx = at.add(weth_address)

        # Forward token from V3
        forward_addr = hop_a.token1_address if hop_a.zfo else hop_a.token0_address
        forward_idx = at.add(forward_addr)

        # V3 callback forward_data:
        # 1. Pay WETH to V3 (from executor's reserve WETH)
        # 2. Transfer USDC to V2 pair (pre-fund for direct swap)
        # 3. V2 swap (no flash/callback — USDC already in pair)
        #    V2 computes WETH output on-chain from actual reserves.
        v3_callback_cmds = enc_erc20_transfer(weth_idx, v3_idx, optimal_input)
        v3_callback_cmds += enc_erc20_transfer(forward_idx, v2_idx, forward_out)
        v3_callback_cmds += enc_v2_swap_compact(
            v2_idx,
            hop_b.zfo,
            weth_out,
            executor_idx,
        )

        # Top-level: V3 swap with forward_data (V3 amount = optimal_input = WETH in)
        commands = enc_v3_swap_compact(
            v3_idx,
            hop_a.zfo,
            optimal_input,
            executor_idx,
            forward_data=v3_callback_cmds,
        )

        return enc_preamble(at) + commands
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-v3v2] {type(e).__name__}: {e}")
        return None


# ═══════════════════════════════════════════════════════════════════════════
# 3-hop optimized encoders
# Reference: ~/code/executor/tests/test_cmd_executor_three_hop_optimized.py
# All 27 permutations with minimum-transfer routing
# ═══════════════════════════════════════════════════════════════════════════


def _encode_cmd_3_hop(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Dispatch 3-hop paths to the correct optimized encoder."""
    hop_types = tuple(
        "V2" if isinstance(h, V2HopInfo) else "V3" if isinstance(h, V3HopInfo) else "V4"
        for h in path_info.hops
    )
    encoder = _3HOP_ENCODERS.get(hop_types)
    if encoder is None:
        return None
    try:
        return encoder(path_info, optimal_input, hop_outputs, executor_address, pool_manager_address, weth_address)
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-3hop] {hop_types}: {type(e).__name__}: {e}")
        return None


def _enc_v4_swap(hop: V4HopInfo, at: AddressTable) -> bytes:
    """Encode V4_SWAP_COMPACT for a V4HopInfo, adding addresses to table."""
    c0_idx = at.add(hop.currency0_address)
    c1_idx = at.add(hop.currency1_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    return enc_v4_swap_compact(
        c0_idx=c0_idx,
        c1_idx=c1_idx,
        fee=hop.fee,
        tick_spacing=hop.tick_spacing,
        hooks_idx=zero_idx,
        zfo=hop.zfo,
        amount_u128=0,  # will be overridden by caller
    )


# ── V2-V2-V2 ──────────────────────────────────────────────────────────────


def _3hop_v2_v2_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Reverse-order flash borrow: V2c first, V2a→V2b via V2_SWAP_DIRECT."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    v2a_idx = at.add(ha.pool_address)
    v2b_idx = at.add(hb.pool_address)
    v2c_idx = at.add(hc.pool_address)

    # Inside V2c callback: WETH→V2a (creates excess), V2a→V2b, V2b→V2c
    c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    c_fwd += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v2b_idx)
    c_fwd += enc_v2_swap_direct(v2b_idx, hb.zfo, out_b, v2c_idx)

    # Flash borrow from V2c
    commands = enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V2-V2-V3 ──────────────────────────────────────────────────────────────


def _3hop_v2_v2_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Reverse-order: V3c fires first, V2a→V2b direct inside V3c callback."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    v2a_idx = at.add(ha.pool_address)
    v2b_idx = at.add(hb.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # Inside V3c callback: WETH→V2a, V2a→V2b, V2b→V3c
    c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    c_fwd += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v2b_idx)
    c_fwd += enc_v2_swap_direct(v2b_idx, hb.zfo, out_b, v3c_idx)

    # V3c fires first (reverse order)
    commands = enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V2-V2-V4 ──────────────────────────────────────────────────────────────


def _3hop_v2_v2_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V4_TAKE→V2a direct, V2b→PM delta netting, all inside V4 unlock."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    _executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v2a_idx = at.add(ha.pool_address)
    v2b_idx = at.add(hb.pool_address)
    c0_idx = at.add(hc.currency0_address)
    c1_idx = at.add(hc.currency1_address)

    # Forward token from V2b (output of V2b swap)
    forward_b_addr = hb.token1_address if hb.zfo else hb.token0_address
    forward_b_idx = at.add(forward_b_addr)

    inner = enc_v4_swap_compact(
        c0_idx, c1_idx, hc.fee, hc.tick_spacing, zero_idx, hc.zfo, optimal_input
    )
    inner += enc_v4_take(weth_idx, v2a_idx, optimal_input)
    inner += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v2b_idx)
    inner += enc_v4_sync(forward_b_idx)
    inner += enc_v2_swap_direct(v2b_idx, hb.zfo, out_b, pm_idx)
    inner += enc_v4_settle()
    inner += enc_v4_settle_delta(weth_idx)

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V2-V3-V2 ──────────────────────────────────────────────────────────────


def _3hop_v2_v3_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Reverse-order from V2c: V3b(to=V2c), V2a→V3b during V3b callback (IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, _out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    _usdc_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    executor_idx = at.add(executor_address)
    v2a_idx = at.add(ha.pool_address)
    v2c_idx = at.add(hc.pool_address)
    v3b_idx = at.add(hb.pool_address)

    # V3b callback: WETH→V2a (excess) + V2a→V3b (IIA ✓)
    b_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    b_fwd += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx)

    # V2c callback: V3b swap (to=V2c)
    c_fwd = enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, v2c_idx, forward_data=b_fwd)

    commands = enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V2-V3-V3 ──────────────────────────────────────────────────────────────


def _3hop_v2_v3_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3c outermost, V2a inside V3b callback (to=V3b, IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    v2a_idx = at.add(ha.pool_address)
    v3b_idx = at.add(hb.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # V3b callback: WETH→V2a (excess) + V2a→V3b
    v3b_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    v3b_fwd += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx)

    # V3c callback: V3b swap (to=V3c)
    v3c_fwd = enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, v3c_idx, forward_data=v3b_fwd)

    commands = enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, forward_data=v3c_fwd)
    return enc_preamble(at) + commands


# ── V2-V3-V4 ──────────────────────────────────────────────────────────────


def _3hop_v2_v3_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3b outermost, V2a inside V3b callback, V3b→PM + V4 unlock."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v2a_idx = at.add(ha.pool_address)
    v3b_idx = at.add(hb.pool_address)
    c0_idx = at.add(hc.currency0_address)
    c1_idx = at.add(hc.currency1_address)

    # Forward token from V3b (output of V3b swap)
    forward_b_addr = hb.token1_address if hb.zfo else hb.token0_address
    forward_b_idx = at.add(forward_b_addr)

    # V3b callback: V4 unlock (provides WETH to V2a) + V2a→V3b
    v4_inner = enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        c0_idx, c1_idx, hc.fee, hc.tick_spacing, zero_idx, hc.zfo, out_b
    )
    v4_inner += enc_v4_take(weth_idx, v2a_idx, optimal_input)
    v4_inner += enc_v4_take(weth_idx, executor_idx, out_c - optimal_input)
    v4_inner += enc_v4_sync(weth_idx)
    v4_inner += enc_v4_settle()

    b_fwd = enc_v4_unlock(v4_inner)
    b_fwd += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx)

    commands = enc_v4_sync(forward_b_idx)
    commands += enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, pm_idx, forward_data=b_fwd)
    return enc_preamble(at) + commands


# ── V2-V4-V2 ──────────────────────────────────────────────────────────────


def _3hop_v2_v4_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Reverse-order from V2c, V2a→PM delta netting."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    forward_b_idx = at.add(
        hb.currency0_address if hb.currency0_address != weth_address else hb.currency1_address
    )
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v2a_idx = at.add(ha.pool_address)
    v2c_idx = at.add(hc.pool_address)

    # V4 unlock: sync/settle from V2a→PM, V4b swap, V4_TAKE→V2c
    v4_inner = enc_v4_sync(forward_a_idx)
    v4_inner += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, pm_idx)
    v4_inner += enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    v4_inner += enc_v4_take(forward_b_idx, v2c_idx, out_b)

    # V2c fires first (reverse-order). Callback: WETH→V2a, V4 unlock
    c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    c_fwd += enc_v4_unlock(v4_inner)

    commands = enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V2-V4-V3 ──────────────────────────────────────────────────────────────


def _3hop_v2_v4_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3c reverse, V2a→PM inside unlock, V4_TAKE forward→V3c."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    forward_b_idx = at.add(
        hb.currency0_address if hb.currency0_address != weth_address else hb.currency1_address
    )
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v2a_idx = at.add(ha.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # V4 unlock: sync, V2a→PM, settle, V4b swap, V4_TAKE forward→V3c
    v4_inner = enc_v4_sync(forward_a_idx)
    v4_inner += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, pm_idx)
    v4_inner += enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    v4_inner += enc_v4_take(forward_b_idx, v3c_idx, out_b)

    # V3c fires first. Callback: WETH→V2a, V4 unlock
    c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    c_fwd += enc_v4_unlock(v4_inner)

    commands = enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V2-V4-V4 ──────────────────────────────────────────────────────────────


def _3hop_v2_v4_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V4_TAKE WETH→V2a (excess), V2a→PM, delta netting, V4_TAKE profit."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)

    v4_inner = enc_v4_sync(forward_a_idx)
    v4_inner += enc_v4_take(weth_idx, at.add(ha.pool_address), optimal_input)
    v4_inner += enc_v2_swap_direct(at.add(ha.pool_address), ha.zfo, out_a, pm_idx)
    v4_inner += enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    v4_inner += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    v4_inner += enc_v4_take(weth_idx, executor_idx, out_c - optimal_input)

    return enc_preamble(at) + enc_v4_unlock(v4_inner)


# ── V3-V2-V2 ──────────────────────────────────────────────────────────────


def _3hop_v3_v2_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3a outermost: sends USDC→V2b, V2b→V2c direct + WETH→V3a."""
    ha, hb, hc = path_info.hops
    _out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    v2b_idx = at.add(hb.pool_address)
    v2c_idx = at.add(hc.pool_address)
    v3a_idx = at.add(ha.pool_address)

    # V3a callback: V2b→V2c + V2c→exec + WETH→V3a
    a_fwd = enc_v2_swap_direct(v2b_idx, hb.zfo, out_b, v2c_idx)
    a_fwd += enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, executor_idx)
    a_fwd += enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)

    commands = enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v2b_idx, forward_data=a_fwd)
    return enc_preamble(at) + commands


# ── V3-V2-V3 ──────────────────────────────────────────────────────────────


def _3hop_v3_v2_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Reverse-order from V3c: V3a→V2b, V2b→V3c direct + explicit WETH→V3a."""
    ha, hb, hc = path_info.hops
    _out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    v2b_idx = at.add(hb.pool_address)
    v3a_idx = at.add(ha.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # V3a callback: V2b→V3c + WETH→V3a
    v3a_fwd = enc_v2_swap_direct(v2b_idx, hb.zfo, out_b, v3c_idx)
    v3a_fwd += enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)

    # V3c callback: V3a swap (recipient=V2b, direct custody)
    v3c_fwd = enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v2b_idx, forward_data=v3a_fwd)

    commands = enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, forward_data=v3c_fwd)
    return enc_preamble(at) + commands


# ── V3-V2-V4 ──────────────────────────────────────────────────────────────


def _3hop_v3_v2_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3a→V2b direct, V2b→PM, V4_TAKE→V3a directly (IIA ✓)."""
    ha, hb, hc = path_info.hops
    _out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3a_idx = at.add(ha.pool_address)
    v2b_idx = at.add(hb.pool_address)

    # Forward from V2b
    forward_b_addr = hb.token1_address if hb.zfo else hb.token0_address
    forward_b_idx = at.add(forward_b_addr)

    # V4 unlock: sync WBTC, V2b→PM, settle, V4c swap, V4_TAKE WETH→V3a
    v4_inner = enc_v4_sync(forward_b_idx)
    v4_inner += enc_v2_swap_direct(v2b_idx, hb.zfo, out_b, pm_idx)
    v4_inner += enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    v4_inner += enc_v4_take(weth_idx, v3a_idx, optimal_input)
    v4_inner += enc_v4_take(weth_idx, executor_idx, out_c - optimal_input)

    a_fwd = enc_v4_unlock(v4_inner)

    commands = enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v2b_idx, forward_data=a_fwd)
    return enc_preamble(at) + commands


# ── V3-V3-V2 ──────────────────────────────────────────────────────────────


def _3hop_v3_v3_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Reverse-order from V2c: V3a→V3b direct, V2c V2_SWAP_DIRECT + WETH→V3a."""
    ha, hb, hc = path_info.hops
    out_a, _out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    v2c_idx = at.add(hc.pool_address)
    v3a_idx = at.add(ha.pool_address)

    # V3a callback: V2c V2_SWAP_DIRECT + WETH→V3a
    v3a_fwd = enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, executor_idx)
    v3a_fwd += enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)

    # V3b callback: V3a swap (recipient=V3b)
    v3b_fwd = enc_v3_swap_compact(
        v3a_idx, ha.zfo, optimal_input, at.add(hb.pool_address), forward_data=v3a_fwd
    )

    # V3b fires first — sends output to V2c (creates excess)
    commands = enc_v3_swap_compact(
        at.add(hb.pool_address), hb.zfo, out_a, v2c_idx, forward_data=v3b_fwd
    )
    return enc_preamble(at) + commands


# ── V3-V3-V3 ──────────────────────────────────────────────────────────────


def _3hop_v3_v3_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Reverse-order direct custody, all auto-pay."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable()
    executor_idx = at.add(executor_address)
    v3a_idx = at.add(ha.pool_address)
    v3b_idx = at.add(hb.pool_address)
    v3c_idx = at.add(hc.pool_address)

    v3a_callback = b""
    v3b_callback = enc_v3_swap_compact(
        v3a_idx, ha.zfo, optimal_input, v3b_idx, forward_data=v3a_callback
    )
    v3c_callback = enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, v3c_idx, forward_data=v3b_callback)

    commands = enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, forward_data=v3c_callback)
    return enc_preamble(at) + commands


# ── V3-V3-V4 ──────────────────────────────────────────────────────────────


def _3hop_v3_v3_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3b→PM, V4_TAKE→V3a directly (IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3a_idx = at.add(ha.pool_address)
    v3b_idx = at.add(hb.pool_address)

    # Forward from V3b
    forward_b_addr = hb.token1_address if hb.zfo else hb.token0_address
    forward_b_idx = at.add(forward_b_addr)

    v4_inner = enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    v4_inner += enc_v4_take(weth_idx, v3a_idx, optimal_input)
    v4_inner += enc_v4_take(weth_idx, executor_idx, out_c - optimal_input)

    a_fwd = enc_v4_unlock(v4_inner)
    b_fwd = enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v3b_idx, forward_data=a_fwd)

    commands = enc_v4_sync(forward_b_idx)
    commands += enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, pm_idx, forward_data=b_fwd)
    return enc_preamble(at) + commands


# ── V3-V4-V2 ──────────────────────────────────────────────────────────────


def _3hop_v3_v4_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3a→PM, V4_TAKE→V2c direct + V2c V2_SWAP_DIRECT + WETH→V3a."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3a_idx = at.add(ha.pool_address)
    v2c_idx = at.add(hc.pool_address)

    # Forward from V3a
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    # Forward from V4b
    forward_b_idx = at.add(
        hb.currency0_address if hb.currency0_address != weth_address else hb.currency1_address
    )

    v4_inner = enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    v4_inner += enc_v4_take(forward_b_idx, v2c_idx, out_b)

    c_cmd = enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, executor_idx)
    a_fwd = enc_v4_unlock(v4_inner) + c_cmd
    a_fwd += enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)

    commands = enc_v4_sync(forward_a_idx)
    commands += enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, forward_data=a_fwd)
    return enc_preamble(at) + commands


# ── V3-V4-V3 ──────────────────────────────────────────────────────────────


def _3hop_v3_v4_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3c reverse, V3a WETH→V3a payment + V4_TAKE WBTC→V3c (IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3a_idx = at.add(ha.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # Forward from V3a
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    # Forward from V4b
    forward_b_idx = at.add(
        hb.currency0_address if hb.currency0_address != weth_address else hb.currency1_address
    )

    # V3a callback: WETH→V3a + V4 unlock (V4b swap, V4_TAKE forward→V3c)
    v4_inner = enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    v4_inner += enc_v4_take(forward_b_idx, v3c_idx, out_b)

    a_fwd = enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)
    a_fwd += enc_v4_unlock(v4_inner)

    # V3c fires first
    c_fwd = enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, forward_data=a_fwd)

    commands = enc_v4_sync(forward_a_idx)
    commands += enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V3-V4-V4 ──────────────────────────────────────────────────────────────


def _3hop_v3_v4_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3a→PM, delta netting, V4_TAKE→V3a (IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3a_idx = at.add(ha.pool_address)

    # Forward from V3a
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)

    v4_inner = enc_v4_settle()
    v4_inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    v4_inner += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    v4_inner += enc_v4_take(weth_idx, v3a_idx, optimal_input)
    v4_inner += enc_v4_take(weth_idx, executor_idx, out_c - optimal_input)

    a_fwd = enc_v4_unlock(v4_inner)

    commands = enc_v4_sync(forward_a_idx)
    commands += enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, forward_data=a_fwd)
    return enc_preamble(at) + commands


# ── V4-V2-V2 ──────────────────────────────────────────────────────────────


def _3hop_v4_v2_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V4_TAKE→V2b direct, V2b→V2c V2_SWAP_DIRECT chain."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency0_address if ha.currency0_address != weth_address else ha.currency1_address
    )

    b_cmd = enc_v2_swap_direct(at.add(hb.pool_address), hb.zfo, out_b, at.add(hc.pool_address))
    c_cmd = enc_v2_swap_direct(at.add(hc.pool_address), hc.zfo, out_c, executor_idx)

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v4_take(forward_a_idx, at.add(hb.pool_address), out_a)
    inner += b_cmd + c_cmd
    inner += enc_v4_settle_delta(weth_idx)

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V2-V3 ──────────────────────────────────────────────────────────────


def _3hop_v4_v2_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3c-reverse: V4_TAKE→V2b, V2b→V3c direct (IIA ✓ during V3c callback)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3c_idx = at.add(hc.pool_address)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency0_address if ha.currency0_address != weth_address else ha.currency1_address
    )
    # Forward from V2b
    _forward_b_idx = at.add(hb.token1_address if hb.zfo else hb.token0_address)

    v4_inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    v4_inner += enc_v4_take(forward_a_idx, at.add(hb.pool_address), out_a)
    v4_inner += enc_v2_swap_direct(at.add(hb.pool_address), hb.zfo, out_b, v3c_idx)
    v4_inner += enc_v4_settle_delta(weth_idx)

    commands = enc_v3_swap_compact(
        v3c_idx, hc.zfo, out_b, executor_idx, forward_data=enc_v4_unlock(v4_inner)
    )
    return enc_preamble(at) + commands


# ── V4-V2-V4 ──────────────────────────────────────────────────────────────


def _3hop_v4_v2_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Single unlock: V4_TAKE→V2b, V2b→exec, V4c swap, settle deltas."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)

    forward_a_idx = at.add(
        ha.currency0_address if ha.currency0_address != weth_address else ha.currency1_address
    )
    forward_b_idx = at.add(hb.token1_address if hb.zfo else hb.token0_address)

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v4_take(forward_a_idx, at.add(hb.pool_address), out_a)
    inner += enc_v2_swap_direct(at.add(hb.pool_address), hb.zfo, out_b, executor_idx)
    inner += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    inner += enc_v4_settle_delta(forward_b_idx)
    inner += enc_v4_settle_delta(weth_idx)

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V3-V2 ──────────────────────────────────────────────────────────────


def _3hop_v4_v3_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V4_TAKE USDC→V3b (IIA ✓), V3b→V2c + WETH→V3b."""
    ha, hb, hc = path_info.hops
    out_a, _out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3b_idx = at.add(hb.pool_address)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency0_address if ha.currency0_address != weth_address else ha.currency1_address
    )
    # Forward from V3b
    _forward_b_idx = at.add(hb.token1_address if hb.zfo else hb.token0_address)

    # V3b callback: V4_TAKE USDC→V3b (IIA ✓) + V2c swap calc
    b_fwd = enc_v4_take(forward_a_idx, v3b_idx, out_a)
    b_fwd += enc_v2_swap_direct(at.add(hc.pool_address), hc.zfo, out_c, executor_idx)

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v3_swap_compact(
        v3b_idx, hb.zfo, out_a, at.add(hc.pool_address), forward_data=b_fwd
    )
    inner += enc_v4_settle_delta(weth_idx)

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V3-V3 ──────────────────────────────────────────────────────────────


def _3hop_v4_v3_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V3c→V3b reverse + V4_TAKE USDC→V3b (IIA ✓), merged WETH settle."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3b_idx = at.add(hb.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency0_address if ha.currency0_address != weth_address else ha.currency1_address
    )

    # V3b callback: V4_TAKE USDC→V3b (IIA ✓ during callback)
    b_fwd = enc_v4_take(forward_a_idx, v3b_idx, out_a)

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v3_swap_compact(
        v3c_idx,
        hc.zfo,
        out_b,
        executor_idx,
        forward_data=enc_v3_swap_compact(
            v3b_idx,
            hb.zfo,
            out_a,
            v3c_idx,
            forward_data=b_fwd,
        ),
    )
    inner += enc_v4_sync(weth_idx)
    inner += enc_erc20_transfer(weth_idx, pm_idx, optimal_input)
    inner += enc_v4_settle()

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V3-V4 ──────────────────────────────────────────────────────────────


def _3hop_v4_v3_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """V4_TAKE forward→V3b (IIA ✓), V3b→PM, delta netting."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    pm_idx = at.add(pool_manager_address)
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)
    v3b_idx = at.add(hb.pool_address)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency0_address if ha.currency0_address != weth_address else ha.currency1_address
    )
    # Forward from V3b
    forward_b_idx = at.add(hb.token1_address if hb.zfo else hb.token0_address)

    # V3b callback: V4_TAKE forward→V3b (IIA ✓) + V4c swap + take profit
    b_fwd = enc_v4_take(forward_a_idx, v3b_idx, out_a)
    b_fwd += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    b_fwd += enc_v4_take(
        weth_idx, executor_idx, out_c - optimal_input if out_c > optimal_input else 0
    )

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v4_sync(forward_b_idx)
    inner += enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, pm_idx, forward_data=b_fwd)
    inner += enc_v4_settle()
    inner += enc_v4_settle_delta(weth_idx)

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V4-V2 ──────────────────────────────────────────────────────────────


def _3hop_v4_v4_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Delta netting V4a↔V4b, V4_TAKE→V2c direct."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)

    # Forward from V4b (= output of V4b swap)
    forward_b_idx = at.add(
        hb.currency0_address if hb.currency0_address != weth_address else hb.currency1_address
    )

    c_cmd = enc_v2_swap_direct(at.add(hc.pool_address), hc.zfo, out_c, executor_idx)

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    inner += enc_v4_take(forward_b_idx, at.add(hc.pool_address), out_b)
    inner += c_cmd
    inner += enc_v4_settle_delta(weth_idx)

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V4-V3 ──────────────────────────────────────────────────────────────


def _3hop_v4_v4_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Delta netting V4a↔V4b, V4_TAKE forward→exec, ERC20 forward→V3c (IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)

    # Forward from V4b
    forward_b_idx = at.add(
        hb.currency0_address if hb.currency0_address != weth_address else hb.currency1_address
    )
    v3c_idx = at.add(hc.pool_address)

    c_pay = enc_erc20_transfer(forward_b_idx, v3c_idx, out_b)

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    inner += enc_v4_take(forward_b_idx, executor_idx, out_b)
    inner += enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, forward_data=c_pay)
    inner += enc_v4_settle_delta(weth_idx)

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V4-V4 ──────────────────────────────────────────────────────────────


def _3hop_v4_v4_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
) -> bytes | None:
    """Pure delta netting, V4_TAKE profit only."""
    if len(hop_outputs) < 3:
        bot_logger.warning(
            f"[cmd-3hop-v4v4v4] hop_outputs has {len(hop_outputs)} elements, expected 3 — skipping"
        )
        return None
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable()
    weth_idx = at.add(weth_address)
    executor_idx = at.add(executor_address)
    zero_idx = at.add(CMD_ZERO_ADDRESS)

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v4_swap_compact(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
        out_a,
    )
    inner += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    inner += enc_v4_take(
        weth_idx, executor_idx, out_c - optimal_input if out_c > optimal_input else 0
    )

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── Dispatch table ──────────────────────────────────────────────────────

_3HOP_ENCODERS: dict[tuple[str, ...], typing.Any] = {
    ("V2", "V2", "V2"): _3hop_v2_v2_v2,
    ("V2", "V2", "V3"): _3hop_v2_v2_v3,
    ("V2", "V2", "V4"): _3hop_v2_v2_v4,
    ("V2", "V3", "V2"): _3hop_v2_v3_v2,
    ("V2", "V3", "V3"): _3hop_v2_v3_v3,
    ("V2", "V3", "V4"): _3hop_v2_v3_v4,
    ("V2", "V4", "V2"): _3hop_v2_v4_v2,
    ("V2", "V4", "V3"): _3hop_v2_v4_v3,
    ("V2", "V4", "V4"): _3hop_v2_v4_v4,
    ("V3", "V2", "V2"): _3hop_v3_v2_v2,
    ("V3", "V2", "V3"): _3hop_v3_v2_v3,
    ("V3", "V2", "V4"): _3hop_v3_v2_v4,
    ("V3", "V3", "V2"): _3hop_v3_v3_v2,
    ("V3", "V3", "V3"): _3hop_v3_v3_v3,
    ("V3", "V3", "V4"): _3hop_v3_v3_v4,
    ("V3", "V4", "V2"): _3hop_v3_v4_v2,
    ("V3", "V4", "V3"): _3hop_v3_v4_v3,
    ("V3", "V4", "V4"): _3hop_v3_v4_v4,
    ("V4", "V2", "V2"): _3hop_v4_v2_v2,
    ("V4", "V2", "V3"): _3hop_v4_v2_v3,
    ("V4", "V2", "V4"): _3hop_v4_v2_v4,
    ("V4", "V3", "V2"): _3hop_v4_v3_v2,
    ("V4", "V3", "V3"): _3hop_v4_v3_v3,
    ("V4", "V3", "V4"): _3hop_v4_v3_v4,
    ("V4", "V4", "V2"): _3hop_v4_v4_v2,
    ("V4", "V4", "V3"): _3hop_v4_v4_v3,
    ("V4", "V4", "V4"): _3hop_v4_v4_v4,
}
