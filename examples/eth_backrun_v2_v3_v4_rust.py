"""Ethereum mainnet backrun bot: Uniswap V2/V3/V4 arbitrage using the Rust engine.

A thin Python orchestration layer over the Rust-owned UniswapArbEngine.
The Rust engine owns all pool state and path solving. Python does: pool
construction, swap encoding, simulation, and transaction submission.

The executor contract (cmd_executor.vy) uses a compact command-stream
format with 1-byte opcodes and tightly-packed parameters. Address indices
reference a shared address table with sentinel support for common addresses
(WETH, PoolManager, executor, USDC, WBTC). V4 swaps execute inside a
single unlock context with automatic delta settlement. Profit is verified
via the expected_balance parameter (packed: value<<8 | check_mode).

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
import datetime
import json
import sys
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
if str(_REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(_REPO_ROOT))
import asyncio
import dataclasses
import gc
import itertools
import operator
import os
import pathlib
import time
import traceback
from collections import deque
from typing import Any

import dotenv
import eth_abi.abi
import eth_account
import web3
from eth_backrun_helpers import (
    HopInfo,
    PathInfo,
    V2HopInfo,
    V3HopInfo,
    V4HopInfo,
    encode_cmd_stream,
    v4_input_is_native,
)
from eth_typing import ChainId, ChecksumAddress
from hexbytes import HexBytes
from web3 import AsyncWeb3, Web3
from web3.exceptions import TransactionNotFound, Web3Exception
from web3.types import BlockStateCallV1, SimulateV1Payload, StateOverrideParams, TxParams

from degenbot import Bot, UniswapV2Pool, UniswapV3Pool, get_checksum_address
from contracts.cmd_stream import (
    _mapping_slot,
    compute_simulation_warmup_slots,
    pack_expected_balance,
)
from degenbot.arbitrage.encoding import fits_int128
from degenbot.calculations.evm_math import next_base_fee
from degenbot.constants import WRAPPED_NATIVE_TOKENS, ZERO_ADDRESS
from degenbot.database.models.pools import (  # noqa:F401
    PancakeswapV2PoolTable,
    PancakeswapV3PoolTable,
    SushiswapV2PoolTable,
    SushiswapV3PoolTable,
    UniswapV2PoolTable,
    UniswapV2PoolTableBase,
    UniswapV3PoolTable,
    UniswapV3PoolTableBase,
    UniswapV4PoolTable,
    UniswapV4PoolTableBase,
)
from degenbot.degenbot_rs import UniswapArbEngine  # type: ignore[attr-defined]
from degenbot.logging import logger as bot_logger
from degenbot.pathfinding import find_paths_async
from degenbot.provider.sync_adapter import ProviderAdapter
from degenbot.uniswap.deployments import EthereumMainnetUniswapV4
from degenbot.uniswap.snapshot_binary import (
    stream_v3_snapshot_to_engine,
    stream_v4_snapshot_to_engine,
)
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
        for part in perm.split("-"):
            versions_needed.add(part)

    types = set()
    for version in versions_needed:
        types.update(_POOL_VERSION_MAP[version])
    return list(types)


# Number of consecutive sim-failures before a path is suppressed.
PATH_SUPPRESS_THRESHOLD = 10

# How many blocks between retry attempts for suppressed paths.
PATH_SUPPRESS_RETRY_INTERVAL = 100

# ── Executor code injection via eth_simulateV1 ──────────────────
# When INJECT_EXECUTOR_CODE=True, we inject the cmd_executor
# runtime bytecode at a fresh address via stateOverrides.code.
# This lets us test the new V2/V3/V4-capable executor contract
# WITHOUT deploying it on mainnet first.
# The runtime bytecode must have immutables (OWNER_ADDR, WETH_ADDR,
# POOL_MANAGER_ADDR, USER0_ADDR, USER1_ADDR, plus 4 precomputed delta slots)
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
    )
)

UNISWAP_V3_MAINNET_FACTORY = "0x33128a8fC17869897dcE68Ed026d694621f6FDfD"
SUSHISWAP_V3_MAINNET_FACTORY = "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
PANCAKESWAP_V3_MAINNET_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"

# V4 PoolManager on Ethereum mainnet
UNISWAP_V4_POOL_MANAGER_ADDRESS = get_checksum_address("0x000000000004444c5dc75cB358380D2e3De08A90")

# V4 hook filtering: exclude pools with amount-modifying hooks.
# Mask 0xCC covers BEFORE_SWAP(0x80), AFTER_SWAP(0x40),
# BEFORE_SWAP_RETURNS_DELTA(0x08), AFTER_SWAP_RETURNS_DELTA(0x04).
# Pools with any of these flags can modify swap amounts, violating
# the solver's assumption that V3 math applies exactly.
AMOUNT_MODIFYING_HOOK_MASK = 0xCC

# V4 dynamic fee flag — pools with fee == 0x100000 have dynamic fees.
# The solver requires a fixed fee for accurate profit calculation.
V4_DYNAMIC_FEE_FLAG = 0x100000

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
    # execute(commands, expected_balance) — cmd_executor
    {
        "stateMutability": "payable",
        "type": "function",
        "name": "execute",
        "inputs": [
            {"name": "commands", "type": "bytes"},
            {"name": "expected_balance", "type": "uint256"},
        ],
        "outputs": [
            {"name": "", "type": "uint256"},
        ],
    },
]


BALANCEOF_SELECTOR = web3.Web3.keccak(text="balanceOf(address)")[:4]


def encode_balanceof_calldata(account: str) -> bytes:
    """Encode an ERC20 balanceOf(address) call for simulation."""
    return BALANCEOF_SELECTOR + eth_abi.abi.encode(
        types=["address"],
        args=[account],
    )


# ──────────────────────────────────────────────────────────────────
# Executor code injection helpers
# ──────────────────────────────────────────────────────────────────

# Cached runtime bytecode (loaded once, reused across all simulations)
_runtime_bytecode_cache: str | None = None


def _load_executor_runtime_bytecode() -> str:
    """Load the patched runtime bytecode from contracts/ directory.

    The bytecode has all 9 immutable slots baked in: OWNER_ADDR, WETH_ADDR,
    POOL_MANAGER_ADDR, USER0_ADDR, USER1_ADDR, and 4 precomputed delta
    slots. See contracts/recompile.py for the full layout.
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
        f"{len(code) // 2 - 1} bytes from {bytecode_path}"
    )
    return _runtime_bytecode_cache


def build_simulation_state_overrides(
    *,
    executor_owner: str,
    inject_code: bool = False,
    injected_address: str | None = None,
) -> dict[ChecksumAddress, StateOverrideParams]:
    """Build the stateOverrides dict for eth_simulateV1.

    All fields (code, balance, nonce, state, stateDiff) go under
    stateOverrides per the Alloy AccountOverride spec. Reth v2.3.0
    applies all of them correctly from stateOverrides in eth_simulateV1.

    IMPORTANT: the runtime bytecode must have Vyper CBOR metadata
    intact and immutables appended AFTER it. Vyper's CODECOPY offsets
    assume the deployed layout [code][CBOR][immutables]; removing
    the CBOR breaks the jump table, JUMPDEST targets, and immutable
    reads. The recompile.py script handles this automatically.

    When inject_code=True:
      - Injects runtime bytecode at injected_address via code
      - Pre-warms the executor's ERC6909 and WETH storage slots via
        stateDiff (avoids ~61K gas in cold SSTORE penalties per simulation)
      - Funds the injected executor with ETH and WETH for V3 callbacks
      - Funds the executor owner with ETH for gas

    When inject_code=False:
      - Only funds the executor owner with ETH for gas
    """

    overrides: dict[ChecksumAddress, StateOverrideParams] = {}

    # Fund executor owner with ETH for gas
    overrides[get_checksum_address(executor_owner)] = StateOverrideParams(balance=100 * 10**18)

    if inject_code and injected_address:
        # Inject executor runtime bytecode at the fresh address.
        # CBOR metadata must remain intact — see IMPORTANT note in the
        # docstring and contracts/recompile.py.
        runtime_code = _load_executor_runtime_bytecode()
        overrides[get_checksum_address(injected_address)] = StateOverrideParams(
            code=runtime_code,
            # Fund the injected executor with ETH for V4 settlement and
            # V3 callback WETH payments (the deployed contract wraps 10 ETH
            # at construction; code injection skips this, so we must set
            # the balance explicitly).
            balance=10 * 10**18,
        )

        # Pre-warm ERC6909 and WETH storage slots to avoid cold SSTORE
        # penalties. compute_simulation_warmup_slots() returns stateDiff
        # entries for 3 slots:
        #   1. WETH9.balanceOf[executor] = 1 wei (mapping slot 3)
        #   2. PM.erc6909_balanceOf[executor][weth_id] = 1 (slot 4)
        #   3. PM.erc6909_balanceOf[executor][native_id] = 1 (slot 4)
        warmup = compute_simulation_warmup_slots(
            executor_address=injected_address,
            weth_address=WETH_ADDRESS,
            pool_manager_address=UNISWAP_V4_POOL_MANAGER_ADDRESS,
        )

        for contract_addr, contract_overrides in warmup.items():
            addr = get_checksum_address(contract_addr)
            if addr not in overrides:
                overrides[addr] = {}
            for key, value in contract_overrides.items():
                if key == "stateDiff":
                    overrides[addr].setdefault("stateDiff", {})
                    overrides[addr]["stateDiff"].update(value)
                elif key == "balance":
                    # Don't let the warmup's 1 wei balance override the
                    # 10 ETH we already set for the executor or owner
                    if "balance" not in overrides[addr]:
                        overrides[addr][key] = value
                else:
                    overrides[addr][key] = value

        # Override the WETH balance with the operational amount (10 ETH)
        # for V3 callbacks. The warmup function set it to 1 wei for
        # slot-warming purposes; we need a much larger balance.
        overrides[get_checksum_address(WETH_ADDRESS)].setdefault("stateDiff", {})

        # Compute the WETH balanceOf slot (same formula as the warmup
        # function uses internally — keccak256(pad(executor, 32) || pad(3, 32)))
        # We overwrite the warmup's 1-wei entry with 10 ETH.

        WETH_BALANCEOF_MAPPING_SLOT = 3
        weth_balance_slot = _mapping_slot(WETH_BALANCEOF_MAPPING_SLOT, int(injected_address, 16))
        overrides[Web3.to_checksum_address(WETH_ADDRESS)]["stateDiff"][
            f"0x{weth_balance_slot:064x}"
        ] = "0x" + (10 * 10**18).to_bytes(32, "big").hex()

    return overrides


class PathSuppression:
    """Track per-path simulation failures and suppress consistently failing paths.

    After a path fails simulation PATH_SUPPRESS_THRESHOLD consecutive times,
    it is suppressed — excluded from the simulation candidate list. Suppressed
    paths are retried every PATH_SUPPRESS_RETRY_INTERVAL blocks. If a retry
    succeeds, the path is permanently un-suppressed.
    """

    def __init__(self) -> None:
        # path_id → consecutive failure count
        self._fail_counts: dict[int, int] = {}
        # path_id → block number when the path was last retried
        self._last_retry_block: dict[int, int] = {}
        # path_id → True if currently suppressed
        self._suppressed: set[int] = set()
        # Total paths suppressed (for logging)
        self._total_suppressed: int = 0

    def record_success(self, path_id: int) -> None:
        """A path succeeded at simulation — reset its failure counter."""
        self._fail_counts.pop(path_id, None)
        if path_id in self._suppressed:
            self._suppressed.discard(path_id)
            bot_logger.debug(f"[suppress] path={path_id} un-suppressed after successful sim")

    def record_failure(self, path_id: int) -> None:
        """A path failed simulation — increment its counter, maybe suppress."""
        count = self._fail_counts.get(path_id, 0) + 1
        self._fail_counts[path_id] = count

        if count >= PATH_SUPPRESS_THRESHOLD and path_id not in self._suppressed:
            self._suppressed.add(path_id)
            self._total_suppressed += 1
            bot_logger.info(
                f"[suppress] path={path_id} SUPPRESSED after {count} consecutive failures "
                f"(total suppressed: {self._total_suppressed})"
            )

    def is_suppressed(self, path_id: int, current_block: int) -> bool:
        """Check if a path is currently suppressed (with retry logic).

        Returns True if the path should be skipped. Returns False if
        the path is due for a retry — the caller should simulate it.
        """
        if path_id not in self._suppressed:
            return False

        # Check if it's time for a retry
        last_retry = self._last_retry_block.get(path_id, 0)
        if current_block - last_retry >= PATH_SUPPRESS_RETRY_INTERVAL:
            # Allow this attempt — mark the retry block
            self._last_retry_block[path_id] = current_block
            bot_logger.debug(f"[suppress] path={path_id} retrying at block {current_block}")
            return False

        return True

    @property
    def total_suppressed(self) -> int:
        return self._total_suppressed

    def discard(self, path_id: int) -> None:
        """Permanently discard suppression tracking for a de-registered path."""
        self._fail_counts.pop(path_id, None)
        self._suppressed.discard(path_id)
        self._last_retry_block.pop(path_id, None)


def _hop_type_str(hop: HopInfo) -> str:
    """Return the Rust HopType string for a HopInfo variant."""
    if isinstance(hop, V2HopInfo):
        return "V2"
    if isinstance(hop, V3HopInfo):
        return "V3"
    return "V4"


def _hop_display_addr(hop: HopInfo) -> str:
    """Return a short display address for logging."""
    if isinstance(hop, V2HopInfo):
        return hop.pool_address
    if isinstance(hop, V3HopInfo):
        return hop.pool_address
    return hop.pool_id_hex


def _hop_token_summary(hops: tuple[HopInfo, ...]) -> str:
    """One-line summary of hop input→output tokens for sim-fail diagnostics."""
    parts: list[str] = []
    for h in hops:
        if isinstance(h, (V2HopInfo, V3HopInfo)):
            t0, t1 = h.token0_address, h.token1_address
        else:
            t0, t1 = h.currency0_address, h.currency1_address
        parts.append(f"{t0}→{t1}{'↗' if h.zfo else '↘'}")
    return " ".join(parts)


class EngineRegistry:
    """Thin wrapper over the Rust UniswapArbEngine.

    Maintains Python pool ↔ Rust key mappings so events can be routed
    to the right engine pool, and results can be mapped back to Python
    pool objects for encoding.

    The first pool in each path provides the flash borrow: V2 via
    uniswapV2Call, V3 via swapCallback, V4 via unlockCallback.

    V4 pools are keyed by pool_id hex string (sufficient since the bot
    uses a single PoolManager). The Rust engine additionally receives
    the pool_manager address at registration time.
    """

    def __init__(self) -> None:
        self.engine = UniswapArbEngine()
        self._v2_keys: dict[str, int] = {}  # address → key
        self._v3_keys: dict[str, int] = {}
        # V4 pools keyed by pool_id hex — for event routing from PoolManager logs
        self._v4_keys: dict[str, int] = {}  # pool_id_hex → key
        self.paths: dict[int, PathInfo] = {}
        # NOTE: These Python dicts (_v2_keys, _v3_keys, _v4_keys) are plain
        # dicts — NOT thread-safe. All access is on the single asyncio event loop.

    def register_v2_pool(self, pool: UniswapV2Pool) -> int:
        if pool.address in self._v2_keys:
            return self._v2_keys[pool.address]
        # Note: Only _fee_token0 is used. _fee_token1 is ignored.
        # Safe for Uniswap V2 and Sushiswap V2 (symmetric 0.3% fees on mainnet).
        # If asymmetric-fee V2 variants are added, this will silently use the
        # wrong fee for one direction. Full fix requires extending the Rust V2
        # engine's register_pool to accept two fee pairs (Plan 079 scope). See F3.
        if pool._fee_token0 != pool._fee_token1:
            bot_logger.warning(
                f"Asymmetric V2 fees detected for {pool.address} "
                f"(fee_token0={pool._fee_token0}, fee_token1={pool._fee_token1}). "
                f"Engine will use fee_token0 for both directions."
            )
        fee = pool._fee_token0
        key = self.engine.register_v2_pool(
            address=pool.address,
            reserve0=pool.reserves_token0,
            reserve1=pool.reserves_token1,
            gamma_numer=fee.denominator - fee.numerator,
            fee_denom=fee.denominator,
        )
        self._v2_keys[pool.address] = key
        return key

    def register_v3_pool(
        self,
        pool: UniswapV3Pool,
        block: int = 0,
    ) -> int:
        """Register a V3 pool with the Rust engine.

        Tick data is resolved automatically by the Rust engine from
        the streamed snapshot (loaded via stream_v3_snapshot_to_engine).
        The engine applies buffered events on top of stale snapshot data.

        """
        if pool.address in self._v3_keys:
            return self._v3_keys[pool.address]

        key = self.engine.register_v3_pool(
            address=pool.address,
            token0=pool.token0.address,
            token1=pool.token1.address,
            fee=pool.fee,
            tick_spacing=pool.tick_spacing,
            factory=pool.factory,
            sqrt_price_x96=pool.sqrt_price_x96,
            liquidity=pool.liquidity,
            tick=pool.tick,
            block=block,
        )
        self._v3_keys[pool.address] = key
        return key

    def register_v4_pool(
        self,
        pool: UniswapV4Pool,
        block: int = 0,
    ) -> int:
        """Register a V4 pool with the Rust engine.

        Tick data is resolved automatically by the Rust engine from
        the streamed snapshot (loaded via stream_v4_snapshot_to_engine).
        The engine applies buffered events on top of stale snapshot data.

        Performs hook filtering at registration:
        - Pools with amount-modifying hooks (mask 0xCC) are rejected
        - Pools with dynamic fees (fee == 0x100000) are rejected

        These pools violate the solver's assumption that V3 CL math
        applies exactly, so they would produce phantom profits.

        """
        pool_id_hex = pool.pool_id.to_0x_hex()

        if pool_id_hex in self._v4_keys:
            return self._v4_keys[pool_id_hex]

        # Hook filtering: reject pools with amount-modifying hooks
        # V4 hook flags are stored in the bottom 12 bits of the hook address
        hook_flags = int(pool.hook_address, 16) & 0xFFF
        if hook_flags & AMOUNT_MODIFYING_HOOK_MASK != 0:
            msg = (
                f"V4 pool {pool} has amount-modifying hooks "
                f"(hook_address={pool.hook_address}, flags=0x{hook_flags:04x})"
            )
            raise ValueError(msg)

        # Dynamic fee filtering: reject pools with dynamic fees
        if pool.fee == V4_DYNAMIC_FEE_FLAG:
            msg = f"V4 pool {pool} has dynamic fees (fee=0x{pool.fee:x})"
            raise ValueError(msg)

        key = self.engine.register_v4_pool(
            pool_manager=pool.address,
            pool_id_hex=pool_id_hex,
            currency0=pool.token0.address,
            currency1=pool.token1.address,
            fee=pool.fee,
            tick_spacing=pool.tick_spacing,
            hook_flags=hook_flags,
            sqrt_price_x96=pool.sqrt_price_x96,
            liquidity=pool.liquidity,
            tick=pool.tick,
            block=block,
        )

        self._v4_keys[pool_id_hex] = key
        return key

    def knows_pool(self, address: str) -> bool:
        return address in self._v2_keys or address in self._v3_keys

    def knows_v4_pool(self, pool_id_hex: str) -> bool:
        return pool_id_hex in self._v4_keys

    def register_path(self, hops: list[HopInfo]) -> int:
        """Register a path from a list of HopInfo objects.

        Uses register_and_solve_path for eager solving so the path
        is immediately included in the next result batch.
        """
        engine_hops = []
        for hop in hops:
            if isinstance(hop, V2HopInfo):
                fwd_key = self._v2_keys.get(hop.pool_address)
                key = fwd_key if hop.zfo else fwd_key + 1
            elif isinstance(hop, V4HopInfo):
                fwd_key = self._v4_keys.get(hop.pool_id_hex)
                key = fwd_key if hop.zfo else fwd_key + 1
            else:
                key = self._v3_keys.get(hop.pool_address)
            if key is None:
                msg = f"Pool not registered: {hop}"
                raise ValueError(msg)
            pool_type = _hop_type_str(hop)
            engine_hops.append((pool_type, key, hop.zfo))

        path_id = self.engine.register_and_solve_path(engine_hops)
        self.paths[path_id] = PathInfo(hops=hops)
        return path_id


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


async def build_paths(
    *,
    bot: Bot,
    engine_registry: EngineRegistry,
    v3_snapshot: UniswapV3LiquiditySnapshot | None = None,
    v4_snapshot: UniswapV4LiquiditySnapshot | None = None,
) -> None:
    """Discover V2/V3/V4 arb paths, build Python pools, register with Rust engine.

    V4 pools are discovered via find_paths_async and built through
    bot.build_managed_pool(). Hook filtering rejects pools with amount-modifying
    hooks (mask 0xCC) and dynamic fees (fee == 0x100000) at registration time.

    Tick data for V3/V4 engine registration is resolved automatically from
    the stored binary snapshots (already streamed via
    stream_v3_snapshot_to_engine/stream_v4_snapshot_to_engine
    in main() before backfill). The Rust engine applies buffered events on top
    of stale snapshot data to bring it current. Verification is handled internally
    by the engine (verify_on_register) — the tick data snapshot is taken while the
    engine lock is held, eliminating the race that existed with Python-side async
    verification.
    """
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
    v4_pool_count = 0
    v4_hook_rejected = 0
    v4_dynamic_fee_rejected = 0
    v4_other_value_error = 0
    other_exc_count = 0
    registered_path_sigs: set[tuple[str, ...]] = set()

    start = time.perf_counter()

    bot_logger.info("[build_paths] Calling find_paths_async...")
    pool_type_per_depth = _parse_permutation_filter(PATH_PERMUTATION_FILTER)
    pool_types = _pool_types_from_filter(PATH_PERMUTATION_FILTER)
    if pool_type_per_depth is not None:
        bot_logger.info(
            f"[build_paths] Permutation filter active: {PATH_PERMUTATION_FILTER} → depths={pool_type_per_depth}"
        )
    bot_logger.info(f"[build_paths] Pool types: {[t.__name__ for t in pool_types]}")
    async for path_steps in find_paths_async(  # noqa:PLR1702
        chain_id=bot.connections.default_chain_id,
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
                except Exception as exc:
                    bot_logger.debug(f"Skip V4 {step.hash}: {exc}")
                    skip = True
                    break
            else:
                skip = True
                break
            pools.append(pool)

        if skip:
            skip_count += 1
            continue

        # Register with Rust engine
        try:
            for pool, pt in zip(pools, pool_type_strs, strict=True):
                if pt == "V2":
                    engine_registry.register_v2_pool(pool)
                elif pt == "V3":
                    engine_registry.register_v3_pool(pool)
                elif pt == "V4":
                    v4_pool_count += 1
                    engine_registry.register_v4_pool(pool)
        except ValueError as exc:
            # Hook filtering / dynamic fee rejection — expected, skip path
            exc_str = str(exc)
            if "amount-modifying hooks" in exc_str:
                v4_hook_rejected += 1
            elif "dynamic fees" in exc_str:
                v4_dynamic_fee_rejected += 1
            else:
                v4_other_value_error += 1
                if v4_other_value_error <= 5:
                    bot_logger.info(f"[build_paths] V4 ValueError (other): {exc}")
            engine_reject_count += 1
            continue
        except RuntimeError as exc:
            # Verification failure — tick data mismatch. This is fatal.
            # The engine's verify_on_register flag causes RuntimeError to be
            # raised when on-chain tick state doesn't match the engine state.
            exc_str = str(exc)
            if "tick data mismatch" in exc_str:
                bot_logger.critical(f"[build_paths] VERIFICATION FAILURE — shutting down: {exc}")
                raise
            # Other RuntimeErrors (e.g. phase violations) — skip path
            engine_reject_count += 1
            other_exc_count += 1
            bot_logger.info(
                f"[build_paths] Engine registration failed ({type(exc).__name__}): {exc}"
            )
            continue
        except Exception as exc:
            engine_reject_count += 1
            other_exc_count += 1
            bot_logger.info(
                f"[build_paths] Engine registration failed ({type(exc).__name__}): {exc}"
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

        try:  # noqa:PLW0717
            hops = []
            for p, pt, zfo in zip(pools, pool_type_strs, zfo_list, strict=True):
                if pt == "V2":
                    fwd_key = engine_registry._v2_keys[p.address]
                    key = fwd_key if zfo else fwd_key + 1
                    hops.append(
                        V2HopInfo(
                            pool_key=key,
                            pool_address=p.address,
                            token0_address=p.token0.address,
                            token1_address=p.token1.address,
                            fee=int(p.fee_for_cache() * 10000),
                            zfo=zfo,
                        )
                    )
                elif pt == "V3":
                    key = engine_registry._v3_keys[p.address]
                    hops.append(
                        V3HopInfo(
                            pool_key=key,
                            pool_address=p.address,
                            token0_address=p.token0.address,
                            token1_address=p.token1.address,
                            fee=p.fee,
                            zfo=zfo,
                        )
                    )
                else:  # V4
                    pool_id_hex = p.pool_id.to_0x_hex()
                    fwd_key = engine_registry._v4_keys[pool_id_hex]
                    key = fwd_key if zfo else fwd_key + 1
                    hops.append(
                        V4HopInfo(
                            pool_key=key,
                            pool_manager_address=p.address,
                            pool_id_hex=pool_id_hex,
                            currency0_address=p.token0.address,
                            currency1_address=p.token1.address,
                            fee=p.fee,
                            tick_spacing=p.tick_spacing,
                            hook_address=p.hook_address,
                            zfo=zfo,
                        )
                    )
            engine_registry.register_path(hops)
        except Exception as exc:
            bot_logger.debug(f"Path registration failed: {exc}")
            continue

        path_count += 1
        if path_count % 1000 == 0:
            bot_logger.info(
                f"[build_paths] Progress: {path_count} paths registered, "
                f"{skip_count} skipped, {token_filter_count} token-filtered, "
                f"{engine_reject_count} engine-rejected, {dup_count} duplicates"
            )

    bot_logger.info(
        f"[build_paths] Path discovery complete: {path_count} paths in "
        f"{time.perf_counter() - start:.1f}s — "
        f"{skip_count} skipped, {token_filter_count} token-filtered, "
        f"{engine_reject_count} engine-rejected "
        f"(hooks={v4_hook_rejected}, dynamic_fee={v4_dynamic_fee_rejected}, "
        f"v4_other={v4_other_value_error}, other_exc={other_exc_count}), "
        f"{dup_count} duplicates"
    )
    bot_logger.info(
        f"[build_paths] Summary: {path_count} paths in "
        f"{time.perf_counter() - start:.1f}s — "
        f"{engine_registry.engine.v2_pool_count()} V2, "
        f"{engine_registry.engine.v3_pool_count()} V3, "
        f"{v4_pool_count} V4 pools, "
        f"{v4_hook_rejected} V4 hook-rejected, "
        f"{v4_dynamic_fee_rejected} V4 dynamic-fee-rejected, "
        f"{v4_other_value_error} V4 other-ValueError, "
        f"{other_exc_count} other-Exception, "
        f"{engine_registry.engine.path_count()} engine paths"
    )


def get_snapshots(
    bot: Bot,
) -> tuple[
    UniswapV3LiquiditySnapshot | None, UniswapV4LiquiditySnapshot | None, int | None, int | None
]:
    """Load V3 and V4 liquidity snapshots from the database.

    Snapshots provide tick_data for Python pool builds via trackers.
    The Rust engine backfills events from the snapshot block to the
    current chain head via backfill_from_snapshot().

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


def _compute_priority_fee(
    gross_profit: int,
    gas_used: int,
    base_fee_next: int,
    solve_block: int,
    current_block: int,
    block_priority_fees: dict[int, dict[int, int]],
) -> int:
    """Compute a market-aware priority fee with age decay.

    The fee is:
    1. Target fee: the priority fee that would achieve TARGET_PROFIT_RATIO
    2. Age decay: older results are worth less (exponential decay)
    3. Market bounds: clamped to feeHistory percentiles so we're competitive
       but not wasteful
    """
    # Target fee from profit ratio
    if gas_used <= 0:
        return 1
    target_priority_fee = max(
        1, int((gross_profit / TARGET_PROFIT_RATIO - gas_used * base_fee_next) / gas_used)
    )

    # Age decay: older results are worth less
    age = max(0, current_block - solve_block)
    age_factor = 1.0 / (1.0 + AGE_DECAY_CONSTANT * age)
    priority_fee = int(target_priority_fee * age_factor)

    # Market bounds from feeHistory
    min_priority_fee = 1
    max_priority_fee = target_priority_fee  # default ceiling
    if block_priority_fees:
        latest_fees = block_priority_fees[max(block_priority_fees)]
        p10 = latest_fees.get(MIN_PRIORITY_FEE_PERCENTILE, 0)
        p50 = latest_fees.get(MAX_PRIORITY_FEE_PERCENTILE, 0)
        min_priority_fee = max(p10 + 1, 1)  # At least 10th percentile + 1
        max_priority_fee = max(p50 + 1, min_priority_fee)  # At most 50th percentile + 1

    # Clamp to market bounds
    return max(
        min_priority_fee,
        min(priority_fee, max_priority_fee),
    )


@dataclasses.dataclass
class SubmittedTx:
    tx_hash: HexBytes
    nonce: int
    pools: set[int]  # Rust pool keys
    submission_block: int


async def monitor_pending_transaction(
    tx: SubmittedTx,
    async_w3: AsyncWeb3,
    pending_nonces: set[int],
    pending_pools: set[int],  # Rust pool keys
    current_block_ref: list[int],  # mutable [block_number] so we can read current
) -> None:
    """Monitor a submitted transaction until it confirms or expires.

    On confirmation: release the nonce and pools.
    On expiry (N blocks without inclusion): void the nonce and release pools.
    """
    while True:
        await asyncio.sleep(1)

        try:
            await async_w3.eth.get_transaction_receipt(tx.tx_hash)
        except TransactionNotFound:
            blocks_waited = current_block_ref[0] - tx.submission_block
            if blocks_waited > BLOCKS_BEFORE_NONCE_EXPIRES:
                pending_nonces.discard(tx.nonce)
                pending_pools.difference_update(tx.pools)
                bot_logger.info(
                    f"Voided expired nonce {tx.nonce} ({blocks_waited} blocks without inclusion)"
                )
                return
        else:
            pending_nonces.discard(tx.nonce)
            pending_pools.difference_update(tx.pools)
            bot_logger.info(f"Confirmed tx {tx.tx_hash.to_0x_hex()} nonce={tx.nonce}")
            return


async def dispatch_profitable_results(
    results: list[
        tuple[int, int, int, tuple[int, ...], tuple[int, ...], int]
    ],  # (path_id, opt_input, profit, hop_outputs, consumed_inputs, solve_block)
    engine_registry: EngineRegistry,
    async_w3: AsyncWeb3,
    executor_address: str,
    operator_address: str,
    operator_private_key: str,
    base_fee_next: int,
    current_block: int,
    operator_nonce: int,
    pending_nonces: set[int],
    pending_pools: set[int],
    active_tasks: set[asyncio.Task],
    current_block_ref: list[int],
    dry_run: bool,
    block_priority_fees: dict[int, dict[int, int]],
    path_suppression: PathSuppression,
) -> None:
    """Encode, simulate, and submit profitable results from the Rust engine.

    Pipeline (Slices 1-5):
    1. Sort by engine profit descending (Slice 4 — best-path first)
    2. Fan out parallel simulation with asyncio.gather (Slice 1)
    3. Each simulation: encode, simulate, gas from sim (Slice 5)
    4. Market-aware priority fee with age decay (Slice 3)
    5. Submit profit-descending with mutual exclusivity (Slice 4)

    Simulation uses a 7-call pattern:
      1. WETH balanceOf(executor) — before
      2. Multicall3.getEthBalance(executor) — before
      3. PM.balanceOf(executor, uint160(WETH)) — ERC-6909 before
      4. execute(commands)
      5. WETH balanceOf(executor) — after
      6. Multicall3.getEthBalance(executor) — after
      7. PM.balanceOf(executor, uint160(WETH)) — ERC-6909 after

    Gross profit = (weth + eth + erc6909)_after - (weth + eth + erc6909)_before.
    This correctly measures profit in WETH (physical ERC-20), native ETH,
    and ERC-6909 WETH held inside the PoolManager (from V4_MINT_COMPACT).

    Submitted transactions are tracked via monitor_pending_transaction tasks
    that release nonces and pools on confirmation or expiry.
    """
    bot_logger.info(f"[dispatch] entered with {len(results)} results, dry_run={dry_run}")

    # Per-dispatch trace dedup — prevents log spam from debug_traceCall
    _traced_reverts_local: set[tuple[int, str]] = set()
    # One-shot dump per V4-hybrid path type (V4-V2, V2-V4, V4-V4)
    _dumped_path_types: set[str] = set()
    # Per-dump deduplication for JSONL state dumps
    _state_dump_keys: set[tuple[int, int]] = set()

    _executor_contract = async_w3.eth.contract(
        address=executor_address,
        abi=EXECUTOR_ABI,
    )

    # Pre-build the balanceOf call for the executor
    weth_balance_calldata = encode_balanceof_calldata(executor_address)
    weth_balance_call = TxParams(
        to=WETH_ADDRESS,
        data=weth_balance_calldata,
    )
    # Pre-build the getEthBalance call for the executor (via Multicall3)
    get_eth_balance_selector = web3.Web3.keccak(text="getEthBalance(address)")[:4]
    eth_balance_calldata = get_eth_balance_selector + eth_abi.abi.encode(
        types=["address"],
        args=[executor_address],
    )
    eth_balance_call = TxParams(
        to=MULTICALL3_ADDRESS,
        data=eth_balance_calldata,
    )

    # Pre-build the ERC-6909 balanceOf call for the executor in the PM.
    # PM.balanceOf(executor, uint160(WETH_ADDRESS)) returns the executor's
    # ERC-6909 token balance for WETH held inside the PoolManager.
    # This detects profit stored via V4_MINT_COMPACT (no physical transfer)
    # alongside the existing WETH balanceOf check (physical ERC-20 transfer).
    pm_balanceof_selector = web3.Web3.keccak(text="balanceOf(address,uint256)")[:4]
    weth_erc6909_id = int.from_bytes(Web3.to_bytes(hexstr=WETH_ADDRESS)[-20:], byteorder="big")
    erc6909_balance_calldata = pm_balanceof_selector + eth_abi.abi.encode(
        types=["address", "uint256"],
        args=[executor_address, weth_erc6909_id],
    )
    erc6909_balance_call = TxParams(
        to=UNISWAP_V4_POOL_MANAGER_ADDRESS,
        data=erc6909_balance_calldata,
    )

    # ── Slice 4: Sort by engine profit descending — best paths first ──
    results.sort(key=operator.itemgetter(2), reverse=True)

    # ── Slice 4: Mutual exclusivity — pools already claimed by this dispatch ──
    committed_pools: set[int] = set()

    # ── Slice 1: Parallel simulation ──────────────────────────────────────
    # Budget for INFO-level logging of simulation failures (same pattern as
    # exceptions). Once exhausted, failures are logged at DEBUG only.
    _sim_info_budget = [5]

    def _sim_log(msg: str) -> None:
        if _sim_info_budget[0] > 0:
            _sim_info_budget[0] -= 1
            bot_logger.info(msg)
        else:
            bot_logger.debug(msg)

    def _write_jsonl(file_path: Path, obj: dict[str, Any]) -> None:
        """Synchronous JSONL writer helper for asyncio.to_thread."""
        file_path.parent.mkdir(parents=True, exist_ok=True)
        with file_path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(obj, default=str) + "\n")

    async def _dump_state_for_revert(
        path_id: int,
        solve_block: int,
        path_type: str,
        revert_info: str,
        calldata: str,
    ) -> None:
        """Write a JSONL engine/on-chain state dump after a simulation revert.

        Deduplicated by (path_id, solve_block) so the same failing path only
        produces one dump per block.
        """
        key = (path_id, solve_block)
        if key in _state_dump_keys:
            return
        _state_dump_keys.add(key)

        try:
            snapshot = await asyncio.to_thread(
                engine_registry.engine.diagnostic_inspect_path,
                path_id,
            )
        except Exception as exc:
            bot_logger.debug(
                f"[state-dump] engine dump failed for path={path_id}: {exc}"
            )
            return

        snapshot["failed_calldata"] = calldata
        snapshot["revert_info"] = revert_info
        snapshot["path_type"] = path_type

        timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
        dump_file = STATE_DUMP_DIR / f"{timestamp}.jsonl"
        await asyncio.to_thread(_write_jsonl, dump_file, snapshot)
        bot_logger.debug(f"[state-dump] wrote {dump_file} for path={path_id}")

    async def simulate_one(
        path_id: int,
        optimal_input: int,
        engine_profit: int,
        hop_outputs: tuple[int, ...],
        consumed_inputs: tuple[int, ...],
        solve_block: int,
    ) -> tuple[int, int, int, int, dict, Any] | None:
        """Simulate a single path. Returns (path_id, gross_profit, net_profit, gas_used, tx_params, path_info) or None."""
        path_info = engine_registry.paths.get(path_id)
        if path_info is None:
            bot_logger.debug(f"[sim-none] path={path_id}: path_info missing")
            return None

        # Guard: hop_outputs must match the number of hops
        if len(hop_outputs) != len(path_info.hops):
            bot_logger.debug(
                f"[dispatch] skip path={path_id}: hop_outputs length "
                f"({len(hop_outputs)}) != hops ({len(path_info.hops)})"
            )
            return None

        # Slice 4: Mutual exclusivity with pending + committed pools \u2500\u2500
        path_pools = {h.pool_key for h in path_info.hops}
        if path_pools & (pending_pools | committed_pools):
            bot_logger.debug(f"[dispatch] skip path={path_id}: pools pending or committed")
            return None

        # ── int128 check: V4 BalanceDelta uses int128 per component ──
        # For any V4 hop, amountSpecified and the swap's output delta
        # must both fit int128. Reject early to avoid wasted encoding.
        for i, hop in enumerate(path_info.hops):
            if isinstance(hop, V4HopInfo):
                amount_specified = optimal_input if i == 0 else hop_outputs[i - 1]
                output_amount = hop_outputs[i]
                if not fits_int128(amount_specified) or not fits_int128(output_amount):
                    bot_logger.debug(
                        f"[sim-fail] path={path_id} {path_info.path_type}: "
                        f"int128 overflow at V4 hop[{i}] "
                        f"amount_specified={amount_specified} output={output_amount}"
                    )
                    return None

        # Encode as cmd_executor command stream
        cmd_bytes = encode_cmd_stream(
            path_info,
            optimal_input,
            hop_outputs,
            executor_address,
            UNISWAP_V4_POOL_MANAGER_ADDRESS,
            WETH_ADDRESS,
        )
        if cmd_bytes is None:
            msg = "Encoding command stream failed!"
            raise ValueError(msg)

        pool_addrs = "→".join(f"{_hop_display_addr(h)}(zfo={h.zfo})" for h in path_info.hops)
        bot_logger.debug(
            f"[sim-debug] path={path_id} {path_info.path_type}: "
            f"{pool_addrs} input={optimal_input} outputs={hop_outputs}"
        )

        # Detailed dump for first occurrence of each path type
        dump_type = path_info.path_type
        if dump_type not in _dumped_path_types:
            _dumped_path_types.add(dump_type)
            if dump_type == "V4-V2":
                hop_v4, hop_v2 = path_info.hops[0], path_info.hops[1]
                bot_logger.info(
                    f"[sim-dump] V4-V2 path={path_id} "
                    f"v4_c0={hop_v4.currency0_address} v4_c1={hop_v4.currency1_address} "
                    f"v4_zfo={hop_v4.zfo} v2_zfo={hop_v2.zfo} v2_pool={hop_v2.pool_address} "
                    f"input={optimal_input} fwd={hop_outputs[0]} out={hop_outputs[1]} "
                    f"cmd_len={len(cmd_bytes)}"
                )
            elif dump_type == "V2-V4":
                hop_v2, hop_v4 = path_info.hops[0], path_info.hops[1]
                v4_in_native = v4_input_is_native(hop_v4)
                bot_logger.info(
                    f"[sim-dump] V2-V4 path={path_id} "
                    f"v4_c0={hop_v4.currency0_address} v4_c1={hop_v4.currency1_address} "
                    f"v4_zfo={hop_v4.zfo} v2_zfo={hop_v2.zfo} v2_pool={hop_v2.pool_address} "
                    f"v4_native_in={v4_in_native} input={optimal_input} fwd={hop_outputs[0]} out={hop_outputs[1]} "
                    f"cmd_len={len(cmd_bytes)}"
                )
            elif dump_type == "V4-V4":
                hop_a, hop_b = path_info.hops[0], path_info.hops[1]
                bot_logger.info(
                    f"[sim-dump] V4-V4 path={path_id} "
                    f"a_c0={hop_a.currency0_address} a_c1={hop_a.currency1_address} "
                    f"a_zfo={hop_a.zfo} a_fee={hop_a.fee} "
                    f"b_c0={hop_b.currency0_address} b_c1={hop_b.currency1_address} "
                    f"b_zfo={hop_b.zfo} b_fee={hop_b.fee} "
                    f"input={optimal_input} fwd={hop_outputs[0]} out={hop_outputs[1]} "
                    f"cmd_len={len(cmd_bytes)}"
                )
            else:
                # Generic dump for all other path types (V3-V3-V3 etc.)
                _hop_parts = []
                for _h in path_info.hops:
                    _part = f"{_hop_display_addr(_h)}(zfo={_h.zfo}"
                    if isinstance(_h, V2HopInfo):
                        _part += f" t0={_h.token0_address} t1={_h.token1_address}"
                    elif isinstance(_h, V3HopInfo):
                        _part += f" t0={_h.token0_address} t1={_h.token1_address} fee={_h.fee}"
                    elif isinstance(_h, V4HopInfo):
                        _part += f" c0={_h.currency0_address} c1={_h.currency1_address} fee={_h.fee}"
                    _part += ")"
                    _hop_parts.append(_part)
                _amounts_str = ",".join(str(a) for a in hop_outputs)
                bot_logger.info(
                    f"[sim-dump] {dump_type} path={path_id} "
                    f"hops=[{' → '.join(_hop_parts)}] "
                    f"input={optimal_input} outputs=[{_amounts_str}] "
                    f"cmd_len={len(cmd_bytes)}"
                )
        # expected_balance=0 (check_mode=0): skip on-chain profit check,
        # operator verifies profitability off-chain via the pre/post balance reads.

        expected_balance = pack_expected_balance(check_mode=0, expected_value=0)
        selector = web3.Web3.keccak(text="execute(bytes,uint256)")[:4]
        calldata = selector + eth_abi.abi.encode(
            types=["bytes", "uint256"],
            args=[cmd_bytes, expected_balance],
        )
        bot_logger.info(
            f"[sim-dump] {dump_type} full_calldata_len={len(calldata)} cmd_stream_len={len(cmd_bytes)}"
        )
        tx_params = TxParams(
            to=Web3.to_checksum_address(executor_address),
            data=calldata,
            chainId=1,
            type=2,
            value=0,
            gas=5_000_000,  # Generous gas for V3 tick-crossing swaps
            maxFeePerGas=0,
            maxPriorityFeePerGas=0,
            nonce=0,
        )
        tx_params["from"] = get_checksum_address(EXECUTOR_OWNER)

        # Compute EIP-2930 access list before simulation so gas_used
        # reflects the savings from pre-warmed storage slots
        try:
            al_result = await async_w3.eth.create_access_list(tx_params, block_identifier="pending")
        except Exception as al_exc:
            # If AL computation fails (e.g. revert), simulate without it.
            # The simulation itself will reject this path.
            bot_logger.debug(f"[sim-debug] path={path_id} access list failed: {al_exc}")
        else:
            tx_params["accessList"] = al_result["accessList"]

        # Simulate with 7-call pattern
        try:
            # All override fields (code, balance, nonce, state, stateDiff)
            # go under stateOverrides per the Alloy AccountOverride spec.
            sim = await async_w3.eth.simulate_v1(
                payload=SimulateV1Payload(
                    blockStateCalls=[
                        BlockStateCallV1(
                            stateOverrides=build_simulation_state_overrides(
                                executor_owner=EXECUTOR_OWNER,
                                inject_code=INJECT_EXECUTOR_CODE,
                                injected_address=INJECTED_EXECUTOR_ADDRESS
                                if INJECT_EXECUTOR_CODE
                                else None,
                            ),
                            calls=[
                                weth_balance_call,  # [0] WETH balance before
                                eth_balance_call,  # [1] ETH balance before
                                erc6909_balance_call,  # [2] ERC-6909 WETH balance before
                                tx_params,  # [3] execute(commands)
                                weth_balance_call,  # [4] WETH balance after
                                eth_balance_call,  # [5] ETH balance after
                                erc6909_balance_call,  # [6] ERC-6909 WETH balance after
                            ],
                        )
                    ],
                ),
                block_identifier="pending",
            )
        except Web3Exception as e:
            _sim_log(
                f"[sim-fail] path={path_id} {path_info.path_type}: simulation RPC failed ({e})"
            )
            return None

        calls = sim[0]["calls"]

        # Check all calls succeeded — log which call failed + revert data
        failed_call = None
        for call_idx, c in enumerate(calls):  # noqa:PLR1702
            if c.get("status", 0) != 1:
                failed_call = c

                # eth_simulateV1 returns revert reason in error.data when
                # returnData is HexBytes(b'') (empty). The error field contains:
                #   AttributeDict(code=3, message='execution reverted: IIA',
                #                 data='0x08c379a0...')  (full ABI-encoded Error(string))
                # Fall back to error.data when returnData is empty.
                # Note: returnData may be HexBytes(b'') from web3.py's wrapper,
                # or raw string "0x" from make_request — handle both.
                revert_data = c.get("returnData", b"")
                if isinstance(revert_data, str):
                    # Raw RPC returns hex strings; normalize to bytes
                    if revert_data == "0x" or not revert_data:
                        revert_data = b""
                    else:
                        revert_data = bytes.fromhex(revert_data.removeprefix("0x"))
                elif isinstance(revert_data, bytes):
                    # HexBytes or regular bytes from web3.py wrapper
                    # HexBytes(b'') has len=0, bool=False — already empty
                    if revert_data == HexBytes("0x") or len(revert_data) == 0:
                        revert_data = b""
                else:
                    revert_data = b""
                _error_obj = c.get("error")
                _error_msg_from_obj = ""  # from error.message as last resort
                # web3.py wraps the error field as AttributeDict (Mapping, NOT
                # a dict subclass) so isinstance(_error_obj, dict) is False.
                # Use dict-style access which works for both dict and AttributeDict.
                if not revert_data and _error_obj is not None:
                    try:
                        _error_data = _error_obj["data"]
                    except (KeyError, TypeError, IndexError):
                        _error_data = None
                    if _error_data and isinstance(_error_data, str) and _error_data.startswith("0x") and len(_error_data) > 2:
                        revert_data = bytes.fromhex(_error_data[2:])
                    else:
                        try:
                            _error_msg_from_obj = _error_obj["message"]
                        except (KeyError, TypeError, IndexError):
                            pass
                # revert_data is always bytes now (normalized above)
                revert_hex = revert_data.hex()

                # Log full call result for V4 empty reverts
                if path_info.path_type in {"V4-V4", "V2-V4", "V4-V2"} and not revert_hex:
                    bot_logger.debug(
                        f"[sim-raw] path={path_id} {path_info.path_type}: call[{call_idx}] DUMP={c}"
                    )
                # Decode common revert patterns
                revert_reason = ""
                _V4_ERRORS = {
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
                _EXECUTOR_ERRORS = {
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
                if len(revert_hex) >= 8:
                    selector = revert_hex[:8]
                    if selector == "4e487b71":  # Panic
                        revert_reason = (
                            f" PANIC(0x{revert_hex[8:]})" if len(revert_hex) > 8 else " PANIC"
                        )
                    elif selector == "08c379a0":  # Error(string)
                        try:
                            # ABI: [selector:8][offset:64][length:64][data:N]
                            # offset is always 0x20 (32) for direct encoding
                            str_len_bytes = int(revert_hex[8 + 64 : 8 + 128], 16)
                            str_start = 8 + 64 + 64  # hex offset to string data
                            str_len_hex = str_len_bytes * 2  # byte length → hex chars
                            msg_bytes = bytes.fromhex(
                                revert_hex[str_start : str_start + str_len_hex]
                            )
                            revert_reason = f" {msg_bytes.decode('utf-8', errors='replace')}"
                        except Exception:
                            pass
                    elif selector in _V4_ERRORS:
                        revert_reason = f" {_V4_ERRORS[selector]}"
                    elif selector in _EXECUTOR_ERRORS:
                        revert_reason = f" {_EXECUTOR_ERRORS[selector]}"
                        # Decode ABI-encoded params for parameterized executor errors
                        params_hex = revert_hex[8:]
                        if (
                            selector == "4e88422a" and len(params_hex) >= 128
                        ):  # InsufficientProfit(uint256,uint256)
                            try:
                                actual = int(params_hex[0:64], 16)
                                expected = int(params_hex[64:128], 16)
                                revert_reason = (
                                    f" InsufficientProfit(actual={actual},expected={expected})"
                                )
                            except (ValueError, IndexError):
                                pass
                        elif (
                            selector == "cf479181" and len(params_hex) >= 128
                        ):  # InsufficientBalance(uint256,uint256)
                            try:
                                amount = int(params_hex[0:64], 16)
                                available = int(params_hex[64:128], 16)
                                revert_reason = (
                                    f" InsufficientBalance(amount={amount},available={available})"
                                )
                            except (ValueError, IndexError):
                                pass
                        elif (
                            selector == "8e4a23d6" and len(params_hex) >= 64
                        ):  # Unauthorized(address)
                            try:
                                addr = int(params_hex[0:64], 16)
                                revert_reason = f" Unauthorized(caller=0x{addr:040x})"
                            except (ValueError, IndexError):
                                pass
                        elif (
                            selector == "b028a63a" and len(params_hex) >= 64
                        ):  # InvalidCallback(address)
                            try:
                                addr = int(params_hex[0:64], 16)
                                revert_reason = f" InvalidCallback(caller=0x{addr:040x})"
                            except (ValueError, IndexError):
                                pass
                        elif (
                            selector == "83276224" and len(params_hex) >= 64
                        ):  # InvalidCommand(uint256)
                            try:
                                opcode = int(params_hex[0:64], 16)
                                revert_reason = f" InvalidCommand(opcode={opcode}/0x{opcode:02x})"
                            except (ValueError, IndexError):
                                pass
                        elif (
                            selector == "60ef0bb0" and len(params_hex) >= 64
                        ):  # BipsTooHigh(uint256)
                            try:
                                bips = int(params_hex[0:64], 16)
                                revert_reason = f" BipsTooHigh(bips={bips})"
                            except (ValueError, IndexError):
                                pass
                    elif revert_hex.startswith(
                        "00000000000000000000000000000000000000000000000000000000"
                    ):
                        # Numeric revert
                        revert_reason = f" num=0x{revert_hex[24:]}"
                    else:
                        revert_reason = f" sel=0x{selector}"
                elif not revert_hex and _error_msg_from_obj:
                    revert_reason = f" {_error_msg_from_obj}"
                _sim_log(
                    f"[sim-fail] path={path_id} {path_info.path_type}: "
                    f"call[{call_idx}] failed (gasUsed={c.get('gasUsed', 0)}) "
                    f"revert=0x{revert_hex}{revert_reason} "
                    f"block={current_block} age={current_block - solve_block} "
                    f"tokens=[{_hop_token_summary(path_info.hops)}]"
                )

                # ── Diagnostic: on-chain state for sim-fail paths ──
                # Only check once per block+type to avoid log spam
                _trace_key = (current_block, path_info.path_type)
                if (
                    failed_call is not None
                    and _trace_key not in _traced_reverts_local
                ):
                    # On-chain state at 'pending' — matches what the
                    # simulation sees. hex(current_block) would give state
                    # after block N-1, which differs when current-block txs
                    # modify pool state.
                    amounts_per_hop = [optimal_input] + list(hop_outputs)
                    for hi, hop in enumerate(path_info.hops):
                        hop_amount = amounts_per_hop[hi] if hi < len(amounts_per_hop) else "?"
                        if isinstance(hop, V2HopInfo):
                            pool_addr = hop.pool_address
                            try:
                                reserves = await async_w3.eth.call(
                                    {
                                        "to": Web3.to_checksum_address(pool_addr),
                                        "data": web3.Web3.keccak(text="getReserves()")[:4],
                                    },
                                    "pending",
                                )
                                if len(reserves) >= 96:
                                    r0 = int.from_bytes(reserves[0:32], "big")
                                    r1 = int.from_bytes(reserves[32:64], "big")
                                    bot_logger.info(
                                        f"[v2-state] path={path_id} hop[{hi}] "
                                        f"pool={pool_addr} "
                                        f"reserve0={r0} reserve1={r1} "
                                        f"token0={hop.token0_address} token1={hop.token1_address} "
                                        f"zfo={hop.zfo} amount={hop_amount}"
                                    )
                            except Exception:
                                pass
                        elif isinstance(hop, V3HopInfo):
                            pool_addr = hop.pool_address
                            try:
                                # Check pool has code at pending
                                _code = await async_w3.eth.get_code(
                                    Web3.to_checksum_address(pool_addr),
                                    "pending",
                                )
                                if not _code or _code == b"\x00" or (isinstance(_code, str) and _code in ("0x", "0x00")):
                                    bot_logger.info(
                                        f"[v3-state] path={path_id} hop[{hi}] "
                                        f"pool={pool_addr} NO CODE at block {current_block}"
                                    )
                                    continue
                                slot0_data = await async_w3.eth.call(
                                    {
                                        "to": Web3.to_checksum_address(pool_addr),
                                        "data": web3.Web3.keccak(text="slot0()")[:4],
                                    },
                                    "pending",
                                )
                                liq_data = await async_w3.eth.call(
                                    {
                                        "to": Web3.to_checksum_address(pool_addr),
                                        "data": web3.Web3.keccak(text="liquidity()")[:4],
                                    },
                                    "pending",
                                )
                                if len(slot0_data) < 64:
                                    bot_logger.info(
                                        f"[v3-state] path={path_id} hop[{hi}] "
                                        f"pool={pool_addr} slot0 returned {len(slot0_data)} bytes (expected 96+)"
                                    )
                                    continue
                                sqrt_price = int.from_bytes(slot0_data[0:32], "big")
                                tick = int.from_bytes(slot0_data[32:64], "big")
                                if tick >= 2**255:
                                    tick -= 2**256  # signed int24
                                liq = int.from_bytes(liq_data[0:32], "big") if len(liq_data) >= 32 else 0
                                bot_logger.info(
                                    f"[v3-state] path={path_id} hop[{hi}] "
                                    f"pool={pool_addr} "
                                    f"sqrtPriceX96={sqrt_price} tick={tick} "
                                    f"liquidity={liq} fee={hop.fee} "
                                    f"token0={hop.token0_address} token1={hop.token1_address} "
                                    f"zfo={hop.zfo} amount={hop_amount}"
                                )
                            except Exception:
                                pass
                        elif isinstance(hop, V4HopInfo):
                            # V4 pool state lives in PoolManager — we can't
                            # easily query slot0 via eth_call on the PM.
                            # Log what we have from the hop metadata.
                            bot_logger.info(
                                f"[v4-state] path={path_id} hop[{hi}] "
                                f"pm={hop.pool_manager_address} pool_id={hop.pool_id_hex} "
                                f"c0={hop.currency0_address} c1={hop.currency1_address} "
                                f"fee={hop.fee} tick_spacing={hop.tick_spacing} "
                                f"hook={hop.hook_address} "
                                f"zfo={hop.zfo} amount={hop_amount}"
                            )

                    _traced_reverts_local.add(_trace_key)

                # Log the revert calldata for manual debugging (always INFO)
                # Always log full calldata for sim-failures so we can
                # reproduce offline with debug_traceCall or cast call
                _cd = tx_params.get("data", tx_params.get("input", b""))
                if isinstance(_cd, bytes):
                    _cd = "0x" + _cd.hex()
                elif isinstance(_cd, str):
                    _cd = _cd if _cd.startswith("0x") else "0x" + _cd

                if STATE_DUMP_ON_REVERT:
                    await _dump_state_for_revert(
                        path_id=path_id,
                        solve_block=solve_block,
                        path_type=path_info.path_type,
                        revert_info=f"0x{revert_hex}{revert_reason}",
                        calldata=_cd,
                    )

                bot_logger.info(
                    f"[sim-revert-data] path={path_id} {path_info.path_type} "
                    f"revert=0x{revert_hex[:16]}{'...' if len(revert_hex) > 16 else ''}{revert_reason} "
                    f"block={current_block} age={current_block - solve_block} "
                    f"calldata={_cd}"
                )

                return None
        try:
            weth_before = int.from_bytes(calls[0]["returnData"], byteorder="big")
            eth_before = int.from_bytes(calls[1]["returnData"], byteorder="big")
            erc6909_before = int.from_bytes(calls[2]["returnData"], byteorder="big")
            weth_after = int.from_bytes(calls[4]["returnData"], byteorder="big")
            eth_after = int.from_bytes(calls[5]["returnData"], byteorder="big")
            erc6909_after = int.from_bytes(calls[6]["returnData"], byteorder="big")
        except (IndexError, ValueError):
            _sim_log(f"[sim-fail] path={path_id} {path_info.path_type}: balance decode failed")
            return None

        combined_before = weth_before + eth_before + erc6909_before
        combined_after = weth_after + eth_after + erc6909_after
        gross_profit = combined_after - combined_before
        if gross_profit <= 0:
            _sim_log(
                f"[sim-fail] path={path_id} {path_info.path_type}: no profit (gross={gross_profit}) "
                f"weth_before={weth_before} weth_after={weth_after} "
                f"eth_before={eth_before} eth_after={eth_after} "
                f"erc6909_before={erc6909_before} erc6909_after={erc6909_after}"
            )
            return None

        # ── Slice 5: Gas estimation from simulation ──────────────────
        # Use the simulation's actual gasUsed with a 1.5x safety margin.
        gas_used = calls[3]["gasUsed"]
        tx_params["gas"] = int(gas_used * 1.5)

        # ── Slice 3: Market-aware priority fee with age decay ────────────
        priority_fee = _compute_priority_fee(
            gross_profit=gross_profit,
            gas_used=gas_used,
            base_fee_next=base_fee_next,
            solve_block=solve_block,
            current_block=current_block,
            block_priority_fees=block_priority_fees,
        )

        gas_fee = gas_used * (base_fee_next + priority_fee)
        net_profit = gross_profit - gas_fee

        # Return all gross-profitable results — the caller separates
        # gas-profitable from gas-unprofitable but onchain-valid.
        return (path_id, gross_profit, net_profit, gas_used, tx_params, path_info)

    # ── Fan out parallel simulation (Slice 1) ──────────────────────────
    # Pre-filter: remove suppressed paths before simulation
    pre_filter_count = len(results)
    results = [
        (pid, inp, pft, ho, ci, sb)
        for pid, inp, pft, ho, ci, sb in results
        if not path_suppression.is_suppressed(pid, current_block)
    ]
    suppressed_count = pre_filter_count - len(results)
    if suppressed_count > 0:
        bot_logger.info(
            f"[dispatch] {suppressed_count}/{pre_filter_count} results filtered by suppression"
        )

    candidates = results[:MAX_SIMULATE_CONCURRENT]
    # Log candidate summary for observability
    cand_types = {}
    for pid, _inp, _pft, _ho, _ci, _sb in candidates:
        pi = engine_registry.paths.get(pid)
        pt = pi.path_type if pi else "?"
        cand_types[pt] = cand_types.get(pt, 0) + 1
    cand_types_str = " ".join(f"{k}={v}" for k, v in sorted(cand_types.items()))
    bot_logger.info(
        f"[dispatch] simulating {len(candidates)}/{len(results)} candidates: {cand_types_str}"
    )
    # Track simulation outcomes for path suppression
    _sim_outcomes: dict[int, bool] = {}  # path_id -> succeeded

    sim_tasks = list(itertools.starmap(simulate_one, candidates))
    sim_results = await asyncio.gather(*sim_tasks, return_exceptions=True)

    # ── Categorize simulation results ────────────────────────────────
    # Separate into gas-profitable (net >= MIN_PROFIT_NET) and
    # onchain-valid but gas-unprofitable (gross > 0, net below threshold).
    gas_profitable: list[tuple[int, int, int, int, dict, Any]] = []
    gas_unprofitable: list[tuple[int, int, int, int, dict, Any]] = []
    exception_count = 0
    _exc_log_limit = 5
    for result in sim_results:
        if isinstance(result, Exception):
            exception_count += 1
            if _exc_log_limit > 0:
                _exc_log_limit -= 1
                bot_logger.info(
                    f"[sim-exc] simulation exception: {result}\n"
                    + "".join(
                        traceback.format_exception(type(result), result, result.__traceback__)
                    )
                )
            else:
                bot_logger.debug(
                    f"[sim-fail] simulation exception: {result}\n"
                    + "".join(
                        traceback.format_exception(type(result), result, result.__traceback__)
                    )
                )
            continue
        if result is None:
            continue
        path_id, gross_profit, net_profit, gas_used, tx_params, path_info = result
        if net_profit >= MIN_PROFIT_NET:
            gas_profitable.append(result)
        else:
            gas_unprofitable.append(result)

    # ── Summary log ──────────────────────────────────────────────────
    sim_ok_count = len(gas_profitable) + len(gas_unprofitable)
    sim_fail_count = len(candidates) - sim_ok_count - exception_count
    best_net = max((r[2] for r in gas_profitable), default=0)
    bot_logger.info(
        f"[sim] {len(candidates)} candidates: "
        f"{sim_ok_count} ok ({len(gas_profitable)} profitable, "
        f"{len(gas_unprofitable)} below threshold), "
        f"{sim_fail_count} failed, {exception_count} exceptions"
        f"{f' — best net={best_net // 10**9}gwei' if gas_profitable else ''}"
    )

    # ── Record simulation outcomes for path suppression ──────────
    # Successful paths reset their failure counter; failed paths increment it.
    succeeded_path_ids: set[int] = set()
    succeeded_path_ids.update(result[0] for result in gas_profitable + gas_unprofitable)
    failed_count = len(candidates) - len(succeeded_path_ids)
    if failed_count > 0:
        bot_logger.debug(
            f"[suppress] {failed_count}/{len(candidates)} candidates failed, {path_suppression.total_suppressed} total suppressed"
        )
    for pid, _inp, _pft, _ho, _ci, _sb in candidates:
        if pid in succeeded_path_ids:
            path_suppression.record_success(pid)
        else:
            path_suppression.record_failure(pid)

    # Sort both categories by net profit descending
    gas_profitable.sort(key=operator.itemgetter(2), reverse=True)
    gas_unprofitable.sort(key=operator.itemgetter(2), reverse=True)

    # ── Log gas-profitable results (rare, verbose for offline inspection) ──
    for path_id, gross_profit, net_profit, gas_used, _tx_params, path_info in gas_profitable:
        hop_details = []
        for i, h in enumerate(path_info.hops):
            if isinstance(h, V2HopInfo):
                hop_details.append(
                    f"  hop[{i}] V2 pool_key={h.pool_key} addr={h.pool_address} "
                    f"t0={h.token0_address} t1={h.token1_address} "
                    f"fee={h.fee} zfo={h.zfo}"
                )
            elif isinstance(h, V3HopInfo):
                hop_details.append(
                    f"  hop[{i}] V3 pool_key={h.pool_key} addr={h.pool_address} "
                    f"t0={h.token0_address} t1={h.token1_address} "
                    f"fee={h.fee} zfo={h.zfo}"
                )
            elif isinstance(h, V4HopInfo):
                hop_details.append(
                    f"  hop[{i}] V4 pool_key={h.pool_key} pm={h.pool_manager_address} "
                    f"pid={h.pool_id_hex} "
                    f"c0={h.currency0_address} c1={h.currency1_address} "
                    f"fee={h.fee} ts={h.tick_spacing} zfo={h.zfo}"
                )
        hops_str = "\n".join(hop_details)
        bot_logger.info(
            f"[profit] path={path_id} {path_info.path_type} "
            f"gross={gross_profit / 1e18:.6f}ETH ({gross_profit // 10**9}gwei) "
            f"net={net_profit / 1e18:.6f}ETH ({net_profit // 10**9}gwei) "
            f"gas={gas_used}\n{hops_str}"
        )

    # Log gas-unprofitable but onchain-valid results at debug level
    for path_id, gross_profit, net_profit, gas_used, _tx_params, path_info in gas_unprofitable:
        bot_logger.debug(
            f"[sim-ok] path={path_id} {path_info.path_type} "
            f"gross={gross_profit / 1e18:.6f}ETH "
            f"net={net_profit / 1e18:.6f}ETH "
            f"gas={gas_used} "
            f"gross_gwei={gross_profit // 10**9}gwei "
            f"net_gwei={net_profit // 10**9}gwei"
        )

    # ── Submit gas-profitable with mutual exclusivity (Slice 4) ────
    for path_id, gross_profit, net_profit, gas_used, tx_params, path_info in gas_profitable:
        # ── Slice 4: mutual exclusivity ──
        path_pools = {h.pool_key for h in path_info.hops}
        if path_pools & (pending_pools | committed_pools):
            bot_logger.debug(f"[dispatch] skip path={path_id}: pools claimed after sim")
            continue

        # Compute final priority fee (re-evaluate with current state)
        priority_fee = _compute_priority_fee(
            gross_profit=gross_profit,
            gas_used=gas_used,
            base_fee_next=base_fee_next,
            solve_block=0,  # Already passed staleness check; no age decay at submission
            current_block=current_block,
            block_priority_fees=block_priority_fees,
        )
        net_profit = gross_profit - gas_used * (base_fee_next + priority_fee)

        bot_logger.info(
            f"[dispatch] path={path_id} "
            f"gross={gross_profit // 10**9}gwei "
            f"net={net_profit // 10**9}gwei "
            f"gas={gas_used} "
            f"prio={priority_fee // 10**9}gwei"
        )

        if dry_run:
            committed_pools.update(path_pools)
            continue

        # Safety: never submit a real transaction when using code injection
        # (the injected contract doesn't exist on-chain)
        if INJECT_EXECUTOR_CODE:
            bot_logger.warning(
                f"[dispatch] path={path_id}: skipping submission — INJECT_EXECUTOR_CODE is active"
            )
            committed_pools.update(path_pools)
            continue

        # Submit
        nonce = next(n for n in itertools.count(operator_nonce) if n not in pending_nonces)
        tx_params["nonce"] = nonce
        tx_params["maxPriorityFeePerGas"] = priority_fee
        tx_params["maxFeePerGas"] = int(1.5 * base_fee_next) + priority_fee

        # Access list was computed during simulation. Re-compute with
        # updated nonce/fees for accuracy.
        try:
            al_result = await async_w3.eth.create_access_list(tx_params, block_identifier="pending")
            tx_params["accessList"] = al_result["accessList"]
        except Exception as al_exc:
            bot_logger.debug(f"[dispatch] access list re-computation failed: {al_exc}")

        try:
            tx_hash = await async_w3.eth.send_raw_transaction(
                eth_account.Account.sign_transaction(
                    transaction_dict=tx_params,
                    private_key=operator_private_key,
                ).raw_transaction,
            )
        except Web3Exception as exc:
            bot_logger.debug(f"Send failed: {exc}")
            continue

        bot_logger.info(f"Submitted path {path_id} hash={tx_hash.to_0x_hex()} nonce={nonce}")
        pending_nonces.add(nonce)
        pending_pools.update(path_pools)
        committed_pools.update(path_pools)

        # Spawn monitor task to release nonce + pools on confirmation/expiry
        task = asyncio.create_task(
            monitor_pending_transaction(
                tx=SubmittedTx(
                    tx_hash=tx_hash,
                    nonce=nonce,
                    pools=path_pools,
                    submission_block=current_block,
                ),
                async_w3=async_w3,
                pending_nonces=pending_nonces,
                pending_pools=pending_pools,
                current_block_ref=current_block_ref,
            )
        )
        active_tasks.add(task)
        task.add_done_callback(active_tasks.discard)


async def consume_result_batches(
    engine_registry: EngineRegistry,
    async_w3: AsyncWeb3,
    executor_address: str,
    operator_address: str,
    operator_private_key: str,
    pending_nonces: set[int],
    pending_pools: set[int],
    active_tasks: set[asyncio.Task],
    current_block_ref: list[int],
    dry_run: bool,
    block_times: deque[tuple[int, int]],
    block_priority_fees: dict[int, dict[int, int]],
    path_suppression: PathSuppression,
) -> None:
    """Consume result batches from the Rust engine and dispatch profitable ones.

    Designed to run as a long-lived asyncio task — started before build_paths
    so eagerly-solved paths are dispatched during path loading (rolling start),
    then continues as the permanent main loop after build_paths completes.
    """
    bot_logger.info("[consumer] Starting — awaiting result batches from Rust pump")

    async for batch in engine_registry.engine:
        block_number = batch["solve_block"]
        block_timestamp = batch["timestamp"]
        base_fee = batch.get("base_fee_per_gas") or 0
        gas_used = batch["gas_used"]
        gas_limit = batch["gas_limit"]

        base_fee_next = next_base_fee(
            parent_base_fee=base_fee,
            parent_gas_used=gas_used,
            parent_gas_limit=gas_limit,
        )
        operator_nonce = await async_w3.eth.get_transaction_count(operator_address)

        try:
            fee_history = await async_w3.eth.fee_history(
                block_count=1,
                newest_block=block_number,
                reward_percentiles=[float(p) for p in FEE_PERCENTILES],
            )
            reward = fee_history.get("reward", [[]])
            if reward and reward[-1]:
                block_priority_fees[block_number] = dict(
                    zip(
                        FEE_PERCENTILES,
                        reward[-1],
                        strict=True,
                    )
                )
                if len(block_priority_fees) > FEE_HISTORY_WINDOW:
                    block_priority_fees.pop(min(block_priority_fees))
        except Web3Exception:
            pass

        block_times.append((block_number, block_timestamp))
        if len(block_times) >= 2:
            oldest_bn, _oldest_ts = block_times[0]
            if block_number != oldest_bn:
                latency = time.time() - block_timestamp
                bot_logger.info(
                    f"[{block_number}][+{latency:.1f}s]"
                    f"[{base_fee / 10**9:.5f}/{base_fee_next / 10**9:.5f}]"
                )

        current_block_ref[0] = block_number

        # Build results list from fresh + updated entries in the batch
        results: list[tuple[int, int, int, tuple[int, ...], tuple[int, ...], int]] = []
        for item in batch["fresh"]:
            path_id, opt_input, profit, hop_outs, consumed_ins = item
            results.append((
                int(path_id),
                int(opt_input),
                int(profit),
                tuple(int(h) for h in hop_outs),
                tuple(int(c) for c in consumed_ins),
                block_number,
            ))
        for item in batch["updated"]:
            path_id, opt_input, profit, hop_outs, consumed_ins = item
            results.append((
                int(path_id),
                int(opt_input),
                int(profit),
                tuple(int(h) for h in hop_outs),
                tuple(int(c) for c in consumed_ins),
                block_number,
            ))

        # Expired: below threshold, still registered (may reappear)
        # for path_id in batch["expired"]:
        #     pass  # No action needed — suppression tracking persists

        # Removed: de-registered, permanently gone
        for path_id in batch["removed"]:
            path_suppression.discard(int(path_id))

        if results:
            await dispatch_profitable_results(
                results=results,
                engine_registry=engine_registry,
                async_w3=async_w3,
                executor_address=executor_address,
                operator_address=operator_address,
                operator_private_key=operator_private_key,
                base_fee_next=base_fee_next,
                current_block=block_number,
                operator_nonce=operator_nonce,
                pending_nonces=pending_nonces,
                pending_pools=pending_pools,
                active_tasks=active_tasks,
                current_block_ref=current_block_ref,
                dry_run=dry_run,
                block_priority_fees=block_priority_fees,
                path_suppression=path_suppression,
            )


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--live", action="store_true", help="Enable live mode (submits real transactions)"
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
    args = parser.parse_args()
    dry_run = not args.live

    # Override PATH_PERMUTATION_FILTER from CLI if --permutation is set
    global PATH_PERMUTATION_FILTER
    if args.permutation is not None:
        PATH_PERMUTATION_FILTER = {args.permutation}
        bot_logger.info(f"[startup] Permutation filter from CLI: {PATH_PERMUTATION_FILTER}")
    if not dry_run:
        bot_logger.info("\n*** LIVE MODE — BOT WILL SUBMIT REAL TRANSACTIONS! ***\n")

    config: dict[str, Any] = dotenv.dotenv_values("examples/mainnet.env")

    # Operator: must come from env
    operator_address = get_checksum_address(config.get("OPERATOR_ADDRESS", ""))
    operator_private_key = config.get("OPERATOR_PRIVATE_KEY", "")
    if not dry_run and (not operator_address or not operator_private_key):
        bot_logger.error("OPERATOR_ADDRESS and OPERATOR_PRIVATE_KEY must be set in mainnet.env")
        return
    if not operator_address:
        operator_address = ZERO_ADDRESS
    if not operator_private_key:
        operator_private_key = "0x" + "0" * 64

    # Node URLs
    node_http = (
        f"{config.get('NODE_HOST_HTTP', 'http://localhost')}:{config.get('NODE_PORT_HTTP', '8545')}"
    )
    node_ws = f"{config.get('NODE_HOST_WEBSOCKET', 'ws://localhost')}:{config.get('NODE_PORT_WEBSOCKET', '8546')}"

    # Executor: hardcoded with env override
    executor_address = get_checksum_address(
        config.get("EXECUTOR_CONTRACT_ADDRESS", EXECUTOR_ADDRESS)
    )
    if executor_address == get_checksum_address("0x" + "0" * 40):
        bot_logger.error("EXECUTOR_CONTRACT_ADDRESS is the zero address")
        return

    # ── Code injection: override executor address with injected one ──
    # When INJECT_EXECUTOR_CODE=True, we use a fresh address with the new
    # executor's runtime bytecode injected via eth_simulateV1 stateOverrides.
    # This avoids needing to deploy the contract first.
    if INJECT_EXECUTOR_CODE:
        executor_address = INJECTED_EXECUTOR_ADDRESS
        bot_logger.info(
            f"[inject] Code injection ENABLED — executor at {executor_address} "
            f"(owner={EXECUTOR_OWNER})"
        )

    # ── Bot session ──────────────────────────────────────────────
    bot = Bot.from_config_file()
    sync_w3 = web3.Web3(web3.HTTPProvider(node_http))
    sync_w3.middleware_onion.clear()
    bot.connections.register_provider(ProviderAdapter.from_web3(sync_w3))
    bot.connections.set_default_chain(1)
    bot_logger.info("Bot session initialized")

    # ── Async web3 ───────────────────────────────────────────────
    async_w3 = AsyncWeb3(web3.AsyncHTTPProvider(node_http))
    async_w3.middleware_onion.clear()

    # ── Build engine + paths ─────────────────────────────────────
    engine_registry = EngineRegistry()

    # Configure engine-internal verification: when a pool is registered from
    # snapshot data (apply_buffer=True), the engine snapshots its tick data
    # while the lock is held (pump cannot race) and verifies against on-chain
    # state via RPC after releasing the lock. This eliminates the timing race
    # that existed when Python called verify_v3_pool/verify_v4_pool async.
    state_view_address = EthereumMainnetUniswapV4.state_view.address
    engine_registry.engine.set_verify_rpc_url(node_http)
    engine_registry.engine.set_verify_state_view(state_view_address)

    latest_block = await async_w3.eth.get_block("latest")
    current_block = latest_block["number"]
    base_fee_next = next_base_fee(
        parent_base_fee=latest_block["baseFeePerGas"],
        parent_gas_used=latest_block["gasUsed"],
        parent_gas_limit=latest_block["gasLimit"],
    )
    operator_nonce = await async_w3.eth.get_transaction_count(operator_address)

    # ── Subscribe to WS (immediately, before path loading) ────────
    _chain_id = 1
    pending_nonces: set[int] = set()
    pending_pools: set[int] = set()
    active_tasks: set[asyncio.Task] = set()
    current_block_ref: list[int] = [current_block]  # mutable for monitor tasks
    block_times: deque[tuple[int, int]] = deque(maxlen=60)
    block_priority_fees: dict[int, dict[int, int]] = {}
    # (dispatch_lock and last_engine_log no longer needed —
    # the async for loop serializes dispatch naturally)
    path_suppression = PathSuppression()  # Suppress paths that consistently fail simulation

    # ── Verify at snapshot block (pre-backfill) ──────────────────
    # Compare the Rust engine's in-memory tick data against on-chain state
    # at the DB snapshot block. This catches stale data that was already
    # wrong at snapshot time.

    # We need the snapshot blocks BEFORE backfill runs, so extract them first.
    # get_snapshots returns (v3_snapshot, v4_snapshot, v3_snapshot_block, v4_snapshot_block).

    # ── Subscribe to WS (immediately, before path loading) ────────
    # Open WS subscriptions now so events start buffering.
    # subscribe() returns the first observed block number — this is
    # our backfill target. The subscribe phase waits until both a
    # newHeads notification and a log for the same block arrive,
    # confirming the logs subscription is live. No events are buffered.
    bot_logger.info("[startup] Subscribing to WS...")
    backfill_target = engine_registry.engine.subscribe(node_ws)
    bot_logger.info(f"[startup] WS subscribe complete — first observed block: {backfill_target}")

    # Use backfill_target instead of an RPC call if it's ahead
    if backfill_target > current_block:
        current_block = backfill_target
        bot_logger.info(f"[startup] Updated current_block to subscribe target: {current_block}")

    # ── Load snapshots ───────────────────────────────────────────
    # Load V3 and V4 snapshots from DB, then transfer to the Rust engine
    # via binary serialization. No Python-side event fetching —
    # the Rust engine backfills via backfill_from_snapshot().
    v3_snapshot, v4_snapshot, v3_snap_block, v4_snap_block = get_snapshots(bot)

    # Load snapshots into the Rust engine via streaming (transitions to SnapshotLoaded phase).
    # The engine auto-lookup tick data at registration time.
    # Streaming avoids building the full snapshot dict in memory.

    if v3_snapshot is not None:
        stream_v3_snapshot_to_engine(v3_snapshot, engine_registry.engine)
        bot_logger.info("[startup] V3 snapshot loaded into engine")
    if v4_snapshot is not None:
        stream_v4_snapshot_to_engine(v4_snapshot, engine_registry.engine)
        bot_logger.info("[startup] V4 snapshot loaded into engine")

    # ── Backfill snapshot gap ────────────────────────────────────
    # Fetch Mint/Burn/ModifyLiquidity events from the snapshot block
    # to the first WS block via eth_getLogs, applying them to the
    # Rust engines. This ensures the engine's tick state and event
    # buffer are current before pool registration begins.
    bot_logger.info("[startup] Running Rust backfill from snapshot to WS block...")
    snapshot_block = min(b for b in (v3_snap_block, v4_snap_block) if b is not None)
    backfilled = engine_registry.engine.backfill_from_snapshot(node_http, snapshot_block)
    bot_logger.info(f"[startup] Backfill complete: {backfilled} blocks")

    # Verify at the SNAPSHOT block, not the backfill boundary.
    # At the snapshot block, the engine's tick data matches the DB exactly
    # (zero events applied yet). This validates that snapshot loading
    # produced correct data. Using the post-backfill block would cause
    # false failures: the WS pump applies ModifyLiquidity events beyond
    # the backfill boundary, bringing the engine state ahead of the
    # comparison block. Buffer/backfill application bugs are caught by
    # downstream simulation failures, not by this verification.
    bot_logger.info(f"[startup] Snapshot block: {snapshot_block}")

    # The verification blocks are captured automatically by the Rust engine
    # during backfill_from_snapshot():
    # - verify_snapshot_block: set to snapshot_block (confirms DB tick data)
    # - verify_backfill_block: set to last_processed_block after backfill
    #   (confirms event decoding + buffer application)
    # No Python-side configuration needed — the engine knows these boundaries.
    engine_registry.engine.set_verify_on_register(True)

    # ── Resume the Rust pump ─────────────────────────────────────
    # The pump was subscribed earlier (WS live, events buffered).
    # Now that backfill is complete, resume normal processing.
    engine_registry.engine.resume()
    bot_logger.info(f"[startup] Rust pump resumed (WS={node_ws}, backfill complete)")

    # ── Rolling start: consume results while building paths ─────────
    # Start the result consumer as a background task BEFORE build_paths,
    # so eagerly-solved paths are dispatched during path loading instead
    # of waiting until all paths are registered. Both coroutines yield
    # at await points, so asyncio interleaves them naturally.
    result_consumer_task = asyncio.create_task(
        consume_result_batches(
            engine_registry=engine_registry,
            async_w3=async_w3,
            executor_address=executor_address,
            operator_address=operator_address,
            operator_private_key=operator_private_key,
            pending_nonces=pending_nonces,
            pending_pools=pending_pools,
            active_tasks=active_tasks,
            current_block_ref=current_block_ref,
            dry_run=dry_run,
            block_times=block_times,
            block_priority_fees=block_priority_fees,
            path_suppression=path_suppression,
        ),
        name="result-consumer",
    )

    bot_logger.info(
        "[startup] Starting path loading (rolling start — consuming results concurrently)..."
    )
    await build_paths(
        bot=bot,
        engine_registry=engine_registry,
        v3_snapshot=v3_snapshot,
        v4_snapshot=v4_snapshot,
    )
    bot_logger.info("[startup] Path loading complete — trimming redundant Python state")

    # ── Trim redundant Python state — Rust engine owns all pool state ──
    # 1. Drop tracker caches and snapshots (prevent them from pinning pool objects)
    for tracker in bot._trackers.values():
        if hasattr(tracker, "_tracked_pools"):
            tracker._tracked_pools.clear()
        if hasattr(tracker, "_untracked_pools"):
            tracker._untracked_pools.clear()
        if hasattr(tracker, "unload_snapshot"):
            tracker.unload_snapshot()

    # 2. Drop the bot's pool and token registries (Rust owns canonical state)
    bot.pools._reset()
    bot.tokens.reset()

    # 3. Drop snapshot wrappers (already streamed to Rust)
    del v3_snapshot, v4_snapshot

    # 4. Drop the bot session entirely and force full collection
    #    The hot loop only needs engine_registry, async_w3, and scalars.
    del bot
    gc.collect()

    bot_logger.info(
        f"[startup] State trimmed — {engine_registry.engine.v2_pool_count()} V2, "
        f"{engine_registry.engine.v3_pool_count()} V3, "
        f"{engine_registry.engine.v4_pool_count()} V4 pools retained in Rust engine; "
        f"{engine_registry.engine.path_count()} paths registered. Entering main loop."
    )

    # The result consumer task runs indefinitely.
    await result_consumer_task


if __name__ == "__main__":
    start = time.perf_counter()
    asyncio.run(main())
    bot_logger.info(f"Completed in {time.perf_counter() - start:.2f}s")
