import dataclasses
import sys
import typing
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
if str(_REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(_REPO_ROOT))

from collections.abc import Mapping

from contracts.cmd_stream import SENTINEL_NATIVE, SENTINEL_PM, SENTINEL_SELF, SENTINEL_WETH
from contracts.cmd_stream import (
    AddressTable,
    enc_erc20_transfer,
    enc_erc20_xfer_balance,
    enc_preamble,
    enc_v2_swap_calc,
    enc_v2_swap_compact,
    enc_v2_swap_direct,
    enc_v3_swap_compact,
    enc_v4_batch,
    enc_v4_mint_compact,
    enc_v4_settle,
    enc_v4_settle_all,
    enc_v4_settle_delta,
    enc_v4_swap_compact,
    enc_v4_swap_dynamic,
    enc_v4_sync,
    enc_v4_take_compact,
    enc_v4_take_delta,
    enc_v4_unlock,
    enc_weth_deposit,
    enc_weth_withdraw,
)
from degenbot.arbitrage import (
    HopInfo,
    PathInfo,
    V2HopInfo,
    V3HopInfo,
    V4HopInfo,
    build_hops_from_pools,
)
from degenbot.arbitrage.encoding import fits_int128
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.logging import logger as bot_logger
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS

# Backrun configuration
# ──────────────────────────────────────────────────────────────────

# Default dispatch tunables — match the example's current operational values
# (eth_backrun_v2_v3_v4_rust.py module-top constants) so the BackrunSession
# bridge (slice 5b) is behavior-preserving. Canonical home for the defaults
# is the config object; the example's constants are its current deployment.
_MIN_PROFIT_NET = 1
_FEE_HISTORY_WINDOW = 10
_FEE_PERCENTILES = (10, 50)
_TARGET_PROFIT_RATIO = 1.25
_BLOCKS_BEFORE_NONCE_EXPIRES = 5
_MAX_SIMULATE_CONCURRENT = 50
_AGE_DECAY_CONSTANT = 0.25
_MIN_PRIORITY_FEE_PERCENTILE = 10
_MAX_PRIORITY_FEE_PERCENTILE = 50
_PATH_SUPPRESS_THRESHOLD = 10
_PATH_SUPPRESS_RETRY_INTERVAL = 100
# Ethereum mainnet default allowed intermediate tokens — mirrors the example's
# ETH_MAINNET_ALLOWED_TOKENS set.
_ALLOWED_INTERMEDIATE_TOKENS = frozenset({
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    "0xdAC17F958D2ee523a2206206994597C13D831ec7",  # USDT
    "0x6B175474E89094C44Da98b954EedeAC495271d0F",  # DAI
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",  # WBTC
    "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",  # UNI
    "0x514910771AF9Ca656af840dff83E8264EcF986CA",  # LINK
    "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2",  # SUSHI
    "0xD533a949740bb3306d119CC777fa900bA034cd52",  # CRV
    "0xc00e94Cb662C3520282E6f5717214004A7f26888",  # COMP
    "0x0bc529c00C6401aEF6D220BE8C6Ea1667F6Ad93e",  # YFI
    "0x7D1AfA7B718fb893dB30A3aBc0Cfc608AaCfeBB0",  # MATIC/POL
})
# Default executor deployment constants — mirror the example's env defaults.
_DEFAULT_EXECUTOR_ADDRESS = "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"
_DEFAULT_INJECTED_ADDRESS = "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1"
_DEFAULT_EXECUTOR_OWNER = "0x9C56a29c7231974c269E24F9FB3c29203039089E"


def _checksum_or_empty(addr: str | None) -> str:
    """Checksum an address, returning "" for empty input (mirrors main()'s get_checksum_address)."""
    if not addr:
        return ""
    return get_checksum_address(addr)


@dataclasses.dataclass(frozen=True)
class BackrunConfig:
    """Unified backrun configuration — one object for the ~20 tunables `main()` reads.

    Replaces the three scattered config sources (a ``mainnet.env`` dotenv
    dict, module-top constants, and CLI args) with a single frozen value object.
    Construct via :meth:`from_env`; the bridge onto ``main()`` lives in the
    ``BackrunSession`` orchestration slice. Live defaults (zero-address
    operator + dummy key in dry-run, localhost nodes) reproduce ``main()``'s
    current behavior exactly — no new defaults invented.
    """

    # Operator identity
    operator_address: str
    operator_private_key: str
    # Node endpoints
    node_http: str
    node_ws: str
    # Executor contract + code-injection
    executor_address: str
    executor_owner: str
    inject_executor_code: bool
    injected_address: str
    # Dispatch tunables
    min_profit_net: int
    fee_history_window: int
    fee_percentiles: tuple[int, ...]
    target_profit_ratio: float
    blocks_before_nonce_expires: int
    max_simulate_concurrent: int
    age_decay_constant: float
    min_priority_fee_percentile: int
    max_priority_fee_percentile: int
    path_suppress_threshold: int
    path_suppress_retry_interval: int
    # Path discovery
    allowed_intermediate_tokens: frozenset[str]
    permutation_filter: frozenset[str] | None
    # Run mode
    dry_run: bool

    @classmethod
    def from_env(
        cls,
        env: Mapping[str, str | None],
        *,
        live: bool,
        permutation: str | None,
    ) -> "BackrunConfig":
        """Build a BackrunConfig from a dotenv-style env mapping + CLI flags.

        Reproduces ``main()``'s env-parsing + defaulting exactly:
        - operator: live mode requires both OPERATOR_ADDRESS/PRIVATE_KEY
          (raises ValueError); dry-run defaults to ZERO_ADDRESS + a 0x00..00 key.
        - nodes: missing host/port default to localhost:8545/8546.
        - executor: zero address is a fatal ``ValueError`` (a factory cannot
          return early like ``main()``'s ``return``).
        - inject code: when ``INJECT_EXECUTOR_CODE=="1"``, the executor address
          is overridden to ``INJECTED_EXECUTOR_ADDRESS``.
        - permutation: a CLI string becomes a singleton frozenset; ``None`` stays ``None``.
        """
        # ── Operator ──
        operator_address_raw = env.get("OPERATOR_ADDRESS") or ""
        operator_private_key = env.get("OPERATOR_PRIVATE_KEY") or ""
        operator_address = _checksum_or_empty(operator_address_raw) if operator_address_raw else ""
        if not live:
            # dry-run: allow missing operator → zero-address + dummy key
            if not operator_address:
                operator_address = _ZERO_ADDRESS
            if not operator_private_key:
                operator_private_key = "0x" + "0" * 64
        else:
            msg = (
                "OPERATOR_ADDRESS and OPERATOR_PRIVATE_KEY must be set in mainnet.env for live mode"
            )
            if not operator_address or not operator_private_key:
                raise ValueError(msg)

        # ── Node URLs (host:port composition lifted verbatim from main()) ──
        node_http = (
            f"{env.get('NODE_HOST_HTTP') or 'http://localhost'}:"
            f"{env.get('NODE_PORT_HTTP') or '8545'}"
        )
        node_ws = (
            f"{env.get('NODE_HOST_WEBSOCKET') or 'ws://localhost'}:"
            f"{env.get('NODE_PORT_WEBSOCKET') or '8546'}"
        )

        # ── Executor ──
        executor_address = _checksum_or_empty(
            env.get("EXECUTOR_CONTRACT_ADDRESS") or _DEFAULT_EXECUTOR_ADDRESS
        )
        if executor_address == _ZERO_ADDRESS:
            msg = "EXECUTOR_CONTRACT_ADDRESS is the zero address"
            raise ValueError(msg)

        inject_executor_code = (env.get("INJECT_EXECUTOR_CODE") or "1") == "1"
        injected_address = _checksum_or_empty(
            env.get("INJECTED_EXECUTOR_ADDRESS") or _DEFAULT_INJECTED_ADDRESS
        )
        executor_owner = _checksum_or_empty(
            env.get("EXECUTOR_OWNER_ADDRESS") or _DEFAULT_EXECUTOR_OWNER
        )
        # main() behavior: when INJECT_EXECUTOR_CODE, override executor with injected
        if inject_executor_code:
            executor_address = injected_address

        return cls(
            operator_address=operator_address,
            operator_private_key=operator_private_key,
            node_http=node_http,
            node_ws=node_ws,
            executor_address=executor_address,
            executor_owner=executor_owner,
            inject_executor_code=inject_executor_code,
            injected_address=injected_address,
            min_profit_net=_MIN_PROFIT_NET,
            fee_history_window=_FEE_HISTORY_WINDOW,
            fee_percentiles=_FEE_PERCENTILES,
            target_profit_ratio=_TARGET_PROFIT_RATIO,
            blocks_before_nonce_expires=_BLOCKS_BEFORE_NONCE_EXPIRES,
            max_simulate_concurrent=_MAX_SIMULATE_CONCURRENT,
            age_decay_constant=_AGE_DECAY_CONSTANT,
            min_priority_fee_percentile=_MIN_PRIORITY_FEE_PERCENTILE,
            max_priority_fee_percentile=_MAX_PRIORITY_FEE_PERCENTILE,
            path_suppress_threshold=_PATH_SUPPRESS_THRESHOLD,
            path_suppress_retry_interval=_PATH_SUPPRESS_RETRY_INTERVAL,
            allowed_intermediate_tokens=_ALLOWED_INTERMEDIATE_TOKENS,
            permutation_filter=(frozenset({permutation}) if permutation is not None else None),
            dry_run=not live,
        )


# ──────────────────────────────────────────────────────────────────
# Simulation revert taxonomy
# ──────────────────────────────────────────────────────────────────

# Selector → human name for the revert selectors the cmd_executor / V4
# PoolManager emit. Kept as canonical data so both the (verbose) per-fail
# diagnostic decode in eth_backrun_v2_v3_v4_rust.py and the (short) bucket
# label produced by _classify_revert stay in sync.
_V4_REVERT_SELECTORS: dict[str, str] = {
    "5212cba1": "CurrencyNotSettled()",
    "486aa307": "PoolNotInitialized()",
    "1e048e1d": "InvalidHookResponse()",
    "a3603d66": "SwapQuantityCannotBeZero()",
    "38606b01": "PriceLimitAlreadyExceeded()",
    "30d6072a": "PriceLimitOutOfBounds()",
    "a40afa38": "LockFailure()",
    "5090d6c6": "AlreadyUnlocked()",
    "54e3ca0d": "ManagerLocked()",
}

_EXECUTOR_REVERT_SELECTORS: dict[str, str] = {
    # Legacy (bare assert)
    "4b9dfc58": "!OWNER",
    "49494100": "IIA(insufficient-input-amount)",
    # Custom errors (Vyper 0.5.0a3+)
    "8e4a23d6": "Unauthorized(caller)",
    "b028a63a": "InvalidCallback(caller)",
    "cf479181": "InsufficientBalance(amount,available)",
    "4e88422a": "InsufficientProfit(actual,expected)",
    "83276224": "InvalidCommand(opcode)",
    "60ef0bb0": "BipsTooHigh(bips)",
    "a61be9f0": "InvalidMsgValue(value)",
    "e5b6bf32": "NotPlainEthTransfer()",
}

# Solidity revert selectors shared across all contracts.
_ERROR_STRING_SELECTOR = "08c379a0"  # Error(string)
_PANIC_SELECTOR = "4e487b71"  # Panic(uint256)


def _classify_revert(revert_data: bytes) -> str:
    """Classify raw simulation revert return-data into a short stable label.

    Used by the ``[sim]`` summary to break the ``N failed`` bucket down by root
    cause. Returns the canonical error *name* for custom-error selectors (params
    dropped, so ``InsufficientProfit(1,2)`` and ``InsufficientProfit(3,4)``
    tally together), the decoded message for ``Error(string)``, the panic code
    for ``Panic``, or ``unknown:0x........`` for anything unrecognised.

    Deliberately never raises — a taxonomy must classify every revert, even
    malformed ones, so the summary always adds up.
    """
    if not revert_data:
        return "empty"
    hexed = revert_data.hex()
    if len(hexed) < 8:
        return f"short:{hexed}"
    selector = hexed[:8]
    if selector == _PANIC_SELECTOR:
        # Panic(uint256 code) — code is the first 32-byte arg.
        code = int(hexed[8:72], 16) if len(hexed) >= 72 else 0
        return f"Panic(0x{code:x})"
    if selector == _ERROR_STRING_SELECTOR:
        # Error(string): [sel][offset:32][len:32][data:N]
        try:
            str_len = int(hexed[8 + 64 : 8 + 128], 16)
            str_start = 8 + 64 + 64
            msg = bytes.fromhex(hexed[str_start : str_start + str_len * 2]).decode(
                "utf-8", errors="replace"
            )
        except (ValueError, IndexError):
            return "Error(string:undecodable)"
        return msg or "Error(string:empty)"
    if selector in _V4_REVERT_SELECTORS:
        return _V4_REVERT_SELECTORS[selector].split("(", 1)[0]
    if selector in _EXECUTOR_REVERT_SELECTORS:
        return _EXECUTOR_REVERT_SELECTORS[selector].split("(", 1)[0]
    # Bare 32-byte numeric revert (Vyper): 0x00..00<value>
    if len(hexed) >= 64 and hexed[:24] == "0" * 24:
        return "numeric-revert"
    return f"unknown:0x{selector}"


def _format_failure_breakdown(buckets: dict[str, int]) -> str:
    """Render a ``name=count`` breakdown, highest count first (name breaks ties).

    Returns ``""`` for an empty tally so the caller can skip the suffix when no
    failures were classified.
    """
    if not buckets:
        return ""
    ordered = sorted(buckets.items(), key=lambda kv: (-kv[1], kv[0]))
    return " ".join(f"{name}={count}" for name, count in ordered)


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
    *,
    bribe_bips: int = 0,
    erc6909_profit: bool = False,
    use_v4_batch: bool = False,
) -> bytes | None:
    """Encode an arbitrage path as a cmd_executor command stream.

    Produces a bytes payload for execute(commands) on the cmd_executor contract.
    Uses compact command encoding (V2_SWAP_COMPACT, V2_SWAP_CALC, etc.)
    with an address table for minimal calldata size.

    Returns None if encoding fails for this path type.

    Args:
        bribe_bips: Profit fraction (in bips, 10000=100%) to send as bribe.
            0 = no bribe. Included in preamble preprocessing section.
        erc6909_profit: If True, use V4_MINT_COMPACT instead of V4_TAKE_DELTA
            for profit capture on pure V4 paths. Saves ~20K gas. Requires
            check_mode=2 on expected_balance. See §13 of user guide.
        use_v4_batch: If True, use V4_BATCH instead of individual V4 swap
            commands for pure V4 paths. Saves gas from single PM extcall.
    """
    num_hops = len(path_info.hops)

    # Generalized N-hop V2 (2+ hops): flash borrow + chained V2_SWAP_CALC
    if all(isinstance(h, V2HopInfo) for h in path_info.hops):
        return _encode_cmd_v2_n_hop(path_info, optimal_input, hop_outputs, executor_address, weth_address, bribe_bips=bribe_bips)

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
                bribe_bips=bribe_bips,
                erc6909_profit=erc6909_profit,
                use_v4_batch=use_v4_batch,
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
        return _encode_cmd_3_hop(path_info, optimal_input, hop_outputs, executor_address, pool_manager_address, weth_address, bribe_bips=bribe_bips, erc6909_profit=erc6909_profit, use_v4_batch=use_v4_batch)

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
    weth_address: str,
    *,
    bribe_bips: int = 0,
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
        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
        )
        executor_idx = SENTINEL_SELF

        # Register all pool addresses (preserves insertion order)
        pool_indices = [at.add(h.pool_address) for h in path_info.hops]

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
            fee=hop_a.fee,
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
    *,
    bribe_bips: int = 0,
    erc6909_profit: bool = False,
    use_v4_batch: bool = False,
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

        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
        pm_idx = SENTINEL_PM
        executor_idx = SENTINEL_SELF
        zero_idx = SENTINEL_NATIVE

        c0_a_idx = at.add(hop_a.currency0_address)
        c1_a_idx = at.add(hop_a.currency1_address)
        c0_b_idx = at.add(hop_b.currency0_address)
        c1_b_idx = at.add(hop_b.currency1_address)
        weth_idx = SENTINEL_WETH
        if (
            a_outputs_native
            or b_needs_native
            or input_currency_a == NATIVE_CURRENCY_ADDRESS
            or output_currency_b == NATIVE_CURRENCY_ADDRESS
        ):
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        # 1. V4 swap for pool A (always explicit amount)
        if not (use_v4_batch and not currency_gap):
            # Individual command (default path, or wrap/unwrap case)
            inner = enc_v4_swap_compact(
                c0_idx=c0_a_idx,
                c1_idx=c1_a_idx,
                fee=hop_a.fee,
                tick_spacing=hop_a.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_a.zfo,
                amount_u96=optimal_input,
            )
        else:
            inner = b""  # V4_BATCH will handle both swaps

        if currency_gap:
            # Take intermediate token out of PM, convert, then second swap with
            # explicit amount. Pattern verified in test_cmd_executor_v4v4_wrap_unwrap.py.
            # Intermediate token must leave PM because PM tracks WETH and native
            # ETH deltas separately — V4_SWAP_DYNAMIC would read the wrong delta type.

            # 2. Take intermediate token to executor
            if a_outputs_native:
                # Pool A output is ETH
                inner += enc_v4_take_compact(native_idx, executor_idx, forward_out)
                # 3. Wrap ETH → WETH
                inner += enc_weth_deposit(forward_out)
            else:
                # Pool A output is WETH
                inner += enc_v4_take_compact(weth_idx, executor_idx, forward_out)
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
                amount_u96=forward_out,
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
            # Same intermediate currency — delta netting
            # Intermediate token stays in PM's delta ledger between swaps.

            if use_v4_batch:
                # V4_BATCH: single extcall to PM, auto-settles WETH/native ETH
                inner += enc_v4_batch([
                    (
                        c0_a_idx, c1_a_idx, hop_a.fee, hop_a.tick_spacing,
                        zero_idx, hop_a.zfo, optimal_input,
                    ),
                    (
                        c0_b_idx, c1_b_idx, hop_b.fee, hop_b.tick_spacing,
                        zero_idx, hop_b.zfo, 0,  # dynamic amount from delta
                    ),
                ])
                # V4_BATCH auto-settles WETH + native ETH deltas.
                # For ERC-20 profit, still need explicit take.
                if output_currency_b not in (NATIVE_CURRENCY_ADDRESS, weth_address):
                    profit_idx = c1_b_idx if hop_b.zfo else c0_b_idx
                    inner += enc_v4_take_delta(profit_idx, executor_idx)
            else:
                # Individual swap commands — more calldata-efficient for 2 hops
                inner += enc_v4_swap_dynamic(
                    c0_idx=c0_b_idx,
                    c1_idx=c1_b_idx,
                    fee=hop_b.fee,
                    tick_spacing=hop_b.tick_spacing,
                    hooks_idx=zero_idx,
                    zfo=hop_b.zfo,
                )

            # Profit capture: V4_MINT_COMPACT (ERC6909) or V4_TAKE_DELTA (physical)
            if erc6909_profit and output_currency_b == weth_address:
                # V4_MINT_COMPACT: convert profit delta to ERC6909 balance.
                # No physical transfer — saves ~20K gas. Requires check_mode=2.
                # Rounding residuals handled by V4_SETTLE_ALL below.
                profit_amount = weth_out - optimal_input
                if profit_amount > 0:
                    inner += enc_v4_mint_compact(weth_idx, executor_idx, profit_amount)
            else:
                # V4_TAKE_DELTA: reads actual positive delta from PM exttload.
                if not use_v4_batch or output_currency_b not in (NATIVE_CURRENCY_ADDRESS, weth_address):
                    if output_currency_b == NATIVE_CURRENCY_ADDRESS:
                        inner += enc_v4_take_delta(native_idx, executor_idx)
                    elif output_currency_b == weth_address:
                        inner += enc_v4_take_delta(weth_idx, executor_idx)
                    else:
                        profit_idx = c1_b_idx if hop_b.zfo else c0_b_idx
                        inner += enc_v4_take_delta(profit_idx, executor_idx)

            # Auto-settle all remaining nonzero deltas (handles rounding residuals)
            inner += enc_v4_settle_all()

        commands = enc_v4_unlock(inner)
        return enc_preamble(at, bribe_bips=bribe_bips) + commands
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
    **kwargs,
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

        _v4_out_native = v4_output_is_native(hop_v4)

        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
        pm_idx = SENTINEL_PM
        executor_idx = SENTINEL_SELF
        zero_idx = SENTINEL_NATIVE

        c0_v4_idx = at.add(hop_v4.currency0_address)
        c1_v4_idx = at.add(hop_v4.currency1_address)
        v3_idx = at.add(hop_v3.pool_address)
        weth_idx = SENTINEL_WETH
        if _v4_out_native:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        # 1. V4 swap (input→forward)
        inner = enc_v4_swap_compact(
            c0_idx=c0_v4_idx,
            c1_idx=c1_v4_idx,
            fee=hop_v4.fee,
            tick_spacing=hop_v4.tick_spacing,
            hooks_idx=zero_idx,
            zfo=hop_v4.zfo,
            amount_u96=optimal_input,
        )

        if _v4_out_native:
            # V4 output is native ETH — take to executor, then wrap
            inner += enc_v4_take_compact(native_idx, executor_idx, forward_out)
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
            inner += enc_v4_take_compact(forward_idx, executor_idx, forward_out)

            # 3. V3 swap with auto-pay — V3 sends WETH to executor,
            #    auto-pays the forward token (e.g., USDC).
            inner += enc_v3_swap_compact(
                v3_idx,
                hop_v3.zfo,
                forward_out,
                executor_idx,
            )

            # 4. Settle V4's input currency debt.
            # V3's swap sent WETH to executor. If V4's input is native ETH,
            # must unwrap WETH→ETH before settling (WETH≠ETH mismatch).
            _v4_in_native = v4_input_is_native(hop_v4)
            if _v4_in_native:
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
    **kwargs,
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
            ERC20_TRANSFER V4 output to V3 (pay V3's input-token debt)
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

        _v4_in_native = v4_input_is_native(hop_v4)

        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
        pm_idx = at.add(pool_manager_address)
        executor_idx = SENTINEL_SELF
        zero_idx = SENTINEL_NATIVE

        v3_idx = at.add(hop_v3.pool_address)
        c0_v4_idx = at.add(hop_v4.currency0_address)
        c1_v4_idx = at.add(hop_v4.currency1_address)
        weth_idx = SENTINEL_WETH
        if _v4_in_native:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        if _v4_in_native:
            # V4 needs native ETH — unwrap WETH→ETH first, then V4 swap
            # Pattern from test_cmd_executor_inline_wrap_unwrap.py::TestV3ToV4InlineUnwrap
            v4_inner = enc_v4_swap_compact(
                c0_idx=c0_v4_idx,
                c1_idx=c1_v4_idx,
                fee=hop_v4.fee,
                tick_spacing=hop_v4.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_v4.zfo,
                amount_u96=forward_out,
            )
            # V4 settle native ETH (executor pays unwrapped ETH to PM)
            v4_inner += enc_v4_settle_delta(native_idx)
            # V4 take profit (WETH or USDC)
            output_currency = hop_v4.currency1_address if hop_v4.zfo else hop_v4.currency0_address
            if output_currency == NATIVE_CURRENCY_ADDRESS:
                v4_inner += enc_v4_take_compact(native_idx, executor_idx, weth_out)
            else:
                # V4 output is ERC-20 — take normally
                output_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
                v4_inner += enc_v4_take_compact(output_idx, executor_idx, weth_out)
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
                amount_u96=forward_out,
            )
            # Take profit: use the V4 output currency (positive delta side)
            # zfo=True → output is currency1; zfo=False → output is currency0
            output_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
            v4_inner += enc_v4_take_compact(output_idx, executor_idx, weth_out)
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
    **kwargs,
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

        _v4_out_native = v4_output_is_native(hop_v4)

        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
        pm_idx = at.add(pool_manager_address)
        executor_idx = SENTINEL_SELF
        zero_idx = SENTINEL_NATIVE

        c0_v4_idx = at.add(hop_v4.currency0_address)
        c1_v4_idx = at.add(hop_v4.currency1_address)
        v2_idx = at.add(hop_v2.pool_address)
        weth_idx = SENTINEL_WETH
        if _v4_out_native:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        # 1. V4 swap (input→forward)
        inner = enc_v4_swap_compact(
            c0_idx=c0_v4_idx,
            c1_idx=c1_v4_idx,
            fee=hop_v4.fee,
            tick_spacing=hop_v4.tick_spacing,
            hooks_idx=zero_idx,
            zfo=hop_v4.zfo,
            amount_u96=optimal_input,
        )

        if _v4_out_native:
            # V4 output is native ETH, V2 needs WETH
            # Pattern from test_cmd_executor_inline_wrap_unwrap.py::TestV4ToV2InlineWrap
            # 2. Take ETH to executor, wrap to WETH
            inner += enc_v4_take_compact(native_idx, executor_idx, forward_out)
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
                fee=hop_v2.fee,
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
            inner += enc_v4_take_compact(forward_idx, v2_idx, forward_out)

            # V2_SWAP_CALC reads excess balance in V2 pool, no callback needed.
            # V4_TAKE sent the forward token directly to V2 (direct custody),
            # creating excess balance that V2_SWAP_CALC consumes.
            inner += enc_v2_swap_calc(v2_idx, hop_v2.zfo, executor_idx, fee=hop_v2.fee)

            # Settle V4's input-currency debt.
            # V2 sent the output (WETH or USDC) to the executor.
            _v4_in_native = v4_input_is_native(hop_v4)
            if _v4_in_native:
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
    **kwargs,
) -> bytes | None:
    """Encode V2-V4 2-hop arbitrage as cmd_executor command stream.

    V2 flash borrow runs first, then V4_UNLOCK wraps sync+transfer+settle+swap+take.

    When V4 requires native ETH as input, V2's WETH output must be
    unwrapped via WETH_WITHDRAW before the V4 swap.

    Flow:
      [V4 input is native ETH]:
        V2_SWAP_COMPACT (flash, callback)
          V2 callback: WETH_WITHDRAW → V4_UNLOCK(swap + settle_delta(ETH) + take(output))
          → ERC20_TRANSFER(forward_token→V2) — satisfy V2's K-invariant with forward
            token (V2 is owed WETH, but receiving forward token also satisfies K)
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

        _v4_in_native = v4_input_is_native(hop_v4)

        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
        pm_idx = at.add(pool_manager_address)
        executor_idx = SENTINEL_SELF
        zero_idx = SENTINEL_NATIVE

        v2_idx = at.add(hop_v2.pool_address)
        c0_v4_idx = at.add(hop_v4.currency0_address)
        c1_v4_idx = at.add(hop_v4.currency1_address)
        weth_idx = SENTINEL_WETH
        if _v4_in_native:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)

        # Forward token = output of V2 swap
        forward_addr = hop_v2.token1_address if hop_v2.zfo else hop_v2.token0_address
        forward_idx = at.add(forward_addr)

        if _v4_in_native:
            # V4 needs native ETH — unwrap WETH first, then V4 swap + settle + take
            # Pattern from test_cmd_executor_inline_wrap_unwrap.py::TestV2ToV4InlineUnwrap
            v4_inner = enc_v4_swap_compact(
                c0_idx=c0_v4_idx,
                c1_idx=c1_v4_idx,
                fee=hop_v4.fee,
                tick_spacing=hop_v4.tick_spacing,
                hooks_idx=zero_idx,
                zfo=hop_v4.zfo,
                amount_u96=forward_out,
            )
            # V4 settle native ETH (executor pays unwrapped ETH to PM)
            v4_inner += enc_v4_settle_delta(native_idx)
            # V4 take profit — use the V4 output currency index
            output_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
            v4_inner += enc_v4_take_compact(output_idx, executor_idx, weth_out)
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
            _v4_out_native = v4_output_is_native(hop_v4)

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
                amount_u96=forward_out,
            )
            # Take profit: if V4 outputs native ETH, add native_idx and use it
            if _v4_out_native:
                native_idx = at.add(NATIVE_CURRENCY_ADDRESS)
                v4_inner += enc_v4_take_compact(native_idx, executor_idx, weth_out)
            else:
                output_idx = c1_v4_idx if hop_v4.zfo else c0_v4_idx
                v4_inner += enc_v4_take_compact(output_idx, executor_idx, weth_out)
            # Settle any rounding residuals inside V4 unlock
            v4_inner += enc_v4_settle_all()

            # V2 callback: V4 unlock first (executor gets V4 output), then pay V2
            callback_cmds = enc_v4_unlock(v4_inner)
            if _v4_out_native:
                # V4 gave native ETH — wrap to WETH before paying V2
                callback_cmds += enc_weth_deposit(weth_out)
            callback_cmds += enc_erc20_transfer(weth_idx, v2_idx, optimal_input)

        # Top-level: V2 flash swap, then V4 unlock (or V4 already in callback)
        outer = enc_v2_swap_compact(
            pool_idx=v2_idx,
            zfo=hop_v2.zfo,
            amount_out=forward_out,
            recipient_idx=executor_idx,
            fee=hop_v2.fee,
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
    **kwargs,
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

        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
        executor_idx = SENTINEL_SELF
        v3_a_idx = at.add(hop_a.pool_address)
        v3_b_idx = at.add(hop_b.pool_address)
        weth_idx = SENTINEL_WETH

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
    **kwargs,
) -> bytes | None:
    """Encode V2-V3 2-hop arbitrage as cmd_executor command stream.

    V2 flash borrow sends forward to executor. In the callback, V3 swap
    runs with forward_data containing an ERC20_TRANSFER that satisfies
    V3's IIA check, then WETH is repaid to V2.

    CRITICAL: The ERC20_TRANSFER must be inside V3's forward_data, NOT
    before V3.swap(). If the transfer runs before the swap, V3 captures
    it in balance_before and IIA fails (no new tokens arrive during
    the callback itself).

    Flow:
      V2_SWAP_COMPACT (WETH→forward, flash borrow, recipient=executor)
        callback (forward_data):
          V3_SWAP_COMPACT (forward→WETH, recipient=executor, forward_data)
            V3 callback (forward_data):
              ERC20_TRANSFER forward to V3 (satisfies IIA during callback)
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

        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
        )
        executor_idx = SENTINEL_SELF
        v2_idx = at.add(hop_a.pool_address)
        v3_idx = at.add(hop_b.pool_address)
        weth_idx = SENTINEL_WETH

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
            fee=hop_a.fee,
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
    **kwargs,
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

        at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
        )
        executor_idx = SENTINEL_SELF
        v3_idx = at.add(hop_a.pool_address)
        v2_idx = at.add(hop_b.pool_address)
        weth_idx = SENTINEL_WETH

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
            fee=hop_b.fee,
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
    *,
    bribe_bips: int = 0,
    erc6909_profit: bool = False,
    use_v4_batch: bool = False,
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
        return encoder(
            path_info, optimal_input, hop_outputs,
            executor_address, pool_manager_address, weth_address,
            bribe_bips=bribe_bips, erc6909_profit=erc6909_profit, use_v4_batch=use_v4_batch,
        )
    except (ValueError, OverflowError) as e:
        bot_logger.info(f"[cmd-3hop] {hop_types}: {type(e).__name__}: {e}")
        return None


def _enc_v4_swap(hop: V4HopInfo, at: AddressTable) -> bytes:
    """Encode V4_SWAP_COMPACT for a V4HopInfo, adding addresses to table."""
    c0_idx = at.add(hop.currency0_address)
    c1_idx = at.add(hop.currency1_address)
    zero_idx = SENTINEL_NATIVE
    return enc_v4_swap_compact(
        c0_idx=c0_idx,
        c1_idx=c1_idx,
        fee=hop.fee,
        tick_spacing=hop.tick_spacing,
        hooks_idx=zero_idx,
        zfo=hop.zfo,
        amount_u96=0,  # will be overridden by caller
    )


# ── V2-V2-V2 ──────────────────────────────────────────────────────────────


def _3hop_v2_v2_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """Reverse-order flash borrow: V2c first, V2a→V2b via V2_SWAP_CALC."""
    ha, hb, hc = path_info.hops
    _out_a, _out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    v2a_idx = at.add(ha.pool_address)
    v2b_idx = at.add(hb.pool_address)
    v2c_idx = at.add(hc.pool_address)

    # Inside V2c callback: WETH→V2a (creates excess), V2a→V2b, V2b→V2c.
    # Use V2_SWAP_CALC so amounts derive from on-chain reserves, avoiding
    # UniswapV2: K reverts when solver reserve estimates are stale.
    c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    c_fwd += enc_v2_swap_calc(v2a_idx, ha.zfo, v2b_idx, fee=ha.fee)
    c_fwd += enc_v2_swap_calc(v2b_idx, hb.zfo, v2c_idx, fee=hb.fee)

    # Flash borrow from V2c
    commands = enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, fee=hc.fee, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V2-V2-V3 ──────────────────────────────────────────────────────────────


def _3hop_v2_v2_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """Reverse-order: V3c fires first, V2a→V2b via V2_SWAP_CALC."""
    ha, hb, hc = path_info.hops
    _out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    v2a_idx = at.add(ha.pool_address)
    v2b_idx = at.add(hb.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # Inside V3c callback: WETH→V2a, V2a→V2b, V2b→V3c.
    # V2_SWAP_CALC reads actual reserves so the intermediate amounts match
    # current state, avoiding UniswapV2: K on the first V2 hop.
    c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    c_fwd += enc_v2_swap_calc(v2a_idx, ha.zfo, v2b_idx, fee=ha.fee)
    c_fwd += enc_v2_swap_calc(v2b_idx, hb.zfo, v3c_idx, fee=hb.fee)

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
    **kwargs,
) -> bytes | None:
    """V2a→V2b inside V4 unlock, V4 final hop dynamic.

    Uses V2_SWAP_CALC for both V2 swaps so amounts derive from on-chain
    reserves. The final V4 swap is dynamic: it reads the actual forward_b
    balance delivered to the PoolManager by V2b, avoiding CurrencyNotSettled
    when solver hop_outputs are stale/rounded relative to chain state.
    """
    ha, hb, hc = path_info.hops
    _out_a, _out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    _executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v2a_idx = at.add(ha.pool_address)
    v2b_idx = at.add(hb.pool_address)
    c0_idx = at.add(hc.currency0_address)
    c1_idx = at.add(hc.currency1_address)

    # Forward token from V2b (output of V2b swap, input to V4c)
    forward_b_addr = hb.token1_address if hb.zfo else hb.token0_address
    forward_b_idx = at.add(forward_b_addr)

    # Phase order inside V4 unlock:
    # 1. Record PM forward_b balance (0) so V2b->PM can be settled.
    # 2. Borrow WETH from PM to kickstart V2a flash swap.
    # 3. V2a -> V2b, then V2b -> PM (delivers forward_b).
    # 4. Settle forward_b delta so the dynamic V4 swap can consume it.
    # 5. Dynamic V4c consumes forward_b; net WETH delta is profit.
    # 6. Settle all remaining WETH/native deltas.
    inner = enc_v4_sync(forward_b_idx)
    inner += enc_v4_take_compact(weth_idx, v2a_idx, optimal_input)
    inner += enc_v2_swap_calc(v2a_idx, ha.zfo, v2b_idx, fee=ha.fee)
    inner += enc_v2_swap_calc(v2b_idx, hb.zfo, pm_idx, fee=hb.fee)
    inner += enc_v4_settle()
    inner += enc_v4_swap_dynamic(
        c0_idx, c1_idx, hc.fee, hc.tick_spacing, zero_idx, hc.zfo
    )
    inner += enc_v4_settle_all()

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V2-V3-V2 ──────────────────────────────────────────────────────────────


def _3hop_v2_v3_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """Reverse-order from V2c: V3b(to=V2c), V2a→V3b during V3b callback (IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, _out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    _usdc_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    executor_idx = SENTINEL_SELF
    v2a_idx = at.add(ha.pool_address)
    v2c_idx = at.add(hc.pool_address)
    v3b_idx = at.add(hb.pool_address)

    # V3b callback: WETH→V2a (excess) + V2a→V3b (IIA ✓)
    b_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    b_fwd += enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx)

    # V2c callback: V3b swap (to=V2c)
    c_fwd = enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, v2c_idx, forward_data=b_fwd)

    commands = enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, fee=hc.fee, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V2-V3-V3 ──────────────────────────────────────────────────────────────


def _3hop_v2_v3_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """V3c outermost, V2a inside V3b callback (to=V3b, IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
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
    **kwargs,
) -> bytes | None:
    """V3b outermost, V2a inside V3b callback, V3b→PM + V4 unlock."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
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
    v4_inner += enc_v4_take_compact(weth_idx, v2a_idx, optimal_input)
    v4_inner += enc_v4_take_compact(weth_idx, executor_idx, out_c - optimal_input)
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
    **kwargs,
) -> bytes | None:
    """Reverse-order from V2c, V2a→PM delta netting."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    forward_b_idx = at.add(
        hb.currency1_address if hb.zfo else hb.currency0_address
    )
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v2a_idx = at.add(ha.pool_address)
    v2c_idx = at.add(hc.pool_address)

    # V4 unlock: sync/settle from V2a→PM, V4b swap, V4_TAKE→V2c
    v4_inner = enc_v4_sync(forward_a_idx)
    v4_inner += enc_v2_swap_calc(v2a_idx, ha.zfo, pm_idx, fee=ha.fee)
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
    v4_inner += enc_v4_take_compact(forward_b_idx, v2c_idx, out_b)

    # V2c fires first (reverse-order). Callback: WETH→V2a, V4 unlock
    c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, optimal_input)
    c_fwd += enc_v4_unlock(v4_inner)

    commands = enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, fee=hc.fee, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V2-V4-V3 ──────────────────────────────────────────────────────────────


def _3hop_v2_v4_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """V3c reverse, V2a→PM inside unlock, V4_TAKE forward→V3c."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    forward_b_idx = at.add(
        hb.currency1_address if hb.zfo else hb.currency0_address
    )
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v2a_idx = at.add(ha.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # V4 unlock: sync, V2a→PM, settle, V4b swap, V4_TAKE forward→V3c
    v4_inner = enc_v4_sync(forward_a_idx)
    v4_inner += enc_v2_swap_calc(v2a_idx, ha.zfo, pm_idx, fee=ha.fee)
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
    v4_inner += enc_v4_take_compact(forward_b_idx, v3c_idx, out_b)

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
    **kwargs,
) -> bytes | None:
    """V4_TAKE WETH→V2a (excess), V2a→PM, delta netting, V4_TAKE profit."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE

    v4_inner = enc_v4_sync(forward_a_idx)
    v4_inner += enc_v4_take_compact(weth_idx, at.add(ha.pool_address), optimal_input)
    v4_inner += enc_v2_swap_calc(at.add(ha.pool_address), ha.zfo, pm_idx, fee=ha.fee)
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
    v4_inner += enc_v4_swap_dynamic(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
    )
    v4_inner += enc_v4_settle_all()

    return enc_preamble(at) + enc_v4_unlock(v4_inner)


# ── V3-V2-V2 ──────────────────────────────────────────────────────────────


def _3hop_v3_v2_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """V3a outermost: sends USDC→V2b, V2b→V2c direct + WETH→V3a."""
    ha, hb, hc = path_info.hops
    _out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
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
    **kwargs,
) -> bytes | None:
    """Reverse-order from V3c: V3a→V2b, V2b→V3c direct + explicit WETH→V3a."""
    ha, hb, hc = path_info.hops
    _out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
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
    **kwargs,
) -> bytes | None:
    """V3a→V2b→V4c: V3a sends forward_a directly to V2b.

    V3a sends forward_a directly to V2b pool (recipient=v2b_idx), so V2b
    already has the flash-swap input. V2b callback only runs V4 unlock
    (no erc20 payment to V2b). V3a callback pays WETH via
    erc20_transfer with the deterministic optimal_input.
    """
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3a_idx = at.add(ha.pool_address)
    v2b_idx = at.add(hb.pool_address)

    forward_b_idx = at.add(hb.token1_address if hb.zfo else hb.token0_address)

    # V4 unlock inner: swap C, take WETH profit, settle forward_b
    v4_inner = enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    v4_inner += enc_v4_take_compact(weth_idx, executor_idx, out_c)
    v4_inner += enc_v4_settle_delta(forward_b_idx)

    # V2b callback: V4 unlock only (V2b already has forward_a from V3a's recipient)
    b_fwd = enc_v4_unlock(v4_inner)

    # V3a callback: V2b swap + pay WETH to V3a
    a_fwd = enc_v2_swap_compact(v2b_idx, hb.zfo, out_b, executor_idx, fee=hb.fee, forward_data=b_fwd)
    a_fwd += enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)

    # Top level: V3a swap (sends forward_a directly to V2b pool)
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
    **kwargs,
) -> bytes | None:
    """Reverse-order from V2c: V3a→V3b direct, V2c V2_SWAP_DIRECT + WETH→V3a."""
    ha, hb, hc = path_info.hops
    out_a, _out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
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
    **kwargs,
) -> bytes | None:
    """Reverse-order direct custody, WETH auto-pay (no explicit ERC20 transfers)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    executor_idx = SENTINEL_SELF
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
    **kwargs,
) -> bytes | None:
    """V3a→V3b→V4c: reverse-order V3, V3b→PM, V4_TAKE→V3a.

    Per docs/pool-mechanics.md §5 & §4.1 (V3-V3-V4):
    V3→V3 requires reverse-order: V3b fires first, V3a sends forward_a
    to V3b during V3b's callback (IIA ✓). V3b sends forward_b to PM
    (delta netting). V4_TAKE sends WETH to V3a during V3a's callback
    (IIA ✓). sync(forward_b) before V3b swap, settle inside V4 unlock.
    """
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3a_idx = at.add(ha.pool_address)
    v3b_idx = at.add(hb.pool_address)

    forward_b_idx = at.add(hb.token1_address if hb.zfo else hb.token0_address)

    # V4 unlock inner: settle forward_b, V4c swap, V4_TAKE WETH→V3a, settle deltas
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
    v4_inner += enc_v4_take_compact(weth_idx, v3a_idx, optimal_input)
    v4_inner += enc_v4_settle_all()

    # V3a callback: V4 unlock + erc20_transfer WETH→V3a (explicit auto-pay)
    a_fwd = enc_v4_unlock(v4_inner)

    # V3b callback: V3a swap (sends forward_a→V3b during callback, IIA ✓) + V3a callback
    b_fwd = enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v3b_idx, forward_data=a_fwd)

    # Top level: sync forward_b + V3b swap (sends forward_b→PM, reverse-order)
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
    **kwargs,
) -> bytes | None:
    """V3a→V4b→V2c: V3a sends forward_a to PM, V4_TAKE→V2c direct.

    Per docs/pool-mechanics.md §4.1 (V3-V4-V2):
    V3a→PM (recipient=pm_idx, sync+settle for delta netting), V4 unlock
    inside V3a callback, V4_TAKE sends forward_b directly to V2c
    (V2 K-invariant ✓ for excess balance), V2c V2_SWAP_DIRECT.
    """
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3a_idx = at.add(ha.pool_address)
    v2c_idx = at.add(hc.pool_address)

    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    forward_b_idx = at.add(
        hb.currency1_address if hb.zfo else hb.currency0_address
    )

    # V4 unlock inner: settle forward_a, V4b swap, V4_TAKE forward_b→V2c, settle deltas
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
    v4_inner += enc_v4_take_compact(forward_b_idx, v2c_idx, out_b)
    v4_inner += enc_v4_settle_all()

    # V3a callback: V4 unlock + V2c swap (forward_b at V2c, auto-pay K-invariant ✓) + pay WETH to V3a
    a_fwd = enc_v4_unlock(v4_inner)
    a_fwd += enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, executor_idx)
    a_fwd += enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)

    # Top level: sync forward_a (before V3a sends to PM) + V3a swap (forward_a→PM)
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
    **kwargs,
) -> bytes | None:
    """V3a→V4b→V3c: V3c-reverse pattern.

    V3c fires first (outermost). V3's optimistic transfer delivers V3c's WETH
    output to the executor before the callback, and V3a's forward_a output to
    the PoolManager (recipient=PM) before V3a's callback — so the V4 unlock
    inside V3a's callback sees the forward_a deposit.

    Inside the V4 unlock:
      V4_SETTLE          — credit V3a's *actual* forward_a deposit
      V4_SWAP_DYNAMIC    — consume the actual settled forward_a (PM exttload)
      V4_TAKE_DELTA      — take the *actual* produced forward_b → V3c
      V4_SETTLE_ALL      — sweep residual dust before the unlock closes

    Using ``V4_SWAP_DYNAMIC`` + ``V4_TAKE_DELTA`` (not ``V4_SWAP_COMPACT`` /
    ``V4_TAKE_COMPACT`` with static solver amounts) avoids ``CurrencyNotSettled``
    when V3a's on-chain deposit or the V4 swap's on-chain output differs from
    the solver's ``out_a`` / ``out_b`` — the residual PM delta would otherwise
    survive at unlock-end. Mirrors the V2-V2-V4 fix (commit 2e505536).
    """
    ha, hb, hc = path_info.hops
    _out_a, _out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3a_idx = at.add(ha.pool_address)
    v3c_idx = at.add(hc.pool_address)

    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)
    forward_b_idx = at.add(
        hb.currency1_address if hb.zfo else hb.currency0_address
    )

    # V4 unlock inner: settle the actual forward_a deposit, dynamic-swap it to
    # forward_b, take the full actual forward_b output to V3c, then sweep dust.
    v4_inner = enc_v4_settle()
    v4_inner += enc_v4_swap_dynamic(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
    )
    v4_inner += enc_v4_take_delta(forward_b_idx, v3c_idx)
    v4_inner += enc_v4_settle_all()

    # V3a callback forward_data: explicit WETH→V3a payment (auto-pay can't be
    # used — V3a's callback must also process the V4 unlock) + V4 unlock.
    a_fwd = enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)
    a_fwd += enc_v4_unlock(v4_inner)

    # V3c callback: V3a swap (forward_a→PM) with forward_data
    c_fwd = enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, forward_data=a_fwd)

    # Top level: sync forward_a before V3c swap (baseline PM balance so the
    # inner V4_SETTLE credits V3a's subsequent deposit), V3c swap.
    commands = enc_v4_sync(forward_a_idx)
    commands += enc_v3_swap_compact(v3c_idx, hc.zfo, _out_b, executor_idx, forward_data=c_fwd)
    return enc_preamble(at) + commands


# ── V3-V4-V4 ──────────────────────────────────────────────────────────────


def _3hop_v3_v4_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """V3a→V4b→V4c: V3a callback runs V4 unlock with delta netting.

    Pattern from test_cmd_executor_three_hop_permutations.py::TestV3V4V4:
    V3a sends forward_a to executor, V3a callback runs V4 unlock
    (swap B + swap C, take WETH, settle forward_a) then pays WETH to V3a.
    V4b and V4c deltas net: forward_a cancels, leaving WETH take + settle.
    """
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3a_idx = at.add(ha.pool_address)

    forward_a_idx = at.add(ha.token1_address if ha.zfo else ha.token0_address)

    # V4 unlock inner: swap B + swap C, take WETH, settle forward_a
    v4_inner = enc_v4_swap_compact(
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
    v4_inner += enc_v4_take_compact(weth_idx, executor_idx, out_c)

    # Settlement: V3a sent forward_a to executor (not PM directly),
    # so the executor holds forward_a tokens. V4_SETTLE credits any
    # PM delta from prior sync+transfer; V4_SETTLE_DELTA would also
    # work here since the executor holds the tokens to transfer.
    v4_inner = enc_v4_settle() + v4_inner

    # V3a callback: V4 unlock + pay WETH to V3a
    a_fwd = enc_v4_unlock(v4_inner)
    a_fwd += enc_erc20_transfer(weth_idx, v3a_idx, optimal_input)

    # Top level: V3a swap
    commands = enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, executor_idx, forward_data=a_fwd)
    return enc_preamble(at) + commands


# ── V4-V2-V2 ──────────────────────────────────────────────────────────────


def _3hop_v4_v2_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """V4_TAKE→V2b direct, V2b→V2c V2_SWAP_DIRECT chain."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency1_address if ha.zfo else ha.currency0_address
    )

    b_cmd = enc_v2_swap_calc(at.add(hb.pool_address), hb.zfo, at.add(hc.pool_address), fee=hb.fee)
    c_cmd = enc_v2_swap_calc(at.add(hc.pool_address), hc.zfo, executor_idx, fee=hc.fee)

    inner = enc_v4_swap_compact(
        at.add(ha.currency0_address),
        at.add(ha.currency1_address),
        ha.fee,
        ha.tick_spacing,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    inner += enc_v4_take_compact(forward_a_idx, at.add(hb.pool_address), out_a)
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
    **kwargs,
) -> bytes | None:
    """V3c-reverse: V4_TAKE→V2b, V2b→V3c direct (IIA ✓ during V3c callback)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3c_idx = at.add(hc.pool_address)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency1_address if ha.zfo else ha.currency0_address
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
    v4_inner += enc_v4_take_compact(forward_a_idx, at.add(hb.pool_address), out_a)
    v4_inner += enc_v2_swap_calc(at.add(hb.pool_address), hb.zfo, v3c_idx, fee=hb.fee)
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
    **kwargs,
) -> bytes | None:
    """V4a→V2b→V4c inside single unlock.

    Per docs/pool-mechanics.md §4.1 (V4-V2-V4):
    V4a swap → V4_TAKE forward_a→V2b (creates excess balance) →
    V2b V2_SWAP_CALC→executor (sends forward_b to executor) →
    V4c swap (consumes forward_b via delta after settle) →
    V4_SETTLE_DELTA for remaining currencies.
    """
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE

    forward_a_idx = at.add(
        ha.currency1_address if ha.zfo else ha.currency0_address
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
    inner += enc_v4_take_compact(forward_a_idx, at.add(hb.pool_address), out_a)
    inner += enc_v2_swap_calc(at.add(hb.pool_address), hb.zfo, executor_idx, fee=hb.fee)
    inner += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    inner += enc_v4_settle_all()

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V3-V2 ──────────────────────────────────────────────────────────────


def _3hop_v4_v3_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """V4_TAKE USDC→V3b (IIA ✓), V3b→V2c + WETH→V3b."""
    ha, hb, hc = path_info.hops
    out_a, _out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3b_idx = at.add(hb.pool_address)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency1_address if ha.zfo else ha.currency0_address
    )
    # Forward from V3b
    _forward_b_idx = at.add(hb.token1_address if hb.zfo else hb.token0_address)

    # V3b callback: V4_TAKE USDC→V3b (IIA ✓) + V2c swap calc
    b_fwd = enc_v4_take_compact(forward_a_idx, v3b_idx, out_a)
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
    **kwargs,
) -> bytes | None:
    """V3c→V3b reverse + V4_TAKE USDC→V3b (IIA ✓), auto-settle WETH delta."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3b_idx = at.add(hb.pool_address)
    v3c_idx = at.add(hc.pool_address)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency1_address if ha.zfo else ha.currency0_address
    )

    # V3b callback: V4_TAKE USDC→V3b (IIA ✓ during callback)
    b_fwd = enc_v4_take_compact(forward_a_idx, v3b_idx, out_a)

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
    inner += enc_v4_settle_delta(weth_idx)

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V3-V4 ──────────────────────────────────────────────────────────────


def _3hop_v4_v3_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """V4a→V3b→V4c inside single unlock with V4_TAKE→V3b during callback.

    Per docs/pool-mechanics.md §4.1 (V4-V3-V4):
    V4a swap → V3b swap (recipient=PM for delta netting), V3b callback
    receives V4_TAKE(USDC→V3b) during V3b's callback (IIA ✓), then
    settle credits WBTC deposit, V4c swap consumes WBTC via delta,
    V4_TAKE WETH profit, settle remaining deltas.
    """
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    pm_idx = at.add(pool_manager_address)
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE
    v3b_idx = at.add(hb.pool_address)

    # Forward from V4a
    forward_a_idx = at.add(
        ha.currency1_address if ha.zfo else ha.currency0_address
    )
    # Forward from V3b
    forward_b_idx = at.add(hb.token1_address if hb.zfo else hb.token0_address)

    # V3b callback: V4_TAKE(USDC→V3b) during V3b's own callback (IIA ✓)
    b_fwd = enc_v4_take_compact(forward_a_idx, v3b_idx, out_a)

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
    inner += enc_v4_swap_compact(
        at.add(hc.currency0_address),
        at.add(hc.currency1_address),
        hc.fee,
        hc.tick_spacing,
        zero_idx,
        hc.zfo,
        out_b,
    )
    inner += enc_v4_settle_all()

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V4-V2 ──────────────────────────────────────────────────────────────


def _3hop_v4_v4_v2(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """Delta netting V4a↔V4b, V4_TAKE→V2c direct."""
    ha, hb, hc = path_info.hops
    out_a, out_b, out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE

    # Forward from V4b (= output of V4b swap)
    forward_b_idx = at.add(
        hb.currency1_address if hb.zfo else hb.currency0_address
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
    inner += enc_v4_swap_dynamic(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
    )
    inner += enc_v4_take_compact(forward_b_idx, at.add(hc.pool_address), out_b)
    inner += c_cmd
    inner += enc_v4_settle_all()

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V4-V3 ──────────────────────────────────────────────────────────────


def _3hop_v4_v4_v3(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    **kwargs,
) -> bytes | None:
    """Delta netting V4a↔V4b, V4_TAKE forward→exec, ERC20 forward→V3c (IIA ✓)."""
    ha, hb, hc = path_info.hops
    out_a, out_b, _out_c = hop_outputs
    if any(x <= 0 for x in hop_outputs):
        return None
    if not fits_int128(optimal_input):
        return None

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE

    # Forward from V4b
    forward_b_idx = at.add(
        hb.currency1_address if hb.zfo else hb.currency0_address
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
    inner += enc_v4_swap_dynamic(
        at.add(hb.currency0_address),
        at.add(hb.currency1_address),
        hb.fee,
        hb.tick_spacing,
        zero_idx,
        hb.zfo,
    )
    inner += enc_v4_take_compact(forward_b_idx, executor_idx, out_b)
    inner += enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, forward_data=c_pay)
    inner += enc_v4_settle_all()

    return enc_preamble(at) + enc_v4_unlock(inner)


# ── V4-V4-V4 ──────────────────────────────────────────────────────────────


def _3hop_v4_v4_v4(
    path_info: PathInfo,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    executor_address: str,
    pool_manager_address: str,
    weth_address: str,
    *,
    bribe_bips: int = 0,
    erc6909_profit: bool = False,
    use_v4_batch: bool = False,
) -> bytes | None:
    """Pure delta netting with V4_MINT or V4_TAKE profit capture.

    Per user-guide §12.27: all 3 V4 swaps inside unlock, intermediate
    deltas cancel, only net WETH profit is taken (or minted as ERC6909).

    With V4_MINT_COMPACT: 0 transfers (profit as ERC6909 accounting entry).
    With V4_TAKE_DELTA: 1 transfer (PM sends profit WETH to executor).
    With V4_BATCH: single PM extcall (gas savings, slightly more calldata).
    """
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

    at = AddressTable(
            weth_address=weth_address,
            executor_address=executor_address,
            pool_manager_address=pool_manager_address,
        )
    weth_idx = SENTINEL_WETH
    executor_idx = SENTINEL_SELF
    zero_idx = SENTINEL_NATIVE

    # Determine output (profit) currency of the final V4 swap
    output_currency_c = hc.currency1_address if hc.zfo else hc.currency0_address

    if use_v4_batch:
        # V4_BATCH: single PM extcall, auto-settles WETH/native ETH deltas.
        # More calldata per swap (20 bytes entry vs 9/21 individual) but
        # saves ~5K gas per swap from fewer extcalls.
        inner = enc_v4_batch([
            (
                at.add(ha.currency0_address),
                at.add(ha.currency1_address),
                ha.fee, ha.tick_spacing,
                zero_idx, ha.zfo, optimal_input,
            ),
            (
                at.add(hb.currency0_address),
                at.add(hb.currency1_address),
                hb.fee, hb.tick_spacing,
                zero_idx, hb.zfo, 0,  # dynamic amount from delta
            ),
            (
                at.add(hc.currency0_address),
                at.add(hc.currency1_address),
                hc.fee, hc.tick_spacing,
                zero_idx, hc.zfo, 0,  # dynamic amount from delta
            ),
        ])
        # V4_BATCH auto-settles WETH + native ETH deltas.
        # For ERC-20 profit, still need explicit take.
        if output_currency_c not in (NATIVE_CURRENCY_ADDRESS, weth_address):
            profit_idx = at.add(output_currency_c)
            inner += enc_v4_take_delta(profit_idx, executor_idx)
    else:
        # Individual swap commands — more calldata-efficient for small counts
        inner = enc_v4_swap_compact(
            at.add(ha.currency0_address),
            at.add(ha.currency1_address),
            ha.fee,
            ha.tick_spacing,
            zero_idx,
            ha.zfo,
            optimal_input,
        )
        inner += enc_v4_swap_dynamic(
            at.add(hb.currency0_address),
            at.add(hb.currency1_address),
            hb.fee,
            hb.tick_spacing,
            zero_idx,
            hb.zfo,
        )
        inner += enc_v4_swap_dynamic(
            at.add(hc.currency0_address),
            at.add(hc.currency1_address),
            hc.fee,
            hc.tick_spacing,
            zero_idx,
            hc.zfo,
        )

    # Profit capture: V4_MINT_COMPACT (ERC6909) or V4_TAKE_DELTA (physical)
    if erc6909_profit and output_currency_c == weth_address:
        # V4_MINT_COMPACT: profit as ERC6909, no physical transfer.
        # Saves ~20K gas. Requires check_mode=2 on expected_balance.
        profit_amount = out_c - optimal_input
        if profit_amount > 0:
            inner += enc_v4_mint_compact(weth_idx, executor_idx, profit_amount)
    elif not use_v4_batch:
        # V4_TAKE_DELTA: reads actual positive delta from PM exttload.
        # When use_v4_batch, V4_BATCH already handles WETH/native ETH.
        if output_currency_c == NATIVE_CURRENCY_ADDRESS:
            native_idx = at.add(NATIVE_CURRENCY_ADDRESS)
            inner += enc_v4_take_delta(native_idx, executor_idx)
        elif output_currency_c == weth_address:
            inner += enc_v4_take_delta(weth_idx, executor_idx)
        else:
            profit_idx = at.add(output_currency_c)
            inner += enc_v4_take_delta(profit_idx, executor_idx)

    # Auto-settle all remaining nonzero deltas (handles rounding residuals)
    inner += enc_v4_settle_all()

    return enc_preamble(at, bribe_bips=bribe_bips) + enc_v4_unlock(inner)


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
