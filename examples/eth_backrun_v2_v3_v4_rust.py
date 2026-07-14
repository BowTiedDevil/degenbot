"""Ethereum mainnet backrun bot: Uniswap V2/V3/V4 arbitrage using the Rust engine.

A thin Python orchestration layer over the Rust-owned UniswapArbEngine.
The Rust engine owns all pool state and path solving. Python does: pool
construction, swap encoding, simulation, and transaction submission.

The executor contract (cmd_executor.vy) uses a compact command-stream
format with 1-byte opcodes and tightly-packed parameters. Address indices
reference a shared address table with sentinel support for common addresses
(WETH, PoolManager, executor). V4 swaps execute inside a
single unlock context with automatic delta settlement. Profit check and
bribes are passed via the packed ``config`` parameter to execute()
(layout: expected_value<<32 | bribe_recipient_idx<<24 | bribe_bips<<8 | check_mode;
config=0 skips the check and sends no bribe).

The contract asserts combined WETH + ETH balance does not decrease (profit =
increase). No WETH prefunding is required — the first pool's callback is the
flash borrow.

Startup sequence:
1. Subscribe to WS (event buffering begins)
2. Load DB snapshots (V3 + V4 tick data)
3. Backfill snapshot→WS gap via Rust engine
4. Resume pump (Rust owns all event processing from here)
5. Start result consumer task (rolling start)
6. build_paths() (paths eagerly solved, results dispatched concurrently)
7. Consumer task continues as the permanent main loop
"""

import argparse
import asyncio
import contextlib
import datetime
import gc
import itertools
import json
import operator
import os
import pathlib
import signal
import time
import traceback
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any, cast

import dotenv
from eth_backrun_helpers import (
    BackrunConfig,
    format_failure_breakdown,
)
from eth_typing import ChainId, ChecksumAddress

from degenbot import Bot, UniswapV2Pool, UniswapV3Pool, degenbot_rs, get_checksum_address
from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.arbitrage.hop_info import HopInfo, V2HopInfo, V3HopInfo, V4HopInfo
from degenbot.arbitrage.verification_retry import (
    VerificationRetryPolicy,
    retry_verification_call,
    retry_verification_call_async,
)
from degenbot.calculations.evm_math import next_base_fee
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.database.models.pools import (
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTable,
    UniswapV4PoolTableBase,
)
from degenbot.degenbot_rs import (
    AlloyProvider,
    AsyncAlloyProvider,
    DynamicFeePoolRejectedError,
    HookedPoolRejectedError,
    PyDispatchCandidate,
    PyDispatchOutcome,
    PyDispatcher,
    PySimulateContext,
    PySubmitCandidate,
    PyTxSigner,
    VerificationMismatchError,
    VerificationRpcError,
    dispatch_and_submit_py,
    dispatch_profitable_py,
    fetch_fee_history_py,
)
from degenbot.logging import logger as bot_logger
from degenbot.pathfinding import find_paths_async
from degenbot.provider import AsyncAlloyProvider
from degenbot.provider import AlloyProvider
from degenbot.uniswap.deployments import EthereumMainnetUniswapV4
from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS, UniswapV4Pool
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot

# ──────────────────────────────────────────────────────────────────
# Configuration
# ──────────────────────────────────────────────────────────────────

WETH_ADDRESS = WRAPPED_NATIVE_TOKENS[ChainId.ETH]
MULTICALL3_ADDRESS = "0xcA11bde05977b3631167028862bE2a173976CA11"

# Verified standard ERC-20 intermediates for Ethereum mainnet.
# Every token here is confirmed to have NO transfer fees and NO rebase.
ETH_MAINNET_ALLOWED_TOKENS: set[str] = {
    "0x163f8C2467924be0ae7B5347228CABF260318753",  # WLD
    "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",  # PyUSD
    "0xB8c77482e45F1F44dE1745F52C74426C631bDD52",  # BNB
    "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84",  # LIDO stETH
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",  # WETH
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
}

MIN_PROFIT_NET = 1  # was 5 * 10**9 (5 gwei)
# T3 (GTOD23-IKJRGO): pre-sim profit-margin floor in bps of optimal_input.
# S1 found the dominant IIA reverts are sub-0.2-bps-margin arb the chain has
# already arbitraged away. 50 bps (0.5%) drops the bulk without losing genuine
# arbs. 0 disables. Env-configurable for tuning without code changes.
MIN_PROFIT_MARGIN_BPS = int(os.environ.get("DEGENBOT_MIN_PROFIT_MARGIN_BPS", "50"))
FEE_HISTORY_WINDOW = 10
FEE_PERCENTILES = (10, 50)
TARGET_PROFIT_RATIO = 1.25
BLOCKS_BEFORE_NONCE_EXPIRES = 5
MAX_SIMULATE_CONCURRENT = 50  # Cap concurrent simulation RPC calls (Slice 1)
AGE_DECAY_CONSTANT = 0.25  # Priority fee age decay factor (Slice 3)
MIN_PRIORITY_FEE_PERCENTILE = 10  # Use Nth percentile from feeHistory as floor (Slice 3)
MAX_PRIORITY_FEE_PERCENTILE = 50  # Use Nth percentile from feeHistory as ceiling (Slice 3)

# ── Path simulation failure suppression ──────────────────────────
# After a path fails simulation N consecutive times, it is
# "suppressed" — excluded from the candidate list so the simulation
# budget goes to paths with a real chance of succeeding. Suppressed
# paths are retried periodically in case conditions change.

# ── Path permutation filter ─────────────────────────────────────
# Only build paths matching these pool-version permutations.
# Set to None (or empty) to allow all permutations.
# Example: {"V3-V3-V4", "V3-V4-V3"} to debug specific orderings.
# Integrated at the pathfinding level: only the required pool types are
# loaded into the graph, and DFS edges are pruned at each hop, so
# discovery is fast and no post-filtering is needed.
#
# When set, pool_types is automatically derived from the version tags
# in the filter. E.g. {"V2-V3-V4"} loads all V2/V3/V4 table types into
# the graph and prunes to V2 at depth 0, V3 at depth 1, V4 at depth 2.
# When None, all V2/V3/V4 table types are loaded (no pruning).
PATH_PERMUTATION_FILTER: set[str] | None = None  # e.g. {"V3-V4-V3"}

# ── Intermediate token whitelist ────────────────────────────────
# Only build paths where intermediate hops use these tokens.
# Set to None to allow all tokens (default). When set, pools that
# connect any non-whitelisted token are excluded from the graph,
# eliminating tax/fee-on-transfer tokens that would waste simulation
# gas and always revert.
#
# All tokens below are verified standard ERC-20 (no transfer fees,
# no rebase mechanics). Do NOT add tokens without verifying:
#   - stETH: rebase token (balance changes without transfers) — use wstETH instead
#   - AMPL: rebase token — exclude
#   - HEX: origin fee on transfer — exclude
ALLOWED_INTERMEDIATE_TOKENS: set[str] | None = ETH_MAINNET_ALLOWED_TOKENS


# Mapping from short version tag to the DB table base class(es) that
# represent that pool family.
def _concrete_pool_types(base_type: type) -> list[type]:
    """Expand an abstract pool table base into its concrete subclasses."""
    if not getattr(base_type, "__abstract__", False):
        return [base_type]
    subs = base_type.__subclasses__()
    if not subs:
        return [base_type]
    result: list[type] = []
    for s in subs:
        result.extend(_concrete_pool_types(s))
    return result


_POOL_VERSION_MAP: dict[str, list[type]] = {
    "V2": _concrete_pool_types(UniswapV2PoolTableBase),
    "V3": _concrete_pool_types(UniswapV3PoolTableBase),
    "V4": [UniswapV4PoolTable],
}


def _parse_permutation_filter(
    perms: set[str] | None,
) -> list[set[type] | None] | None:
    """Convert a set of permutation strings like {'V3-V4-V3'} into a
    pool_type_per_depth list suitable for find_paths_async.

    Returns None if perms is None/empty (no filter).
    Returns a list of sets, one per depth, where each set contains the
    allowed pool table types at that depth. If all permutations agree
    that any type is allowed at a depth, that entry is None.
    """
    if not perms:
        return None
    # Parse each permutation string into list of version tags
    parsed: list[list[str]] = []
    for perm in perms:
        parts = perm.split("-")
        if not all(p in _POOL_VERSION_MAP for p in parts):
            msg = f"Invalid permutation '{perm}': unknown version tag"
            raise ValueError(msg)
        parsed.append(parts)
    # All permutations must have the same depth
    if len({len(p) for p in parsed}) != 1:
        msg = f"All permutations must have the same depth, got: {perms}"
        raise ValueError(msg)
    max_depth = len(parsed[0])
    # Build per-depth allowed types
    result: list[set[type] | None] = []
    for depth in range(max_depth):
        allowed_this_depth: set[type] = set()
        for perm_parts in parsed:
            allowed_this_depth.update(_POOL_VERSION_MAP[perm_parts[depth]])
        result.append(allowed_this_depth or None)
    return result


def _pool_types_from_filter(perms: set[str] | None) -> list[type]:
    """Derive the pool_types list from the permutation filter.

    When a permutation filter is set, only include pool table types for
    the version tags mentioned in the permutations. When the filter is
    None/empty, include all V2/V3/V4 types so every permutation is
    discoverable.

    This ensures the graph contains the right pool tables for the
    requested permutations, regardless of which version tags appear.
    """
    if not perms:
        # No filter — include all pool types for maximum coverage
        types: set[type] = set()
        for version_types in _POOL_VERSION_MAP.values():
            types.update(version_types)
        return list(types)

    # Only include types for versions mentioned in the filter
    versions_needed: set[str] = set()
    for perm in perms:
        versions_needed.update(perm.split("-"))

    types = set()
    for version in versions_needed:
        types.update(_POOL_VERSION_MAP[version])
    return list(types)


# Number of consecutive sim-failures before a path is suppressed.
PATH_SUPPRESS_THRESHOLD = 10

# Cap on per-batch `[sim-fail]` lines emitted by `_render_sim_failures`. A
# thin-margin revert storm can otherwise flood the log during a stalled BP —
# the remainder is summarized as a single `… (+N more)` trailing line.
_SIM_FAIL_RENDER_CAP = 25

# How many blocks between retry attempts for suppressed paths.
PATH_SUPPRESS_RETRY_INTERVAL = 100

# T7 (ADR-006 D4): how many blocks between recurring liquidity-map verifies
# in the hot loop. Default 50 bounds RPC cost; a post-release / in-loop desync
# (e.g. the V3 unregister bug at 3ae6fa04) is caught within this window.
RECURRING_VERIFY_INTERVAL = 50

# ── Executor code injection via eth_simulateV1 ──────────────────
# When INJECT_EXECUTOR_CODE=True, we inject the cmd_executor
# runtime bytecode at a fresh address via stateOverrides.code.
# This lets us test the new V2/V3/V4-capable executor contract
# WITHOUT deploying it on mainnet first.
# The runtime bytecode must have immutables (OWNER_ADDR, WETH_ADDR,
# POOL_MANAGER_ADDR, plus 2 precomputed delta slots for WETH and NATIVE)
# already baked
# in — see contracts/cmd_executor_runtime_bytecode.txt.
#
# IMPORTANT: the runtime bytecode must have Vyper CBOR metadata
# intact and immutables appended AFTER it. Vyper's CODECOPY offsets
# assume the deployed layout [code][CBOR][immutables]; removing
# the CBOR breaks the jump table, JUMPDEST targets, and immutable
# reads. The recompile.py script handles this automatically.
#
# All override fields (code, balance, nonce, state, stateDiff) are
# passed under stateOverrides per the Alloy AccountOverride spec.
# web3.py's simulate_v1 formatter hex-encodes balance/code values.
#
# eth_simulateV1 calls within a blockStateCalls group chain their
# state changes sequentially, so the 7-call pattern (WETH balanceOf + ETH
# → execute(commands) → balanceOf after) correctly measures profit
# without needing WETH storage overrides or prefunding.
INJECT_EXECUTOR_CODE = os.environ.get("INJECT_EXECUTOR_CODE", "1") == "1"
STATE_DUMP_ON_REVERT = os.environ.get("STATE_DUMP_ON_REVERT", "0") == "1"
STATE_DUMP_DIR = Path(os.environ.get("STATE_DUMP_DIR", "logs/state_dumps"))
INJECTED_EXECUTOR_ADDRESS = get_checksum_address(
    os.environ.get(
        "INJECTED_EXECUTOR_ADDRESS",
        "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1",
    ),
)

UNISWAP_V3_MAINNET_FACTORY = "0x33128a8fC17869897dcE68Ed026d694621f6FDfD"
SUSHISWAP_V3_MAINNET_FACTORY = "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
PANCAKESWAP_V3_MAINNET_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"

# V4 PoolManager on Ethereum mainnet
UNISWAP_V4_POOL_MANAGER_ADDRESS = get_checksum_address("0x000000000004444c5dc75cB358380D2e3De08A90")

# V4 pool admission (amount-modifying hooks / dynamic fees) is enforced by
# the Rust core (BotState::register_v4_pool) as a correctness floor, surfaced
# as typed HookedPoolRejectedError / DynamicFeePoolRejectedError — no
# Python-side pre-check.

# V3 sqrt price limits
MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342

# Executor contract — cmd_executor.vy (V2+V3+V4 command stream)
# Supports V2/V3/V4 callbacks and bribes — all via compact command stream encoding.
# NOTE: The default address (0x543C7e...) is the OLD V3-only executor.
# Use code injection (INJECT_EXECUTOR_CODE=1, the default) to test
# against the new cmd_executor runtime bytecode.
EXECUTOR_ADDRESS = os.environ.get(
    "EXECUTOR_CONTRACT_ADDRESS",
    "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",  # OLD — update after deployment
)
EXECUTOR_OWNER = os.environ.get(
    "EXECUTOR_OWNER_ADDRESS",
    "0x9C56a29c7231974c269E24F9FB3c29203039089E",  # Throwaway — override with real key at runtime
)
EXECUTOR_ABI = [
    # execute(commands, config) — cmd_executor
    {
        "stateMutability": "payable",
        "type": "function",
        "name": "execute",
        "inputs": [
            {"name": "commands", "type": "bytes"},
            {"name": "config", "type": "uint256"},
        ],
        "outputs": [
            {"name": "", "type": "uint256"},
        ],
    },
]


# ──────────────────────────────────────────────────────────────────
# Executor code injection helpers
# ──────────────────────────────────────────────────────────────────

# Cached runtime bytecode (loaded once, reused across all simulations)
_runtime_bytecode_cache: str | None = None


def _load_executor_runtime_bytecode() -> str:
    """Load the patched runtime bytecode from contracts/ directory.

    The bytecode has all 5 immutable slots baked in: OWNER_ADDR, WETH_ADDR,
    POOL_MANAGER_ADDR, and 2 precomputed delta slots (WETH, NATIVE).
    See contracts/recompile.py for the full layout.
    """
    global _runtime_bytecode_cache
    if _runtime_bytecode_cache is not None:
        return _runtime_bytecode_cache

    bytecode_path = (
        pathlib.Path(pathlib.Path(pathlib.Path(__file__).resolve()).parent).parent
        / "contracts"
        / "cmd_executor_runtime_bytecode.txt"
    )
    code = pathlib.Path(bytecode_path).read_text(encoding="utf-8").strip()
    if not code.startswith("0x"):
        msg = f"Runtime bytecode file must start with 0x, got: {code[:20]}..."
        raise ValueError(msg)
    _runtime_bytecode_cache = code
    bot_logger.info(
        f"[inject] Loaded executor runtime bytecode: "
        f"{len(code) // 2 - 1} bytes from {bytecode_path}",
    )
    return _runtime_bytecode_cache


def _hop_display_addr(hop: HopInfo) -> str:
    """Return a short display address for logging."""
    if isinstance(hop, V2HopInfo):
        return hop.pool_address
    if isinstance(hop, V3HopInfo):
        return hop.pool_address
    return hop.pool_id_hex


def _hop_token_summary(hops: list[HopInfo] | tuple[HopInfo, ...]) -> str:
    """One-line summary of hop input→output tokens for sim-fail diagnostics."""
    parts: list[str] = []
    for h in hops:
        if isinstance(h, (V2HopInfo, V3HopInfo)):
            t0, t1 = h.token0_address, h.token1_address
        else:
            t0, t1 = h.currency0_address, h.currency1_address
        parts.append(f"{t0}→{t1}{'↗' if h.zfo else '↘'}")
    return " ".join(parts)


def _make_backrun_config(node_http: str) -> DegenbotConfig:
    """Build a single-chain DegenbotConfig for the backrun session (ADR-006 D5).

    The chain identity is Ethereum mainnet (1); the RPC is the caller's
    ``node_http`` — the cascade-resolved endpoint from
    :func:`degenbot.config.resolve_rpc_uris` (CLI > OS env
    ``DEGENBOT_RPC_HTTP_CHAINID_1`` > legacy ``NODE_HOST_*`` > config.toml
    ``rpc[1]``). When config.toml was the winning source, ``node_http`` already
    equals ``rpc[1]``, so the injection here is consistent rather than a bypass.
    The Bot enforces the connected RPC's ``eth_chainId`` matches at construction.

    The database path is read from the existing user config at
    ``~/.config/degenbot/config.toml`` (so locally-configured DB paths are
    honored) and falls back to the default path if no config exists.
    """
    from degenbot.config import CONFIG_FILE, load_config_from_file

    if CONFIG_FILE.exists():
        base = load_config_from_file(CONFIG_FILE)
        # Override the RPC with the env-derived endpoint while keeping
        # the database path (and any other settings) from the config file.
        return DegenbotConfig(
            database=base.database,
            rpc={1: cast("Any", node_http)},
            default_chain_id=1,
        )

    return DegenbotConfig(
        database=DatabaseSettings(path=Path("~/.config/degenbot/degenbot.db").expanduser()),
        rpc={1: cast("Any", node_http)},
        default_chain_id=1,
    )


# ──────────────────────────────────────────────────────────────────
# Direction resolver
# ──────────────────────────────────────────────────────────────────


def resolve_directions(
    pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool],
    input_token_address: str,
) -> list[bool] | None:
    """Determine zero_for_one for each hop so the cycle closes.

    The cycle: input_token → hop_0 → intermediate → hop_1 → ... → input_token
    Returns a list of zfo values (one per hop), or None if the cycle
    cannot close (token mismatch).

    V4 pools use NATIVE_CURRENCY_ADDRESS (address(0)) for ETH. For direction
    resolution, we treat NATIVE_CURRENCY_ADDRESS as equivalent to WETH —
    since our profit token is always WETH.
    """
    addr = get_checksum_address(input_token_address)
    zfo_list: list[bool] = []

    for pool in pools:
        token0_addr = get_checksum_address(pool.token0.address)
        token1_addr = get_checksum_address(pool.token1.address)

        # V4: treat NATIVE_CURRENCY_ADDRESS as WETH for matching
        if token0_addr == NATIVE_CURRENCY_ADDRESS:
            token0_addr = WETH_ADDRESS
        if token1_addr == NATIVE_CURRENCY_ADDRESS:
            token1_addr = WETH_ADDRESS

        if token0_addr == addr:
            zfo = True  # selling token0 (input) for token1
        elif token1_addr == addr:
            zfo = False  # selling token1 (input) for token0
        else:
            return None

        # Intermediate token comes out of this hop
        addr = token1_addr if zfo else token0_addr
        zfo_list.append(zfo)

    # Verify the cycle closes: last output must be the input token
    if addr != get_checksum_address(input_token_address):
        return None

    return zfo_list


class BackrunSession:
    """Orchestrator that collapses the backrun startup ritual behind one facade.

    Owns the config + the three actors (``bot``, ``engine_registry``, ``async_w3``)
    + the ``Dispatcher`` + scalar block state, and is the ONE place that
    enforces the phase ordering the engine's state machine requires:

        start():  subscribe → stream snapshots → backfill → verify config
                  (``EngineRegistry.start``, stops at Backfilled, pre-resume)
        run():    attach consumer → ``resume()`` → ``build_paths`` →
                  ``bot.release_python_state()`` → drop bot → main loop

    Usage (production)::

        cfg = BackrunConfig.from_env(
            dotenv_values("examples/mainnet.env"), live=not dry_run, permutation=args.permutation
        )
        async with BackrunSession(cfg) as session:
            await session.run()

    The early release inside ``run()`` (drop the bot before the main loop) is
    preserved: the hot loop keeps only ``engine_registry`` + ``async_w3`` +
    dispatcher — the Python pool/token caches are scaffolding once the Rust
    engine owns canonical state.

    Testability seams (mirrors ``EngineRegistry``'s ``engine=`` seam): ``bot``,
    ``engine_registry``, ``async_w3``, ``snapshots``, ``path_builder``, and
    ``consumer`` are injectable. When injected, ``start()``/``run()``
    orchestrate the fakes and the phase ordering is verifiable offline; when
    ``None`` (production), the actors are built from ``cfg`` and the real
    module functions are called.
    """

    def __init__(
        self,
        cfg: BackrunConfig,
        *,
        bot: Bot | None = None,
        engine_registry: EngineRegistry | None = None,
        async_w3: AsyncAlloyProvider | None = None,
        snapshots: tuple[Any, Any, Any, Any] | None = None,
        path_builder: Any = None,
        consumer: Any = None,
        install_sigint: bool = True,
    ) -> None:
        self.cfg = cfg
        self._injected_bot = bot
        self._injected_engine_registry = engine_registry
        self._injected_async_w3 = async_w3
        self._injected_snapshots = snapshots
        self._path_builder = path_builder
        self._consumer = consumer
        # Resolved in start():
        self.bot: Bot | None = None
        self.engine_registry: EngineRegistry | None = None
        self.async_w3: AsyncAlloyProvider | None = None
        self.dispatcher: PyDispatcher | None = None
        self._sim_ctx: PySimulateContext | None = None
        self.v3_snapshot: Any = None
        self.v4_snapshot: Any = None
        self.current_block: int = 0
        self._started = False
        # Created in run():
        self._result_consumer_task: asyncio.Task | None = None
        # SIGINT handler installed by `start()`, restored by `__aexit__`.
        # Stores the previous handler so teardown restores it (the default
        # SIGINT → KeyboardInterrupt machinery) rather than leaving a
        # process-wide handler bound after the session ends.
        self._previous_sigint_handler: object = signal.SIG_DFL
        self._sigint_installed = False
        # Production (main()) installs the SIGINT→engine.stop() handler so a
        # Ctrl-C during the synchronous find_paths section stops the pump
        # immediately. Tests pass install_sigint=False to avoid binding a
        # process-global handler (signal.signal pollutes across tests).
        self._install_sigint = install_sigint

    # ── Phase A: pre-resume startup ─────────────────────────────────
    async def start(self) -> "BackrunSession":
        """Build the actors, fetch block state, load snapshots, run ``engine_registry.start()``.

        Stops at ``Backfilled`` — BEFORE ``resume()``. Zero result batches
        emit during this window (the pump isn't running), so ``run()`` can
        attach the consumer in the gap before ``resume()`` without a stale-backlog
        window. Idempotent guard via ``_started``.
        """
        if self._started:
            return self
        self._started = True

        cfg = self.cfg

        # ── Build the three actors (injected or from cfg) ──
        self.bot = self._injected_bot or self._build_bot(cfg)
        self.async_w3 = self._injected_async_w3 or await self._build_async_w3(cfg)
        self.engine_registry = self._injected_engine_registry or EngineRegistry(bot=self.bot)

        # ── Fetch current block (for the dispatcher + backfill comparison) ──
        # Note: main()'s start-phase base_fee_next/operator_nonce fetches were
        # dead state (recomputed per-batch inside consume_result_batches) — dropped.
        latest_block = await self.async_w3.get_block("latest")
        self.current_block = latest_block["number"]

        # ── Coordination state ──
        self.dispatcher = PyDispatcher.for_block(self.current_block)

        # ── Simulation seam context (A5) — one PySimulateContext per session,
        # held alongside the dispatcher. The runtime-bytecode file-load stays
        # Python (A2 disposition `stays-python`); the bytes cross here. The
        # AsyncAlloyProvider handle is taken from the session's provider so
        # `dispatch_profitable_py` shares one provider with the rest of the
        # pipeline.
        async_alloy = self.async_w3.as_async_alloy()
        if async_alloy is None:
            # Non-Alloy provider (test fakes). Defer the sim context:
            # production sessions are Alloy-backed + build it eagerly here;
            # dispatch raises a clear error if reached without one.
            self._sim_ctx = None
        else:
            runtime_code = _load_executor_runtime_bytecode()
            self._sim_ctx = PySimulateContext(
                provider=async_alloy,
                executor_owner=cfg.operator_address,
                executor_address=cfg.executor_address,
                weth_address=WETH_ADDRESS,
                pool_manager_address=UNISWAP_V4_POOL_MANAGER_ADDRESS,
                multicall3_address=MULTICALL3_ADDRESS,
                inject_code=INJECT_EXECUTOR_CODE,
                executor_runtime_bytecode=bytes.fromhex(runtime_code[2:]),
                injected_address=INJECTED_EXECUTOR_ADDRESS if INJECT_EXECUTOR_CODE else None,
            )

        # ── Snapshots (V3 pool tracker pre-population only; the engine's DB
        # snapshot is loaded eagerly at PyBot construction via
        # `Bot::load_snapshot_from_db` — JUCFCB, Shape 2 — and the
        # snapshot→WS gap closes in `resume_from_subscribe` — J3FMDO).
        # `engine_registry.start()` takes `v3_snapshot`/`v4_snapshot` kwargs
        # ONLY when the snapshots are non-DB (file/memory) — the `_injected`
        # fast path. The production DB path reads the snapshot at
        # construction and `start()` takes no snapshot kwargs (the retired
        # DB-snapshot `stream_*_to_engine` SQLAlchemy forwarding is gone —
        # JUCFCB/2SM4Y7).
        v3_snap: Any = None
        v4_snap: Any = None
        start_v3 = None  # snapshots passed to `start()` (non-DB only)
        start_v4 = None
        if self._injected_snapshots is not None:
            v3_snap, v4_snap, _v3_blk, _v4_blk = self._injected_snapshots
            start_v3, start_v4 = v3_snap, v4_snap
        else:
            # Production DB path: snapshot for the V3 pool tracker only
            # (engine feeds from the core store, set at Bot construction).
            v3_snap, v4_snap, _v3_blk, _v4_blk = get_snapshots(self.bot)
        self.v3_snapshot = v3_snap
        self.v4_snapshot = v4_snap

        # ── Engine pre-resume ritual (subscribe → verify) ──
        # J3FMDO: the snapshot→WS gap is closed automatically inside
        # `BlockPump::resume_from_subscribe` at resume. `start()` only
        # subscribes + sets up verify config; resume drives both the backfill
        # and the live loop. Non-DB snapshots flow through `load_*_from_py`
        # in `start()`; the DB path takes no kwargs (snapshot loaded at
        # construction; `snapshot_seed_block` is read from the core
        # `BotState` by `start()` via the `snapshot_seed_block` getter).
        backfill_target = self.engine_registry.start(
            cfg.node_http,
            cfg.node_ws,
            v3_snapshot=start_v3,
            v4_snapshot=start_v4,
            verify_state_view=EthereumMainnetUniswapV4.state_view.address,
        )
        if backfill_target > self.current_block:
            self.current_block = backfill_target
            self.dispatcher.advance_block(backfill_target)

        self._install_sigint_handler()
        return self

    # ── Phase B: the rolling-start main loop ──────────────────────────
    async def run(self) -> None:
        """Attach the consumer, resume the pump, build paths, release, then run the main loop.

        Ordering (the invariant this session enforces):
        1. create the consumer task (BEFORE resume — closes the stale-backlog window)
        2. ``engine_registry.engine.resume()`` (the single gate after which batches flow)
        3. ``await build_paths(...)`` (rolling start: eager solves dispatch as fresh blocks roll in)
        4. ``bot.release_python_state()`` + drop the bot (hot loop keeps only engine + async_w3)
        5. ``await result_consumer_task`` (the main loop, indefinite)
        """
        assert self._started, "BackrunSession.start() must be awaited before run()"
        assert self.engine_registry is not None
        assert self.async_w3 is not None
        assert self.bot is not None
        assert self.dispatcher is not None

        cfg = self.cfg
        consumer = self._consumer or consume_result_batches

        # 1. Acquire the once-only block_stream EXACTLY ONCE and fan it to two
        # branches: the result consumer (full block dicts) + the recurring-
        # verify ticker (block numbers). The pump's `engine.block_stream()`
        # moves the mpsc receiver out of a Mutex on each call — a second call
        # raises RuntimeError("block_stream() can only be called once"), which
        # previously crashed entering the main loop (the consumer self-acquired
        # one, run() acquired another for recurring-verify). See
        # `_tee_block_stream` for the full rationale + regression.
        _block_stream = self.engine_registry.engine.block_stream()
        _consumer_branch, _verify_branch, _tee_driver = _tee_block_stream(_block_stream)

        # Attach the consumer BEFORE resume (consumer-safety invariant).
        self._result_consumer_task = asyncio.create_task(
            consumer(
                engine_registry=self.engine_registry,
                async_w3=self.async_w3,
                sim_ctx=self._sim_ctx,
                executor_address=cfg.executor_address,
                operator_address=cfg.operator_address,
                operator_private_key=cfg.operator_private_key,
                dispatcher=self.dispatcher,
                dry_run=cfg.dry_run,
                block_stream=_consumer_branch,
            ),
            name="result-consumer",
        )

        # 2. Resume the pump — the single gate after which result batches flow.
        self.engine_registry.engine.resume()

        # 3. Build paths with the pump live (rolling start).
        path_builder = self._path_builder or build_paths
        await path_builder(
            bot=self.bot,
            engine_registry=self.engine_registry,
            v3_snapshot=self.v3_snapshot,
            v4_snapshot=self.v4_snapshot,
            retry_policy=cfg.verification_retry_policy,
        )

        # 3b. STARTUP batch verify REMOVED — redundant with the per-pool two-step
        # verify and racy at the moving head. Step-1 (seed @ snapshot block) runs
        # inside build_paths for each Tracked pool and proves the snapshot was
        # good; step-2 (post-drain @ backfill block) proves the drain/pump
        # applied buffered events correctly. Re-verifying the whole batch at
        # `last_processed_block()` (the live head) re-checked what step-1/step-2
        # just verified AND raced the pump's WS log-application lag: a block's
        # header can advance `last_processed_block()` past it before its Mint
        # log is dispatched (V2-V2-V3 crash — Mint at 25397047 unapplied when
        # 25397049's header advanced the cursor, false-mismatching tick
        # -887270). The per-pool gates are race-free (frozen-block pin); the
        # T7 recurring-verify carries in-loop drift detection. The analyzer
        # now keys `verify_basis` on the per-pool `[verify-seed]`/`[verify-drain]`
        # lines (see permutation_analyzer._VERIFY_OK_RE).

        # 4. Trim redundant Python state — Rust engine owns canonical pool state.
        self.bot.release_python_state()
        self.v3_snapshot = None
        self.v4_snapshot = None
        self.bot = None  # drop the only Python ref; engine keeps its own PyBot ref
        gc.collect()
        self._injected_bot = None  # release the injected ref too

        bot_logger.info(
            f"[startup] State trimmed — "
            f"{self.engine_registry.engine.v2_pool_count()} V2, "
            f"{self.engine_registry.engine.v3_pool_count()} V3, "
            f"{self.engine_registry.engine.v4_pool_count()} V4 pools retained in "
            f"Rust engine; {self.engine_registry.engine.path_count()} paths registered. "
            f"Entering main loop.",
        )

        # 5. Main loop — runs until the consumer task ends. A recurring verify
        # task (T7) runs alongside: every RECURRING_VERIFY_INTERVAL blocks it
        # re-checks liquidity maps so post-release / in-loop desyncs surface
        # instead of trading silently. Both complete together.
        assert self._result_consumer_task is not None
        recurring_verify = asyncio.ensure_future(
            run_recurring_verify_until_done(
                registry=self.engine_registry,
                block_ticker=(b["number"] async for b in _verify_branch),
                interval=RECURRING_VERIFY_INTERVAL,
                retry_policy=cfg.verification_retry_policy,
            ),
        )
        try:
            await self._result_consumer_task
        finally:
            recurring_verify.cancel()
            _tee_driver.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await recurring_verify
            with contextlib.suppress(asyncio.CancelledError):
                await _tee_driver

    # ── Actor builders (production path — only used when not injected) ──
    @staticmethod
    def _build_bot(cfg: BackrunConfig) -> Bot:
        config_obj = _make_backrun_config(cfg.node_http)
        # ADR-005: the Bot's build path (ERC20 + V2/V3/V4 pool construction)
        # issues many `eth_call`s via `PyBotIo` → `provider.call`. A web3.py
        # sync backend (`from_web3`) holds the GIL through every
        # `requests.post` on the event-loop thread, starving the asyncio loop
        # during `build_paths`. Use the Rust `AlloyProvider` instead —
        # `PyAlloyProvider.call` releases the GIL (`py.detach`) and does HTTP
        # in Rust, so the pump/consumer can proceed and RPC is faster. This
        # is the sync web3.py AlloyProvider being retired.
        alloy = AlloyProvider(cfg.node_http)
        return Bot(config_obj, provider=alloy)

    @staticmethod
    async def _build_async_w3(cfg: BackrunConfig) -> AsyncAlloyProvider:
        """Build the dispatch-path RPC provider (PAGQCK).

        Returns an ``AsyncAlloyProvider`` wrapping a Rust
        ``AsyncAlloyProvider`` — every dispatch-side ``eth_*`` call the hot
        loop makes goes through Rust (releasing the GIL), not raw
        ``AsyncWeb3(AsyncHTTPProvider(...))``. The four typed calls
        (``eth_simulateV1`` / ``eth_feeHistory`` / ``eth_createAccessList`` /
        ``eth_sendRawTransaction``) route via ``make_request`` on the alloy
        backend; the generic ones (``get_block`` / ``get_transaction_count`` /
        ``eth_call`` / ``get_code`` / ``get_transaction_receipt``) route via
        the adapter's typed methods.

        Returns:
            An ``AsyncAlloyProvider`` (alloy backend) for the dispatch path.

        """
        alloy = await AsyncAlloyProvider.create(cfg.node_http)
        return alloy

    # ── Async context manager ────────────────────────────────────────
    async def __aenter__(self) -> "BackrunSession":
        await self.start()
        return self

    def _install_sigint_handler(self) -> None:
        """Bind a SIGINT handler that stops the Rust pump *immediately*.

        The ``__aexit__`` → ``shutdown()`` → ``engine.stop()`` path only fires
        once the awaited coroutine unwinds — and during ``build_paths`` the
        main thread is blocked inside the synchronous ``find_paths`` graph
        prep / the Rust ``find_paths_rust`` DFS. Python's default SIGINT →
        raise ``KeyboardInterrupt`` mechanism is *deferred* until that section
        yields control to the eval loop, so the first Ctrl-C appeared to be
        swallowed: the pump (on the shared tokio runtime, a separate thread)
        kept running, the operator pressed Ctrl-C again, and only when
        ``find_paths`` finally returned did the deferred exception unwind to
        ``__aexit__`` and stop the pump.

        Installing this handler closes the gap: the moment SIGINT arrives,
        ``engine.stop()`` runs (it just sets the shutdown flag + aborts the
        pump task — cheap, GIL-only, idempotent) regardless of what the main
        thread is doing. The Rust ``find_paths_rust`` DFS releases the GIL via
        ``py.detach()``, so the handler *can* run even mid-DFS. We then
        re-raise so the normal ``KeyboardInterrupt`` unwind proceeds to
        ``__aexit__`` (which runs ``shutdown()`` again — a no-op — for the
        consumer cancellation).

        Idempotent: if already installed (or if ``signal`` can't bind — e.g. a
        non-main thread), it's a no-op so the call site in ``start()`` is safe
        to re-enter.
        """
        if self._sigint_installed or not self._install_sigint:
            return
        engine = self.engine_registry.engine if self.engine_registry is not None else None
        if engine is None:
            return
        try:
            self._previous_sigint_handler = signal.getsignal(signal.SIGINT)
        except ValueError:
            # `signal.signal` only works on the main thread; if start() is
            # ever driven off-thread there is nothing to bind — rely on
            # __aexit__'s shutdown() alone.
            return

        def _on_sigint(_signum: int, _frame: object) -> None:
            # Stop the pump first — fires even while the main thread is
            # blocked in find_paths (Rust DFS released the GIL). Wrapped
            # because the engine may have been torn down concurrently.
            try:
                engine.stop()
            except Exception:  # noqa: BLE001
                pass
            # Re-raise KeyboardInterrupt so the awaiting coroutine unwinds
            # through __aexit__ → shutdown() (idempotent) + consumer cancel.
            raise KeyboardInterrupt

        signal.signal(signal.SIGINT, _on_sigint)
        self._sigint_installed = True

    def _restore_sigint_handler(self) -> None:
        if not self._sigint_installed:
            return
        try:
            signal.signal(signal.SIGINT, cast("Any", self._previous_sigint_handler))
        except (ValueError, TypeError):
            pass
        self._sigint_installed = False

    async def __aexit__(self, *exc: object) -> None:
        """Best-effort cleanup; never suppresses.

        Signals the Rust pump to stop, then cancels the consumer task so no
        hanging background task outlives the session. ``shutdown()`` is
        best-effort: it swallows any error from the Rust ``stop()`` so a
        torn-down engine during a partial startup can't mask the original
        exception (the one this ``__aexit__`` is unwinding).

        Ordering rationale: the pump must be stopped BEFORE the consumer task
        is cancelled. The consumer awaits ``engine.__anext__()`` which blocks
        on the pump's result channel; cancelling the consumer first leaves the
        pump's WS task running on the shared tokio runtime, blocking process
        exit until the WS subscription closes itself (up to 60s on a silent
        stream). Stopping the pump first closes the channels → the consumer's
        next ``__anext__`` raises ``StopAsyncIteration`` → the consumer task
        ends cleanly, and the ``await task`` below returns without needing the
        ``CancelledError`` path in the common case.
        """
        await self.shutdown()
        self._restore_sigint_handler()
        task = self._result_consumer_task
        if task is not None and not task.done():
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await task

    async def shutdown(self) -> None:
        """Signal the Rust core to stop the pump (best-effort).

        Safe to call at any point in the lifecycle — before ``start()`` finished
        (``engine_registry`` may be ``None``), after ``run()`` exited, or from a
        ``SIGINT``/``KeyboardInterrupt`` handler. Mirrors the Rust ``stop()``
        contract: idempotent, sets the shutdown flag + aborts the pump task so
        the WS stream's ``combined.next().await`` unblocks immediately (60s
        cold-shutdown otherwise). Any exception is swallowed and logged so a
        partial-startup teardown can't mask the original in-flight exception.

        This is the one place that closes the Rust core's pump — the
        ``KeyboardInterrupt``-exits-slowly bug was the pump task (spawned on the
        shared tokio runtime, decoupled from the asyncio loop) blocking on a
        silent WS subscription, which ``asyncio.run``'s teardown did not reach
        until the OS closed the socket.
        """
        registry = getattr(self, "engine_registry", None)
        engine = getattr(registry, "engine", None) if registry is not None else None
        if engine is None:
            return
        try:
            engine.stop()
        except Exception as exc:  # noqa: BLE001 — best-effort teardown
            bot_logger.warning(f"[shutdown] engine.stop() failed: {exc!r}")


async def _shim_run_recurring_verify_until_done(
    *,
    registry: EngineRegistry,
    block_ticker: AsyncIterator[int],
    interval: int,
    retry_policy: VerificationRetryPolicy,
) -> None:
    """T7: delegate to the library recurring-verify (kept in
    degenbot.arbitrage.recurring_verify so tests can import it without the
    example's cmd_stream import chain).
    """
    from degenbot.arbitrage.recurring_verify import run_recurring_verify_until_done

    await run_recurring_verify_until_done(
        registry=registry,
        block_ticker=block_ticker,
        interval=interval,
        retry_policy=retry_policy,
    )


run_recurring_verify_until_done = _shim_run_recurring_verify_until_done


async def build_paths(
    *,
    bot: Bot,
    engine_registry: EngineRegistry,
    v3_snapshot: UniswapV3LiquiditySnapshot | None = None,
    v4_snapshot: UniswapV4LiquiditySnapshot | None = None,
    retry_policy: VerificationRetryPolicy | None = None,
) -> None:
    """Discover V2/V3/V4 arb paths, build Python pools, register with Rust engine.

    V4 pools are discovered via find_paths_async and built through
    bot.build_managed_pool(). V4 pool admission (amount-modifying hooks /
    dynamic fees) is enforced by the Rust core at registration time, surfacing
    as typed HookedPoolRejectedError / DynamicFeePoolRejectedError.

    Tick data for V3/V4 engine registration is resolved automatically from
    the core `SnapshotStore` (the DB snapshot loaded eagerly at `PyBot`
    construction via `Bot::load_snapshot_from_db`; `register_v3_pool`
    consumes it via `seed_from_store=True` when `coverage="tracked"` + no
    inline `tick_data`). The Rust engine applies buffered events on top of
    stale snapshot data to bring it current. Verification is handled
    internally by the engine (verify_on_register) — the tick data snapshot is
    taken while the engine lock is held, eliminating the race that existed
    with Python-side async verification.

    VP42BP AC item 4: each ``register_vN_pool`` call is wrapped in
    ``retry_verification_call`` with a bounded retry-with-backoff policy
    (defaults from ``VerificationRetryPolicy()``; override via
    ``BackrunConfig.verification_retry_policy`` / the
    ``VERIFICATION_RETRY_*`` env vars). Transient ``VerificationRpcError``
    (per-call transport / provider-init) is retried; ``VerificationMismatchError``
    (genuine on-chain divergence) is never retried and still crashes the bot.
    Admission errors (``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError``
    / ``ValueError``) are not in the retry set and propagate immediately.
    """
    _retry_policy = retry_policy or VerificationRetryPolicy()
    # V3 snapshot provides tick data for Python pool builds via trackers.
    # Event backfill is handled by the Rust engine.
    # Trackers use it for tick data at build time.
    uniswap_v3_tracker = bot.add_tracker(
        UniswapV3PoolTracker,
        factory_address=UNISWAP_V3_MAINNET_FACTORY,
        snapshot=v3_snapshot,
    )
    sushiswap_v3_tracker = bot.add_tracker(
        UniswapV3PoolTracker,
        factory_address=SUSHISWAP_V3_MAINNET_FACTORY,
        snapshot=v3_snapshot,
    )
    pancakeswap_v3_tracker = bot.add_tracker(
        UniswapV3PoolTracker,
        factory_address=PANCAKESWAP_V3_MAINNET_FACTORY,
        snapshot=v3_snapshot,
    )
    weth = bot.build_erc20token(WETH_ADDRESS)
    bot_logger.info("[build_paths] V3 trackers added, WETH built — starting path discovery")

    path_count = 0
    skip_count = 0
    token_filter_count = 0
    engine_reject_count = 0
    dup_count = 0
    direction_fail_count = 0
    register_fail_count = 0
    v4_pool_count = 0
    v4_hook_rejected = 0
    v4_dynamic_fee_rejected = 0
    other_exc_count = 0
    registered_path_sigs: set[tuple[str | bool, ...]] = set()

    start = time.perf_counter()

    bot_logger.info("[build_paths] Calling find_paths_async...")
    pool_type_per_depth = _parse_permutation_filter(PATH_PERMUTATION_FILTER)
    pool_types = _pool_types_from_filter(PATH_PERMUTATION_FILTER)
    if pool_type_per_depth is not None:
        bot_logger.info(
            f"[build_paths] Permutation filter active: {PATH_PERMUTATION_FILTER} → depths={pool_type_per_depth}",
        )
    bot_logger.info(f"[build_paths] Pool types: {[t.__name__ for t in pool_types]}")
    async for path_steps in find_paths_async(  # noqa:PLR1702
        chain_id=bot.chain_id,
        start_tokens=[
            WETH_ADDRESS,
            NATIVE_CURRENCY_ADDRESS,  # V4 allows Ether-paired pools
        ],
        end_tokens=[
            WETH_ADDRESS,
            NATIVE_CURRENCY_ADDRESS,  # V4 allows Ether-paired pools
        ],
        max_depth=3,
        pool_types=pool_types,
        db=bot.db,
        pool_type_per_depth=pool_type_per_depth,
        allowed_intermediate_tokens=ALLOWED_INTERMEDIATE_TOKENS,
    ):
        await asyncio.sleep(0)

        # Determine pool types for each step
        steps = list(path_steps)
        pool_type_strs: list[str] = []
        for step in steps:
            if issubclass(step.type, UniswapV2PoolTableBase):
                pool_type_strs.append("V2")
            elif issubclass(step.type, UniswapV3PoolTableBase):
                pool_type_strs.append("V3")
            elif issubclass(step.type, UniswapV4PoolTableBase):
                pool_type_strs.append("V4")
            else:
                pool_type_strs.append("")

        # Build pools through appropriate constructors
        pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool] = []
        skip = False
        # V4 admission refusals (hook/dynamic-fee) surface from the builder at
        # `bot.build_managed_pool` time — the Rust core enforces them in
        # BotState::register_v4_pool (the builder calls py_bot.register_v4_pool
        # → shared BotState). They are counted on their OWN dedicated counters
        # (not skip_count / engine_reject_count), so the summary reflects
        # admission refusals distinctly from generic build skips. Tracked with
        # this flag so the `if skip:` guard below skips skip_count for them.
        v4_admission_rejected = False
        for step, pt in zip(steps, pool_type_strs, strict=True):
            if pt == "V2":
                try:
                    pool = bot.build_pool(
                        step.address,
                        silent=True,
                    )
                except Exception as exc:
                    bot_logger.debug(f"Skip V2 {step.address}: {exc}")
                    skip = True
                    break
            elif pt == "V3":
                try:
                    try:
                        pool = uniswap_v3_tracker.get_pool(
                            pool_address=step.address,
                            silent=True,
                        )
                    except Exception:
                        try:
                            pool = sushiswap_v3_tracker.get_pool(
                                pool_address=step.address,
                                silent=True,
                            )
                        except Exception:
                            try:
                                pool = pancakeswap_v3_tracker.get_pool(
                                    pool_address=step.address,
                                    silent=True,
                                )
                            except Exception:
                                pool = bot.build_pool(
                                    step.address,
                                    silent=True,
                                )
                except Exception as exc:
                    bot_logger.debug(f"Skip V3 {step.address}: {exc}")
                    skip = True
                    break
            elif pt == "V4":
                # V4 pools are identified by (PoolManager, pool_id), not address
                # step.hash contains the pool_id (bytes32 hash of PoolKey)
                if not step.hash:
                    skip = True
                    break
                try:
                    pool = bot.build_managed_pool(
                        address=UNISWAP_V4_POOL_MANAGER_ADDRESS,
                        pool_id=step.hash,
                        silent=True,
                    )
                except HookedPoolRejectedError:
                    # Amount-modifying hook — admission refusal (correctness
                    # floor, enforced by the Rust core at build time). Skip this
                    # path and continue. (Previously mis-handled: this fired at
                    # the registration block, but admission runs at build time,
                    # so the registration-site branch was dead and these were
                    # silently counted as generic skip_count. Now classified by
                    # type per Plan 102.)
                    v4_hook_rejected += 1
                    skip = True
                    v4_admission_rejected = True
                    break
                except DynamicFeePoolRejectedError:
                    # Dynamic fee — admission refusal (correctness floor).
                    # Same rationale as HookedPoolRejectedError above.
                    v4_dynamic_fee_rejected += 1
                    skip = True
                    v4_admission_rejected = True
                    break
                except Exception as exc:
                    bot_logger.debug(f"Skip V4 {step.hash}: {exc}")
                    skip = True
                    break
            else:
                skip = True
                break
            pools.append(cast("UniswapV2Pool | UniswapV3Pool | UniswapV4Pool", pool))

        if skip:
            # V4 admission refusals have their own dedicated counters; don't
            # also count them as generic skips (avoids double-counting).
            if not v4_admission_rejected:
                skip_count += 1
            continue

        # Register with Rust engine
        try:
            for pool, pt in zip(pools, pool_type_strs, strict=True):
                if pt == "V2":
                    retry_verification_call(_retry_policy, engine_registry.register_v2_pool, pool)
                elif pt == "V3":
                    await retry_verification_call_async(
                        _retry_policy,
                        engine_registry.register_v3_pool,
                        pool,
                    )
                elif pt == "V4":
                    v4_pool_count += 1
                    await retry_verification_call_async(
                        _retry_policy,
                        engine_registry.register_v4_pool,
                        pool,
                    )
        except VerificationMismatchError as exc:
            # Verification mismatch — on-chain tick state does not match the
            # engine state. This is fatal: trade on stale data = lose money.
            # Crash loudly (TODO-53b7453b / 7SSOJX: was previously detected by
            # string-matching "tick data mismatch"; now classified by type).
            bot_logger.critical(f"[build_paths] VERIFICATION FAILURE — shutting down: {exc}")
            raise
        except VerificationRpcError as exc:
            # Verification could not be performed (provider construction /
            # transport failure during verify_on_register) AFTER exhausting the
            # bounded retry-with-backoff in ``retry_verification_call`` above
            # (``cfg.verification_retry_policy`` — VP42BP AC item 4). A
            # persistent transport failure means the node is unreachable; the
            # bot must not operate on unverified tick data → crash loudly.
            # (Transient per-call blips are retried before reaching here;
            # provider-construction failure is not transient, so abort is the
            # correct default.)
            bot_logger.critical(f"[build_paths] VERIFICATION RPC FAILURE — shutting down: {exc}")
            raise
        except RuntimeError as exc:
            # Other RuntimeErrors (e.g. phase violations) — NOT a
            # verification failure category; skip this path and continue.
            # Narrowed deliberately: `VerificationMismatchError` and
            # `VerificationRpcError` (both subclass RuntimeError) are caught
            # above, so this arm only fires for genuinely non-verification
            # runtime errors.
            engine_reject_count += 1
            other_exc_count += 1
            bot_logger.info(
                f"[build_paths] Engine registration failed ({type(exc).__name__}): {exc}",
            )
            continue
        except Exception as exc:
            engine_reject_count += 1
            other_exc_count += 1
            bot_logger.info(
                f"[build_paths] Engine registration failed ({type(exc).__name__}): {exc}",
            )
            continue

        # Verification is handled inside the engine at registration time
        # (see set_verify_on_register). No separate Python-side verification
        # needed — the engine snapshots tick data while its lock is held, so
        # the pump cannot race between registration and verification.

        # Resolve directions and register path
        # V4 pools use the same token0/token1 model as V3 for direction resolution
        zfo_list = resolve_directions(pools, weth.address)
        if zfo_list is None:
            direction_fail_count += 1
            continue

        # Skip duplicate paths (same pools, same directions)
        # For V4 pools, use pool_id instead of address
        pool_sigs: list[str] = []
        for p in pools:
            if isinstance(p, UniswapV4Pool):
                pool_sigs.append(p.pool_id.to_0x_hex())
            else:
                pool_sigs.append(p.address)
        path_sig = tuple(v for pair in zip(pool_sigs, zfo_list, strict=True) for v in pair)
        if path_sig in registered_path_sigs:
            dup_count += 1
            continue
        registered_path_sigs.add(path_sig)

        try:
            engine_registry.register_path(list(zip(pools, zfo_list, strict=True)))
        except Exception as exc:
            register_fail_count += 1
            if register_fail_count <= 5:
                bot_logger.warning(f"Path registration failed: {type(exc).__name__}: {exc}")
            continue

        path_count += 1
        if path_count % 1000 == 0:
            bot_logger.info(
                f"[build_paths] Progress: {path_count} paths registered, "
                f"{skip_count} skipped, {token_filter_count} token-filtered, "
                f"{engine_reject_count} engine-rejected, {dup_count} duplicates",
            )

    bot_logger.info(
        f"[build_paths] Path discovery complete: {path_count} paths in "
        f"{time.perf_counter() - start:.1f}s — "
        f"{skip_count} skipped, {token_filter_count} token-filtered, "
        f"{engine_reject_count} engine-rejected "
        f"(other_exc={other_exc_count}), "
        f"{v4_hook_rejected} V4 hook-rejected, "
        f"{v4_dynamic_fee_rejected} V4 dynamic-fee-rejected, "
        f"{dup_count} duplicates, "
        f"{direction_fail_count} direction-failed, "
        f"{register_fail_count} register-failed",
    )
    bot_logger.info(
        f"[build_paths] Summary: {path_count} paths in "
        f"{time.perf_counter() - start:.1f}s — "
        f"{engine_registry.engine.v2_pool_count()} V2, "
        f"{engine_registry.engine.v3_pool_count()} V3, "
        f"{v4_pool_count} V4 pools, "
        f"{v4_hook_rejected} V4 hook-rejected, "
        f"{v4_dynamic_fee_rejected} V4 dynamic-fee-rejected, "
        f"{other_exc_count} other-Exception, "
        f"{engine_registry.engine.path_count()} engine paths",
    )


def get_snapshots(
    bot: Bot,
) -> tuple[
    UniswapV3LiquiditySnapshot | None,
    UniswapV4LiquiditySnapshot | None,
    int | None,
    int | None,
]:
    """Load V3 and V4 liquidity snapshots from the database for the V3 pool
    tracker pre-population.

    Historically the snapshot also fed `engine_registry.start()` via
    `stream_v3_snapshot_to_engine`/`stream_v4_snapshot_to_engine` SQLAlchemy
    forwarding — that path is retired (JUCFCB/2SM4Y7/DADWUP: the engine's DB
    snapshot is loaded eagerly at `PyBot` construction by
    `Bot::load_snapshot_from_db`, and the snapshot→WS gap is closed
    automatically inside `BlockPump::resume_from_subscribe` — J3FMDO; the
    per-pool `insert_*_pool_snapshot` pyo3 surface + the SQLAlchemy
    `yield_per` loops are removed).

    Returns (v3_snapshot, v4_snapshot, v3_snapshot_block, v4_snapshot_block).
    """
    v3_snapshot_block: int | None = None
    v4_snapshot_block: int | None = None

    # ── V3 snapshot ──────────────────────────────────────────────
    v3_snapshot = None
    try:
        v3_snapshot = UniswapV3LiquiditySnapshot(
            source=V3DatabaseSnapshot(chain_id=1, db=bot.db),
        )
    except ValueError:
        bot_logger.info("[backfill] V3: no snapshot data in database, skipping")

    if v3_snapshot is not None:
        v3_snapshot_block = v3_snapshot.newest_block
        bot_logger.info(f"[backfill] V3: DB snapshot at block {v3_snapshot_block}")

    # ── V4 snapshot ──────────────────────────────────────────────
    v4_snapshot = None
    try:
        v4_db_snapshot = V4DatabaseSnapshot(chain_id=1, db=bot.db)
        v4_snapshot = UniswapV4LiquiditySnapshot(source=v4_db_snapshot)
    except ValueError:
        bot_logger.info("[backfill] V4: no snapshot data in database, skipping")

    if v4_snapshot is not None:
        v4_snapshot_block = v4_snapshot.newest_block
        bot_logger.info(f"[backfill] V4: DB snapshot at block {v4_snapshot_block}")

    return v3_snapshot, v4_snapshot, v3_snapshot_block, v4_snapshot_block


async def _dispatch_profitable(
    *,
    results: list[tuple[int, int, int, tuple[int, ...], tuple[int, ...], int]],
    engine_registry: EngineRegistry,
    async_w3: AsyncAlloyProvider,
    sim_ctx: PySimulateContext,
    operator_private_key: str,
    operator_nonce: int,
    dispatcher: PyDispatcher,
    current_block: int,
    base_fee_next: int,
    dry_run: bool,
) -> None:
    """Encode → simulate → submit a batch of profitable results via the Rust seam.

    The A5 cutover: this replaces the Python ``dispatch_profitable_results`` +
    ``simulate_one`` chain with ``dispatch_profitable_py`` (simulate) →
    ``dispatch_and_submit_py`` (submit). The sim fan-out, the gross/net profit
    arithmetic, the market-aware priority fee, the path suppression, and the
    thin-margin pre-filter all run in the Rust core; Python only builds the
    candidate list, renders the ``[sim]``/``[profit]`` summaries, and chains to
    the submit seam.
    """
    candidates: list[PyDispatchCandidate] = []
    for pid, inp, prof, ho, _ci, sb in results:
        path_info = engine_registry.paths.get(pid)
        if path_info is None:
            bot_logger.debug(f"[sim-none] path={pid}: path_info missing")
            continue
        if len(ho) != len(path_info.hops):
            bot_logger.debug(
                f"[dispatch] skip path={pid}: hop_outputs length "
                f"({len(ho)}) != hops ({len(path_info.hops)})",
            )
            continue
        candidates.append(
            PyDispatchCandidate(
                path_id=pid,
                optimal_input=inp,
                engine_profit=prof,
                hop_outputs=list(ho),
                solve_block=sb,
                path_info=path_info,
            ),
        )

    if not candidates:
        return

    outcome = await dispatch_profitable_py(
        candidates=candidates,
        context=sim_ctx,
        dispatcher=dispatcher,
        base_fee_next=base_fee_next,
        current_block=current_block,
        min_profit_net=MIN_PROFIT_NET,
        min_profit_margin_bps=MIN_PROFIT_MARGIN_BPS,
    )
    _render_sim_summary(outcome)
    _render_sim_failures(outcome)
    _render_profit_logs(outcome)

    # ── Submit gas-profitable via the Rust submit leaf ───
    async_alloy = async_w3.as_async_alloy()
    if async_alloy is None:
        bot_logger.error("[dispatch] async_w3 is not an Alloy-backed provider; cannot submit")
        return
    signer = PyTxSigner(key=operator_private_key, chain_id=1)
    records = await dispatch_and_submit_py(
        candidates=outcome.gas_profitable,
        dispatcher=dispatcher,
        provider=async_alloy,
        signer=signer,
        operator_nonce=operator_nonce,
        current_block=current_block,
        dry_run=dry_run,
        inject_code=INJECT_EXECUTOR_CODE,
    )
    for record in records:
        if record["kind"] == "submitted":
            bot_logger.info(
                f"Submitted path {record['path_id']} "
                f"hash={record['tx_hash']} nonce={record['nonce']}",
            )
        elif record["reason"] == "pools_claimed":
            bot_logger.debug(f"[dispatch] skip path={record['path_id']}: pools claimed after sim")
        elif record["reason"] == "dry_run":
            pass  # dry_run skip already logged above
        elif record["reason"] == "inject_code":
            bot_logger.warning(
                f"[dispatch] path={record['path_id']}: skipping submission — "
                "INJECT_EXECUTOR_CODE is active",
            )
        elif record["reason"] == "broadcast_failed":
            bot_logger.debug(f"Send failed: {record.get('detail', '')}")


def _render_sim_summary(outcome: PyDispatchOutcome) -> None:
    """Render the ``[sim]`` line from ``PyDispatchOutcome`` fields (D4 stay-Python).

    Ports the prior ``[sim] N candidates: X ok (Y profitable, Z below
    threshold), W failed, V exceptions …`` summary (the only rendering the A5
    acceptance criterion requires). Appends the suppressed/thin drops when
    non-zero (more informative than the prior line, which folded them in
    opaquely). The ``[profit]``/``[sim-ok]`` per-path logs are rendered by
    :func:`_render_profit_logs`.
    """
    profitable = outcome.gas_profitable
    best_net = max((c.net_profit for c in profitable), default=0)
    breakdown = format_failure_breakdown(outcome.fail_buckets)
    sim_ok = len(profitable) + outcome.gas_unprofitable_count
    extra = ""
    if outcome.suppressed_count or outcome.thin_dropped:
        extra = f" — suppressed={outcome.suppressed_count}, thin={outcome.thin_dropped}"
    bot_logger.info(
        f"[sim] {outcome.candidate_count} candidates: "
        f"{sim_ok} ok ({len(profitable)} profitable, "
        f"{outcome.gas_unprofitable_count} below threshold), "
        f"{outcome.fail_count} failed, {outcome.exception_count} exceptions"
        f"{f' — best net={best_net // 10**9}gwei' if profitable else ''}"
        f"{f' — by reason: {breakdown}' if breakdown else ''}"
        f"{extra}",
    )


def _render_profit_logs(outcome: PyDispatchOutcome) -> None:
    """Render the ``[profit]`` per-path hop-detail log (D4 stay-Python).

    The prior ``[sim-ok]`` gas-unprofitable per-path log is NOT reproduced:
    ``PyDispatchOutcome`` collapses gas-unprofitable to a count by design (A2 —
    the cockpit only logs these as a tally; they're valid sims below the net
    threshold, not submitted). The ``[profit]`` log iterates the survivors +
    looks up each path's ``PathInfo`` via ``outcome.path_infos`` (Decision 1=B).
    """
    for cand in outcome.gas_profitable:
        path_info = outcome.path_infos.get(cand.path_id)
        hop_details = []
        if path_info is not None:
            for i, h in enumerate(path_info.hops):
                if isinstance(h, V2HopInfo):
                    hop_details.append(
                        f"  hop[{i}] V2 addr={h.pool_address} "
                        f"t0={h.token0_address} t1={h.token1_address} "
                        f"fee={h.fee} zfo={h.zfo}",
                    )
                elif isinstance(h, V3HopInfo):
                    hop_details.append(
                        f"  hop[{i}] V3 addr={h.pool_address} "
                        f"t0={h.token0_address} t1={h.token1_address} "
                        f"fee={h.fee} zfo={h.zfo}",
                    )
                elif isinstance(h, V4HopInfo):
                    hop_details.append(
                        f"  hop[{i}] V4 pm={h.pool_manager_address} "
                        f"pid={h.pool_id_hex} "
                        f"c0={h.currency0_address} c1={h.currency1_address} "
                        f"fee={h.fee} ts={h.tick_spacing} zfo={h.zfo}",
                    )
        hops_str = "\n".join(hop_details)
        bot_logger.info(
            f"[profit] path={cand.path_id} {path_info.path_type if path_info else '?'} "
            f"gross={cand.gross_profit / 1e18:.6f}ETH ({cand.gross_profit // 10**9}gwei) "
            f"net={cand.net_profit / 1e18:.6f}ETH ({cand.net_profit // 10**9}gwei) "
            f"gas={cand.gas_used} prio={cand.priority_fee // 10**9}gwei\n{hops_str}",
        )


def _render_sim_failures(outcome: PyDispatchOutcome) -> None:
    """Render one ``[sim-fail]`` line per reverted / failed candidate (D3).

    Counterpart to :func:`_render_profit_logs` — operates on the FAILURES
    rather than the survivors. Each record carries the per-candidate detail the
    Rust core surfaced across the FFI (``path_id`` + bucket + ``fail_index``
    + raw revert bytes); this renderer joins it to the path's hop token
    summary (via :func:`_hop_token_summary`), looked up from
    ``outcome.path_infos`` — the same map :func:`_render_profit_logs` uses —
    so the operator can identify WHICH path reverted against WHICH pools
    without lifting a session.

    The ``[sim]`` aggregate summary still leads; this only emits when the
    Rust outcome reports ``N > 0`` failures. Capped at
    :data:`_SIM_FAIL_RENDER_CAP` records per batch with a ``… (+M more)``
    trailing line so a thin-margin revert storm doesn't flood the log.
    """
    failures = outcome.failures
    if not failures:
        return
    cap = _SIM_FAIL_RENDER_CAP
    path_infos = outcome.path_infos
    for rec in failures[:cap]:
        path_id = rec["path_id"]
        bucket = rec["bucket"]
        fail_idx = rec["fail_index"]
        revert_hex = rec["revert_data"]
        path_info = path_infos.get(path_id)
        path_type = path_info.path_type if path_info is not None else "?"
        hops = (
            _hop_token_summary(path_info.hops) if path_info is not None else "(path_info missing)"
        )
        bot_logger.info(
            f"[sim-fail] path={path_id} type={path_type} bucket={bucket} "
            f"fail_idx={fail_idx} revert={revert_hex} hops={hops}",
        )
    overflow = len(failures) - cap
    if overflow > 0:
        bot_logger.info(f"[sim-fail] … (+{overflow} more)")


async def consume_result_batches(
    engine_registry: EngineRegistry,
    async_w3: AsyncAlloyProvider,
    sim_ctx: PySimulateContext,
    executor_address: str,
    operator_address: str,
    operator_private_key: str,
    dispatcher: PyDispatcher,
    dry_run: bool,
    *,
    block_stream: AsyncIterator[dict[str, int]] | None = None,
    result_iter: AsyncIterator[dict[str, object]] | None = None,
) -> None:
    """Consume the block stream (clock) + result batches (dispatch) in parallel.

    Epic 6W35AI: the block clock comes from the forwarded ``newHeads`` stream
    (``engine.block_stream()``), NOT from ``ResultBatch.solve_block``. The
    result batch's ``solve_block`` lagged by the send debounce + only advanced
    when a batch was actually sent, so the bot's ``[block: N]`` froze behind
    the pump's ``current_block``. The block stream ticks once per accepted
    ``WsEvent::BlockHeader`` — the authoritative clock.

    Two async streams are awaited concurrently via ``asyncio.wait``:
      * block stream  → ``dispatcher.advance_block``, ``record_block_time``,
        ``fee_history``, the ``[block:]`` log, and ``base_fee_next``.
      * result batch  → ``_dispatch_profitable`` (the Rust
        ``dispatch_profitable_py`` → ``dispatch_and_submit_py`` chain) keyed
        off ``dispatcher.current_block`` (the block clock), with the
        per-result solve_block recorded for age/staleness.

    Both streams are injectable for testing; production pulls them from the
    engine.
    """
    bot_logger.info("[consumer] Starting — block stream + result batches from Rust pump")

    if block_stream is None:
        block_stream = engine_registry.engine.block_stream()
    if result_iter is None:
        result_iter = aiter(engine_registry.engine)

    # Prime both streams. Each completed future is re-primed unless its stream
    # ended (StopAsyncIteration); the loop exits when both are exhausted.
    block_fut = cast(
        "asyncio.Task[dict[str, int]] | None", asyncio.ensure_future(anext(block_stream))
    )
    result_fut = cast(
        "asyncio.Task[dict[str, object]] | None", asyncio.ensure_future(anext(result_iter))
    )

    while block_fut is not None or result_fut is not None:
        pending = {f for f in (block_fut, result_fut) if f is not None}
        done, _ = await asyncio.wait(pending, return_when=asyncio.FIRST_COMPLETED)

        for fut in done:
            if fut is block_fut:
                block_fut = cast(
                    "asyncio.Task[dict[str, int]] | None",
                    _reprime(block_stream, fut, "block stream"),
                )
                await _apply_block_if_ready(fut, dispatcher, async_w3)
            elif fut is result_fut:
                result_fut = cast(
                    "asyncio.Task[dict[str, object]] | None",
                    _reprime(result_iter, fut, "result stream"),
                )
                await _apply_result_if_ready(
                    fut,
                    dispatcher,
                    engine_registry,
                    async_w3,
                    sim_ctx,
                    executor_address,
                    operator_address,
                    operator_private_key,
                    dry_run,
                )


_TEE_SENTINEL: Any = object()


def _tee_block_stream(
    source: AsyncIterator[dict[str, int]],
) -> tuple[
    AsyncIterator[dict[str, int]],
    AsyncIterator[dict[str, int]],
    asyncio.Task[None],
]:
    """Fan a single once-only block stream to two independent async iterators.

    The pump's `engine.block_stream()` is single-consumer: `BlockStream.__anext__`
    moves the mpsc receiver out of an `Arc<Mutex<Option<rx>>>` per call, so two
    `async for` loops over one stream object race (the second sees `None` and
    raises `StopAsyncIteration` immediately). This tee drives the source once and
    copies each block to two unbounded queues (eth ~1 block/12s — no backpressure
    deadlock risk), yielding two independent iterators that EACH see every block.

    Regression: `run()` previously called `engine.block_stream()` twice (the real
    `consume_result_batches` self-acquires when `block_stream=None`; `run()` line
    ~967 acquired another for the recurring-verify ticker). The real seam is
    once-only — the second call raised
    `RuntimeError("block_stream() can only be called once")` entering the main
    loop, crashing every permutation run. Acquire once + tee fixes it.

    Returns `(branch_a, branch_b, driver_task)`. `branch_a` feeds the result
    consumer; `branch_b` feeds the recurring-verify ticker. The driver completes
    when the source exhausts (production: never — the pump runs indefinitely; the
    driver stays pending on the source and is cancelled by the caller on exit).
    """
    q_a: asyncio.Queue[Any] = asyncio.Queue()
    q_b: asyncio.Queue[Any] = asyncio.Queue()

    async def _driver() -> None:
        try:
            async for block in source:
                await q_a.put(block)
                await q_b.put(block)
        finally:
            await q_a.put(_TEE_SENTINEL)
            await q_b.put(_TEE_SENTINEL)

    async def _branch(q: asyncio.Queue[Any]) -> AsyncIterator[dict[str, int]]:
        while True:
            item = await q.get()
            if item is _TEE_SENTINEL:
                return
            yield item

    driver = asyncio.create_task(_driver(), name="block-stream-tee")
    return _branch(q_a), _branch(q_b), driver


def _reprime(
    stream: AsyncIterator[Any],
    fut: asyncio.Task[Any],
    label: str,
) -> asyncio.Task[Any] | None:
    """If `fut`'s stream ended, return None; else schedule the next pull."""
    try:
        fut.result()
    except StopAsyncIteration:
        bot_logger.info("[consumer] %s ended", label)
        return None
    except BaseException:  # noqa: BLE001 — surfaced via fut.result() in caller
        return None
    return asyncio.ensure_future(anext(stream))


async def _apply_block_if_ready(
    fut: asyncio.Task[dict[str, int]],
    dispatcher: PyDispatcher,
    async_w3: AsyncAlloyProvider,
) -> None:
    """Drive the block clock from a forwarded ``newHeads`` tick if fut resolved.

    Authoritative clock source (epic 6W35AI): ``advance_block``,
    ``record_block_time``, ``fee_history``, and the ``[block:]`` log all key
    off the block-stream number — never ``ResultBatch.solve_block``.
    """
    if fut.cancelled() or fut.exception() is not None:
        return
    try:
        block = fut.result()
    except StopAsyncIteration:
        return

    block_number = int(block["number"])
    block_timestamp = int(block["timestamp"])
    base_fee = int(block.get("base_fee_per_gas") or 0)
    gas_used = int(block["gas_used"])
    gas_limit = int(block["gas_limit"])

    base_fee_next = next_base_fee(
        parent_base_fee=base_fee,
        parent_gas_used=gas_used,
        parent_gas_limit=gas_limit,
    )

    # 7UIYJ6: ``eth_feeHistory`` + hex-decode + ``record_priority_fees`` now
    # happen in the Rust submit leaf (``fetch_fee_history_py`` extracts the
    # AsyncAlloyProvider, calls eth_feeHistory, records into the dispatcher's
    # ring internally, no-ops on RPC failure — matching the prior
    # ``except Web3Exception: pass``).
    async_alloy = async_w3.as_async_alloy()
    if async_alloy is not None:
        await fetch_fee_history_py(
            provider=async_alloy,
            dispatcher=dispatcher,
            block_count=1,
            last_block=block_number,
            reward_percentiles=[float(p) for p in FEE_PERCENTILES],
        )

    dispatcher.record_block_time(block_number, block_timestamp)
    if dispatcher.block_time_count() >= 2:
        oldest_bn, _oldest_ts = dispatcher.block_times_oldest()
        if block_number != oldest_bn:
            latency = time.time() - block_timestamp
            bot_logger.info(
                f"[block: {block_number}]"
                f"[latency: {latency:.1f}s]"
                f"[base fee: {base_fee / 10**9:.5f}, {base_fee_next / 10**9:.5f} next]",
            )

    dispatcher.advance_block(block_number)


async def _apply_result_if_ready(
    fut: asyncio.Task[dict[str, object]],
    dispatcher: PyDispatcher,
    engine_registry: EngineRegistry,
    async_w3: AsyncAlloyProvider,
    sim_ctx: PySimulateContext,
    executor_address: str,
    operator_address: str,
    operator_private_key: str,
    dry_run: bool,
) -> None:
    """Dispatch profitable results from a solver result batch if fut resolved.

    The block clock is read from ``dispatcher.current_block`` (advanced by the
    block stream), NOT ``batch["solve_block"]`` — the latter is retained only
    as per-result metadata for age/staleness.
    """
    if fut.cancelled() or fut.exception() is not None:
        return
    try:
        batch = fut.result()
    except StopAsyncIteration:
        return

    current_block = dispatcher.current_block
    operator_nonce = await async_w3.get_transaction_count(cast("ChecksumAddress", operator_address))
    solve_block = int(cast("Any", batch["solve_block"]))  # per-result age metadata, not the clock

    results: list[tuple[int, int, int, tuple[int, ...], tuple[int, ...], int]] = []
    for item in cast("Any", batch["fresh"]):
        path_id, opt_input, profit, hop_outs, consumed_ins = item
        results.append((
            int(path_id),
            int(opt_input),
            int(profit),
            tuple(int(h) for h in hop_outs),
            tuple(int(c) for c in consumed_ins),
            solve_block,
        ))
    for item in cast("Any", batch["updated"]):
        path_id, opt_input, profit, hop_outs, consumed_ins = item
        results.append((
            int(path_id),
            int(opt_input),
            int(profit),
            tuple(int(h) for h in hop_outs),
            tuple(int(c) for c in consumed_ins),
            solve_block,
        ))

    for path_id in cast("Any", batch["removed"]):
        dispatcher.discard_path(int(path_id))

    if results:
        await _dispatch_profitable(
            results=results,
            engine_registry=engine_registry,
            async_w3=async_w3,
            sim_ctx=sim_ctx,
            operator_private_key=operator_private_key,
            operator_nonce=operator_nonce,
            dispatcher=dispatcher,
            current_block=current_block,
            base_fee_next=next_base_fee(
                parent_base_fee=int(cast("Any", batch.get("base_fee_per_gas") or 0)),
                parent_gas_used=int(cast("Any", batch["gas_used"])),
                parent_gas_limit=int(cast("Any", batch["gas_limit"])),
            ),
            dry_run=dry_run,
        )


def _build_arg_parser() -> argparse.ArgumentParser:
    """Build the backrun example's argument parser.

    Extracted from ``main()`` so the CLI surface (especially the
    ``--node-http`` / ``--node-ws`` cascade overrides) is testable without
    running the full async session.

    Returns:
        The configured ``ArgumentParser`` (caller invokes ``parse_args``).

    """
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--live",
        action="store_true",
        help="Enable live mode (submits real transactions)",
    )
    parser.add_argument(
        "--permutation",
        type=str,
        default=None,
        help=(
            "Pool version permutation filter (e.g. V2-V3-V4). "
            "Only paths matching this 3-hop ordering will be built and simulated. "
            "Overrides PATH_PERMUTATION_FILTER in the source file."
        ),
    )
    parser.add_argument(
        "--node-http",
        type=str,
        default=None,
        help=(
            "HTTP RPC endpoint for the backrun chain (Ethereum mainnet). "
            "Highest-priority source in the RPC URI cascade: "
            "--node-http > DEGENBOT_RPC_HTTP_CHAINID_1 > NODE_HOST_HTTP "
            "> config.toml rpc[1] > error."
        ),
    )
    parser.add_argument(
        "--node-ws",
        type=str,
        default=None,
        help=(
            "WebSocket RPC endpoint for the backrun chain (Ethereum mainnet). "
            "Highest-priority source in the RPC URI cascade: "
            "--node-ws > DEGENBOT_RPC_WS_CHAINID_1 > NODE_HOST_WEBSOCKET "
            "> config.toml ws[1] > error."
        ),
    )
    return parser


async def main() -> None:
    parser = _build_arg_parser()
    args = parser.parse_args()
    dry_run = not args.live

    # Override PATH_PERMUTATION_FILTER from CLI if --permutation is set
    global PATH_PERMUTATION_FILTER
    if args.permutation is not None:
        PATH_PERMUTATION_FILTER = {args.permutation}
        bot_logger.info(f"[startup] Permutation filter from CLI: {PATH_PERMUTATION_FILTER}")
    if not dry_run:
        bot_logger.info("\n*** LIVE MODE — BOT WILL SUBMIT REAL TRANSACTIONS! ***\n")

    env = dotenv.dotenv_values("examples/mainnet.env")
    try:
        cfg = BackrunConfig.from_env(
            env,
            live=not dry_run,
            permutation=args.permutation,
            cli_http=args.node_http,
            cli_ws=args.node_ws,
        )
    except ValueError as exc:
        bot_logger.error(str(exc))
        return

    # BackrunSession owns the full startup handshake and enforces the phase
    # ordering the Rust pump's state machine requires:
    #   start():  subscribe → stream snapshots → backfill → verify config
    #             (EngineRegistry.start, stops BEFORE resume)
    #   run():    attach consumer → resume → build_paths → release_python_state
    #             → drop bot → await result consumer (the main loop)
    # The early release inside run() preserves the hot-loop memory profile:
    # the loop keeps only engine_registry + async_w3 + dispatcher once the
    # Rust engine owns canonical pool state.
    #
    # Ctrl-C handling: a SIGINT during ``await session.run()`` unwinds through
    # ``BackrunSession.__aexit__`` → ``shutdown()`` (calls ``engine.stop()``,
    # which aborts the Rust pump task so it doesn't block process exit on a
    # silent WS stream). The KeyboardInterrupt is then caught here so the
    # operator sees a single clean line instead of a traceback. ``CancelledError``
    # is caught too: some asyncio versions surface SIGINT inside the awaited
    # coroutine as a cancelled task.
    try:
        async with BackrunSession(cfg) as session:
            await session.run()
    except (KeyboardInterrupt, asyncio.CancelledError):
        bot_logger.info("[shutdown] interrupted — Rust pump stopped, exiting.")


if __name__ == "__main__":
    start = time.perf_counter()
    asyncio.run(main())
    bot_logger.info(f"Completed in {time.perf_counter() - start:.2f}s")
