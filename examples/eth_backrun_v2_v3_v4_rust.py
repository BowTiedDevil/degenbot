"""Ethereum mainnet backrun bot: Uniswap V3 ↔ V2 arbitrage using the Rust engine.

A thin Python orchestration layer over the Rust-owned UniswapArbEngine (Plan 079).

The Rust engine owns all pool state and path solving.
Python does: pool construction, event routing, V3 + V2 encoding, simulation, and
transaction submission.

Token flow (V3 is always the first swap — its callback provides the flash borrow):

Case 1 — V3 "buy forward" (zfo=True, WETH→forward), V2 has higher ROE:
  Pay 0: V3 swap(recipient=V2_POOL) — V3 sends forward directly to V2, callbacks executor
  Pay 1: V2 swap(to=EXECUTOR) — V2 sends WETH to executor
  V3 callback: executor pays WETH to V3 (from V2 proceeds)

Case 2 — V3 "sell forward" (zfo=False, forward→WETH), V3 has higher ROE:
  Pay 0: V3 swap(recipient=EXECUTOR) — V3 sends WETH to executor, callbacks executor
  Pay 1: ERC20 transfer(WETH) from executor to V2
  Pay 2: V2 swap(to=V3_POOL) — V2 sends forward to V3 pool (satisfies V3's debt)

No WETH prefunding is required. The V3 callback is the flash borrow.
The executor contract asserts WETH balance does not decrease (profit = increase).

Per-event lifecycle (same-block reactivity):
1. WS subscription delivers logs and newHeads concurrently
2. Each log event is decoded and pushed to the Rust engine immediately
3. After each engine update, check for profitable results
4. On newHead: update fee/nonce state, also check for profitable results
5. For profitable results: encode, simulate, submit
6. Submitted transactions are monitored; their pools/nonces are released on
   confirmation or expiry
"""

import argparse
import asyncio
import dataclasses
import itertools
import os
import time
from collections import deque
from typing import Any

import dotenv
import eth_abi.abi
import eth_account
import web3
from hexbytes import HexBytes
from web3 import AsyncWeb3, Web3
from web3.exceptions import ContractLogicError, TransactionNotFound, Web3Exception
from web3.utils.subscriptions import NewHeadsSubscription

from degenbot import Bot, UniswapV2Pool, UniswapV3Pool, get_checksum_address
from degenbot.calculations.evm_math import next_base_fee
from degenbot.constants import WRAPPED_NATIVE_TOKENS, ZERO_ADDRESS
from degenbot.database.models.pools import (
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
from degenbot.sushiswap.pools import SushiswapV2Pool as _SushiV2
from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool as _V2Pool
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS, UniswapV4Pool
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot

# ──────────────────────────────────────────────────────────────────
# Configuration
# ──────────────────────────────────────────────────────────────────

WETH_ADDRESS = WRAPPED_NATIVE_TOKENS[1]

MIN_PROFIT_NET = 5 * 10**9  # 5 gwei
FEE_HISTORY_WINDOW = 10
FEE_PERCENTILES = (10, 50)
TARGET_PROFIT_RATIO = 1.25
BLOCKS_BEFORE_NONCE_EXPIRES = 5
MAX_SIMULATE_CONCURRENT = 50  # Cap concurrent simulation RPC calls (Slice 1)
STALENESS_TOLERANCE = 5  # Blocks after solve_block to discard results (Slice 2)
AGE_DECAY_CONSTANT = 0.25  # Priority fee age decay factor (Slice 3)
MIN_PRIORITY_FEE_PERCENTILE = 10  # Use Nth percentile from feeHistory as floor (Slice 3)
MAX_PRIORITY_FEE_PERCENTILE = 50  # Use Nth percentile from feeHistory as ceiling (Slice 3)
RECONNECT_BASE_DELAY = 1.0  # WS reconnection initial delay in seconds (Slice 6)
RECONNECT_MAX_DELAY = 30.0  # WS reconnection max delay in seconds (Slice 6)
MAX_PROFIT_WEI = 5 * 10**18  # Reject profits above 5 ETH — solver defect / scam token guard
DRY_RUN = False

# ── Token quality filter ────────────────────────────────────────
# Known-good intermediate tokens (whitelist). Paths using these as
# the non-WETH leg are far more likely to simulate successfully
# because they have no transfer taxes, no anti-bot hooks, and
# unrestricted approvals.
KNOWN_GOOD_TOKENS: set[str] = {
    # Stablecoins
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    "0xdAC17F958D2ee523a2206206994597C13D831ec7",  # USDT
    "0x6B175474E89094C44Da98b954EedeAC495271d0F",  # DAI
    "0x4Ddc2D193948926D02f9B1fE9e1DAa391349896F",  # cdDAI (compound)
    "0x5d3a536E4D6DbD6114cc1Ead35777bAB948E3643",  # cDAI (compound)
    # LSTs / major DeFi
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",  # WBTC
    "0x7Fc66500c84A76Ad7e9f9f8d8e948a8a316946F1",  # AAVE
    "0x6De037ef9aD27997fA8750e460742a63D03e837e",  # cbETH
    "0xE95A203B1a91a908F9B9CE46459d101078c2c3cb",  # ETH2x-FLI
    "0xae78736Cd6155379c5C6c7BE7184e4365aF310f0",  # rETH
    "0xD533a949740bb3306d119CC777fa900bA034cd52",  # CRV
    "0x5A98FcBEA516C06694E63110Ab2A8A1E1c655d0c",  # LDO
    "0x0D8775F648430679A709E98d2b0Cb6250d2887EF",  # BAT
    "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",  # UNI
    "0xC011a73ee8576Fb46F5E1c5751cA3B9Fe0af2a6F",  # SNX
    "0x9E46A38F5Daa053F6FB342C6DcB1E00354033b54",  # cbETH (coinbase)
    "0xBe9895146f7AF43049ca1c1AE358B0548Ea794e6",  # cbETH (coinbase, alternate)
    "0xdf1E1c5191E4dc93D0c2a10C9F7b5d243E0830b7",  # ETHFI
    "0xD31a59c85aE5b4e9fbd552c5D7a2a7DF09B3D0Ee",  # sDai
    # Note: FEI (0x956F...) was deprecated in 2023 and transfers are restricted — do NOT add
    "0x6810e776880C02933D47DB1b9fc05908e5386b96",  # GNO
    "0xc00e94Cb662C3520282E6f5717214004A7f26888",  # COMP
    "0x2b591e99afE9f32eAA6214f7B7629768C40Eeb39",  # HEX
    "0x514910771AF9Ca656af840dff83E8264EcF986CA",  # LINK
    "0x9f8F72aA9304c8B593d555F12eF6589cC3A579A2",  # MKR
    "0x7B50775383d3D6f0215A8F290d2a9A1805b050D2",  # stETH (wrapped)
    "0x1985365e9f2437Ec872EaB27a7d5b8034948962b",  # ENJ
    "0xDDAfbb505ad214D7b80b1f830fcC89BA60B7e729",  # BAL
    "0x0bc529c00C6401aEF6D220BE8276A939E587F21e",  # YFI
    "0x1CE0c282017aCB03EB9C01b68B6f31E3c7B7a90b",  # WSTETH
    "0x7f39C581F544B12896aa41374b37A6a8A1fA5e94",  # WSTETH (correct)
}

# Known scam/tax/anti-bot tokens (blacklist). Paths using these
# will always fail simulation because the token contract blocks or
# taxes transfers in ways that break the executor's callback flow.
KNOWN_SCAM_TOKENS: set[str] = {
    "0x51166Bb3a7c4659FcD8f40D0c0DC5a1e705a6b74",  # "Loyalty Labs" (TF revert)
}

# When True, only register paths where the intermediate (non-WETH)
# token is in KNOWN_GOOD_TOKENS. Eliminates 95%+ of sim-failures
# from scam/tax/honeypot tokens at the cost of missing unknown-good paths.
TOKEN_WHITELIST_MODE = os.environ.get("TOKEN_WHITELIST_MODE", "0") == "1"

# When True, skip paths where any token is in KNOWN_SCAM_TOKENS.
# This is less restrictive than whitelist mode — it allows unknown tokens
# but blocks known bad ones.
TOKEN_BLACKLIST_MODE = os.environ.get("TOKEN_BLACKLIST_MODE", "1") == "1"

# ── Executor code injection via eth_simulateV1 ──────────────────
# When INJECT_EXECUTOR_CODE=True, we inject the new tstore_executor
# runtime bytecode at a fresh address via stateOverrides.code.
# This lets us test the new V2-callback-capable executor contract
# WITHOUT deploying it on mainnet first.
# The runtime bytecode must have immutables (OWNER_ADDR, WETH_ADDR)
# already baked in — see contracts/tstore_executor_runtime_bytecode.txt.
#
# eth_simulateV1 calls within a blockStateCalls group chain their
# state changes sequentially, so the 3-call pattern (balanceOf before
# → execute_payloads → balanceOf after) correctly measures profit
# without needing WETH storage overrides or prefunding.
INJECT_EXECUTOR_CODE = os.environ.get("INJECT_EXECUTOR_CODE", "0") == "1"
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

# Executor contract
# Source: contracts/tstore_executor.vy
# Supports V2 callbacks (uniswapV2Call, hook, pancakeCall), V3 callbacks
# (uniswapV3SwapCallback, pancakeV3SwapCallback), and bribes.
EXECUTOR_ADDRESS = os.environ.get(
    "EXECUTOR_CONTRACT_ADDRESS",
    "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",  # Update after deployment
)
EXECUTOR_OWNER = os.environ.get(
    "EXECUTOR_OWNER_ADDRESS",
    "0x9C56a29c7231974c269E24F9FB3c29203039089E",  # Throwaway — override with real key at runtime
)
EXECUTOR_ABI = [
    # execute_payloads(payloads, bribe_bips=0)
    {
        "stateMutability": "payable",
        "type": "function",
        "name": "execute_payloads",
        "inputs": [
            {
                "name": "payloads",
                "type": "tuple[]",
                "components": [
                    {"name": "target", "type": "address"},
                    {"name": "calldata", "type": "bytes"},
                    {"name": "will_callback", "type": "bool"},
                ],
            },
            {"name": "bribe_bips", "type": "uint256"},
        ],
        "outputs": [],
    },
]


# ──────────────────────────────────────────────────────────────────
# ABI selectors
# ──────────────────────────────────────────────────────────────────

V3_SWAP_SELECTOR = web3.Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
V2_SWAP_SELECTOR = web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
ERC20_TRANSFER_SELECTOR = web3.Web3.keccak(text="transfer(address,uint256)")[:4]
BALANCEOF_SELECTOR = web3.Web3.keccak(text="balanceOf(address)")[:4]

# V4 PoolManager selectors
V4_UNLOCK_SELECTOR = web3.Web3.keccak(text="unlock(bytes)")[:4]
V4_SWAP_SELECTOR = web3.Web3.keccak(
    text="swap((address,address,uint24,int24,address),(bool,int256,uint160),bytes32)"
)[:4]
V4_TAKE_SELECTOR = web3.Web3.keccak(text="take(address,address,uint256)")[:4]
V4_SYNC_SELECTOR = web3.Web3.keccak(text="sync(address)")[:4]
V4_SETTLE_SELECTOR = web3.Web3.keccak(text="settle()")[:4]

V4_MIN_SQRT_PRICE_X96 = 4295128739 + 1  # MIN_SQRT_PRICE + 1
V4_MAX_SQRT_PRICE_X96 = 1461446703485210103287273052203988822378723970342 - 1


# ──────────────────────────────────────────────────────────────────
# Swap encoding helpers
# ──────────────────────────────────────────────────────────────────


def encode_v3_swap_calldata(
    recipient: str,
    zero_for_one: bool,
    amount_specified: int,
    sqrt_price_limit_x96: int,
) -> bytes:
    """Encode a Uniswap V3 pool swap() call.

    V3 amountSpecified sign convention (OPPOSITE to V4!):
      positive (> 0) → exact INPUT  (swap exactly this much INTO the pool)
      negative (< 0) → exact OUTPUT (receive exactly this much FROM the pool)

    For arbitrage, we always use exact INPUT mode → amountSpecified must be POSITIVE.
    """
    return V3_SWAP_SELECTOR + eth_abi.abi.encode(
        types=["address", "bool", "int256", "uint160", "bytes"],
        args=[recipient, zero_for_one, amount_specified, sqrt_price_limit_x96, b""],
    )


def encode_v2_swap_calldata(
    zero_for_one: bool,
    amount_out: int,
    recipient: str,
    flash_borrow: bool = False,
) -> bytes:
    """Encode a Uniswap V2 pool swap(uint256,uint256,address,bytes) call.

    When flash_borrow=True, non-empty data triggers V2's callback
    (uniswapV2Call/hook/pancakeCall). Without non-empty data, V2 does a
    direct swap with no callback, and the invariant check fails immediately
    because tokens have left the pool with nothing returned.
    """
    a0_out, a1_out = (0, amount_out) if zero_for_one else (amount_out, 0)
    # Non-empty data triggers the callback; empty data = direct swap (no callback)
    data = b"\x01" if flash_borrow else b""
    return V2_SWAP_SELECTOR + eth_abi.abi.encode(
        types=["uint256", "uint256", "address", "bytes"],
        args=[a0_out, a1_out, recipient, data],
    )


def encode_erc20_transfer_calldata(recipient: str, amount: int) -> bytes:
    """Encode an ERC20 transfer(address,uint256) call."""
    return ERC20_TRANSFER_SELECTOR + eth_abi.abi.encode(
        types=["address", "uint256"],
        args=[recipient, amount],
    )


def encode_balanceof_calldata(account: str) -> bytes:
    """Encode an ERC20 balanceOf(address) call for simulation."""
    return BALANCEOF_SELECTOR + eth_abi.abi.encode(
        types=["address"],
        args=[account],
    )


def encode_v4_unlock_calldata(data: bytes = b"") -> bytes:
    """Encode PoolManager.unlock(bytes) calldata.

    This triggers unlockCallback on the executor, which resumes
    payload delivery inside the V4 unlock context.
    """
    return V4_UNLOCK_SELECTOR + eth_abi.abi.encode(
        types=["bytes"],
        args=[data],
    )


def encode_v4_swap_calldata(
    currency0: str,
    currency1: str,
    fee: int,
    tick_spacing: int,
    hooks: str,
    zero_for_one: bool,
    amount_specified: int,
    sqrt_price_limit_x96: int,
    hook_data: bytes = b"\x00" * 32,
) -> bytes:
    """Encode PoolManager.swap(PoolKey, SwapParams, bytes32) calldata.

    V4 amountSpecified sign convention (OPPOSITE to V3!):
      negative (< 0) → exact INPUT  (swap exactly this much INTO the pool)
      positive (> 0) → exact OUTPUT (receive exactly this much FROM the pool)

    For arbitrage, we always use exact INPUT mode → amountSpecified must be NEGATIVE.
    This is the reverse of V3's convention.
    """
    return V4_SWAP_SELECTOR + eth_abi.abi.encode(
        types=["(address,address,uint24,int24,address)", "(bool,int256,uint160)", "bytes32"],
        args=[
            (currency0, currency1, fee, tick_spacing, hooks),
            (zero_for_one, amount_specified, sqrt_price_limit_x96),
            hook_data,
        ],
    )


def encode_v4_take_calldata(
    currency: str,
    to: str,
    amount: int,
) -> bytes:
    """Encode PoolManager.take(address,address,uint256) calldata.

    Receives tokens from PoolManager's internal accounting.
    For ERC-20: tokens are transferred to `to`.
    For ETH (currency=address(0)): ETH is sent to `to`.
    """
    return V4_TAKE_SELECTOR + eth_abi.abi.encode(
        types=["address", "address", "uint256"],
        args=[currency, to, amount],
    )


def encode_v4_sync_calldata(currency: str) -> bytes:
    """Encode PoolManager.sync(address) calldata.

    Must be called before settle() for ERC-20 tokens.
    Reads the current balance of the currency in PoolManager
    and updates its internal tracking so settle() credits correctly.
    """
    return V4_SYNC_SELECTOR + eth_abi.abi.encode(
        types=["address"],
        args=[currency],
    )


def encode_v4_settle_calldata() -> bytes:
    """Encode PoolManager.settle() calldata.

    Credits any tokens that PoolManager has received since the last sync().
    For ERC-20: must call sync() before settle() so PoolManager sees the new balance.
    For ETH: msg.value is credited directly (no sync needed).
    """
    return V4_SETTLE_SELECTOR


# ──────────────────────────────────────────────────────────────────
# Executor code injection helpers
# ──────────────────────────────────────────────────────────────────

# Cached runtime bytecode (loaded once, reused across all simulations)
_runtime_bytecode_cache: str | None = None


def _load_executor_runtime_bytecode() -> str:
    """Load the patched runtime bytecode from contracts/ directory.

    The bytecode has OWNER_ADDR and WETH_ADDR immutables already baked in.
    """
    global _runtime_bytecode_cache
    if _runtime_bytecode_cache is not None:
        return _runtime_bytecode_cache

    bytecode_path = os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "contracts",
        "tstore_executor_runtime_bytecode.txt",
    )
    with open(bytecode_path) as f:
        code = f.read().strip()
    if not code.startswith("0x"):
        msg = f"Runtime bytecode file must start with 0x, got: {code[:20]}..."
        raise ValueError(msg)
    _runtime_bytecode_cache = code
    bot_logger.info(
        f"[inject] Loaded executor runtime bytecode: {len(code) // 2 - 1} bytes from {bytecode_path}"
    )
    return _runtime_bytecode_cache


def build_simulation_state_overrides(
    executor_address: str,
    executor_owner: str,
    inject_code: bool = False,
    injected_address: str | None = None,
) -> dict:
    """Build the stateOverrides dict for eth_simulateV1.

    When inject_code=True:
      - Injects runtime bytecode at injected_address via stateOverrides.code
      - Funds the executor owner with ETH for gas

    When inject_code=False:
      - Only funds the executor owner with ETH for gas

    Note: No WETH storage override is needed. eth_simulateV1 chains calls
    sequentially within a blockStateCalls group, so the 3-call balanceOf
    pattern correctly captures WETH balance changes from execute_payloads.
    """
    overrides: dict[str, Any] = {}

    # Fund executor owner with ETH for gas
    overrides[Web3.to_checksum_address(executor_owner)] = {
        "balance": 100 * 10**18,
    }

    if inject_code and injected_address:
        # Inject executor runtime bytecode at the fresh address
        runtime_code = _load_executor_runtime_bytecode()
        overrides[Web3.to_checksum_address(injected_address)] = {
            "code": runtime_code,
        }

    return overrides


# ──────────────────────────────────────────────────────────────────
# Rust engine wrapper
# ──────────────────────────────────────────────────────────────────


@dataclasses.dataclass
class V4PoolInfo:
    """V4 pool metadata for encoding and event routing.

    Stored in EngineRegistry._v4_pool_info keyed by the Rust engine key.
    """

    pool: UniswapV4Pool
    pool_manager_address: str
    pool_id_hex: str  # e.g. "0x1234..."


@dataclasses.dataclass
class HopInfo:
    pool: UniswapV2Pool | UniswapV3Pool | UniswapV4Pool
    pool_type: str  # "V2", "V3", or "V4" — mirrors the Rust HopType
    zfo: bool


@dataclasses.dataclass
class PathInfo:
    hops: list[HopInfo]

    @property
    def path_type(self) -> str:
        """Combined pool types: 'V3-V2', 'V3-V3', 'V2-V2', 'V4-V3', etc."""
        return "-".join(h.pool_type for h in self.hops)


class EngineRegistry:
    """Thin wrapper over the Rust UniswapArbEngine.

    Maintains Python pool ↔ Rust key mappings so events can be routed
    to the right engine pool, and results can be mapped back to Python
    pool objects for encoding.

    V3 is always the first pool in the path (provides flash borrow via callback).
    V4 pools are identified by (pool_manager_address, pool_id) — stored in
    _v4_keys keyed by pool_id hex string for fast lookup during event routing.
    """

    def __init__(self) -> None:
        self.engine = UniswapArbEngine()
        self._v2_keys: dict[str, int] = {}  # address → key
        self._v3_keys: dict[str, int] = {}
        # V4 pools keyed by pool_id hex — for event routing from PoolManager logs
        self._v4_keys: dict[str, int] = {}  # pool_id_hex → key
        # Reverse map: key → V4PoolInfo for encoding
        self._v4_pool_info: dict[int, V4PoolInfo] = {}
        self.paths: dict[int, PathInfo] = {}

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

    def register_v3_pool(self, pool: UniswapV3Pool, block: int = 0) -> int:
        if pool.address in self._v3_keys:
            return self._v3_keys[pool.address]
        tick_data = {
            idx: (info.liquidity_gross, info.liquidity_net) for idx, info in pool.tick_data.items()
        }
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
            tick_data=tick_data,
            block=block,
        )
        self._v3_keys[pool.address] = key
        return key

    def register_v4_pool(self, pool: UniswapV4Pool, block: int = 0) -> int:
        """Register a V4 pool with the Rust engine.

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
        hook_flags = int(pool.hook_address, 16)
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

        tick_data = {
            idx: (info.liquidity_gross, info.liquidity_net) for idx, info in pool.tick_data.items()
        }

        key = self.engine.register_v4_pool(
            pool_manager=pool.pool_manager_address,
            pool_id_hex=pool_id_hex,
            currency0=pool.token0.address,
            currency1=pool.token1.address,
            fee=pool.fee,
            tick_spacing=pool.tick_spacing,
            hook_flags=hook_flags,
            sqrt_price_x96=pool.sqrt_price_x96,
            liquidity=pool.liquidity,
            tick=pool.tick,
            tick_data=tick_data,
            block=block,
        )

        self._v4_keys[pool_id_hex] = key
        # Store V4 pool info for encoding (pool_key, pool_manager, pool_id)
        self._v4_pool_info[key] = V4PoolInfo(
            pool=pool,
            pool_manager_address=pool.pool_manager_address,
            pool_id_hex=pool_id_hex,
        )
        return key

    def knows_pool(self, address: str) -> bool:
        return address in self._v2_keys or address in self._v3_keys

    def knows_v4_pool(self, pool_id_hex: str) -> bool:
        return pool_id_hex in self._v4_keys

    def register_path(self, hops: list[HopInfo]) -> int:
        """Register a path from a list of HopInfo objects."""
        engine_hops = []
        for hop in hops:
            if hop.pool_type == "V2":
                fwd_key = self._v2_keys.get(hop.pool.address)
                # V2 pools are registered in both orientations:
                # forward (fwd_key): reserve0→reserve1 (zfo=True)
                # reverse (fwd_key+1): reserve1→reserve0 (zfo=False)
                key = fwd_key if hop.zfo else fwd_key + 1
            elif hop.pool_type == "V4":
                pool_id_hex = hop.pool.pool_id.to_0x_hex()
                fwd_key = self._v4_keys.get(pool_id_hex)
                # V4 pools are also registered in both orientations (like V2)
                key = fwd_key if hop.zfo else fwd_key + 1
            else:
                key = self._v3_keys.get(hop.pool.address)
            if key is None:
                msg = f"Pool not registered: {hop.pool_type} {hop.pool}"
                raise ValueError(msg)
            engine_hops.append((hop.pool_type, key, hop.zfo))

        path_id = self.engine.register_path(engine_hops)
        self.paths[path_id] = PathInfo(hops=hops)
        return path_id

    def process_block(
        self,
        v2_updates: list[tuple[str, int, int]],
        v3_updates: list[tuple[str, int, int, int, list[tuple[int, tuple[int, int]]]]],
        v4_updates: list[tuple[str, str, int, int, int, list[tuple[int, tuple[int, int]]]]],
        block_number: int,
    ) -> None:
        """Push one block's worth of updates and solve.

        v4_updates: list of (pool_manager_address, pool_id_hex, sqrt_price_x96,
                    liquidity, tick, tick_priors) tuples.
        """
        if v2_updates or v3_updates or v4_updates:
            self.engine.process_logs(v2_updates, v3_updates, v4_updates, block_number)

    def profitable_results(
        self, min_profit: int = MIN_PROFIT_NET
    ) -> list[tuple[int, int, int, int]]:
        """Return (path_id, optimal_input, profit, solve_block) for results above min_profit.

        solve_block is the block_number passed to process_logs when the result was computed.
        Used for staleness tracking — if current_block > solve_block, the pools may have
        changed and the result may be stale.
        """
        flat, solve_block = self.engine.latest_results()
        results = []
        for i in range(0, len(flat), 3):
            if i + 2 >= len(flat):
                break
            path_id, opt_input, profit = int(flat[i]), int(flat[i + 1]), int(flat[i + 2])
            if profit > min_profit:
                if profit > MAX_PROFIT_WEI:
                    bot_logger.debug(
                        f"drop: path={path_id} profit={profit // 10**18}ETH "
                        f"exceeds MAX_PROFIT_WEI={MAX_PROFIT_WEI // 10**18}ETH"
                    )
                    continue
                results.append((path_id, opt_input, profit, solve_block))
        return results


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
        if token0_addr == ZERO_ADDRESS:
            token0_addr = WETH_ADDRESS
        if token1_addr == ZERO_ADDRESS:
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


# ──────────────────────────────────────────────────────────────────
# Payload encoding
# ──────────────────────────────────────────────────────────────────


def encode_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for an arbitrage path.

    Dispatches to the appropriate encoder based on path type.
    Returns a list of (target, calldata, will_callback) tuples, or None if
    encoding fails or the path type is not yet supported.
    """
    pt = path_info.path_type
    if pt == "V3-V2":
        return encode_v3v2_payloads(path_info, optimal_input, executor_address)
    if pt == "V3-V3":
        return encode_v3v3_payloads(path_info, optimal_input, executor_address)
    if pt == "V2-V2":
        return encode_v2v2_payloads(path_info, optimal_input, executor_address)
    if pt == "V2-V3":
        return encode_v2v3_payloads(path_info, optimal_input, executor_address)
    if pt == "V4-V4":
        return encode_v4v4_payloads(path_info, optimal_input, executor_address)
    if pt == "V4-V3":
        return encode_v4v3_payloads(path_info, optimal_input, executor_address)
    if pt == "V3-V4":
        return encode_v3v4_payloads(path_info, optimal_input, executor_address)
    if pt == "V4-V2":
        return encode_v4v2_payloads(path_info, optimal_input, executor_address)
    if pt == "V2-V4":
        return encode_v2v4_payloads(path_info, optimal_input, executor_address)
    return None


def encode_v3v3_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V3/V3 arb path.

    Both V3 pools support callbacks, enabling nested callback execution.
    The executor's generic payload queue delivers payloads sequentially
    through nested callbacks.

    Token flow (example for V3_A(zfo=True) → V3_B(zfo=False)):
      V3_A: forward_in → WETH_out (to executor)
      V3_B: WETH_in → forward_out (to executor)
      Profit: WETH_out - WETH_in (stays in executor)

    Payload sequence:
      0: V3_A.swap(recipient=executor) [will_callback=True]
         → V3_A sends output to executor, then callbacks executor
      1: V3_B.swap(recipient=executor) [will_callback=True]
         → delivered inside V3_A's callback
         → V3_B sends output to executor, then callbacks executor (nested)
      2: <payment>.transfer(V3_B, amount) [will_callback=False]
         → delivered inside V3_B's callback
         → pays V3_B's debt (token V3_B is owed)
      3: <payment>.transfer(V3_A, amount) [will_callback=False]
         → delivered inside V3_A's callback (after V3_B's callback returns)
         → pays V3_A's debt (token V3_A is owed)

    V3 balance check: balanceAfter >= balanceBefore + amount_owed.
    Tokens transferred DURING a pool's callback increase balanceAfter,
    satisfying the check. Tokens must arrive during the callback, not before
    the swap call (which would increase balanceBefore and raise the bar).
    """
    v3_a = path_info.hops[0].pool
    v3_b = path_info.hops[1].pool
    zfo_a = path_info.hops[0].zfo
    zfo_b = path_info.hops[1].zfo

    try:
        # Determine which token each pool is owed (positive delta = debt)
        # zfo=True: pool receives token0 (owes token1), swapper owes token0
        # zfo=False: pool receives token1 (owes token0), swapper owes token1
        #
        # Pool_A owes: token1 if zfo_a=True, token0 if zfo_a=False
        # Pool_A is owed: token0 if zfo_a=True, token1 if zfo_a=False
        #
        # Pool_B owes: token1 if zfo_b=True, token0 if zfo_b=False
        # Pool_B is owed: token0 if zfo_b=True, token1 if zfo_b=False

        token_a_owed = v3_a.token0 if zfo_a else v3_a.token1  # token swapper must pay to V3_A
        token_b_owed = v3_b.token0 if zfo_b else v3_b.token1  # token swapper must pay to V3_B

        # Calculate V3_A output
        token_in_a = v3_a.token0 if zfo_a else v3_a.token1
        output_a = v3_a.calculate_tokens_out_from_tokens_in(
            token_in=token_in_a,
            token_in_quantity=optimal_input,
        )
        if output_a <= 0:
            return None

        # Calculate V3_B output (using V3_A's output as input)
        token_in_b = v3_b.token0 if zfo_b else v3_b.token1
        output_b = v3_b.calculate_tokens_out_from_tokens_in(
            token_in=token_in_b,
            token_in_quantity=output_a,
        )
        if output_b <= 0:
            return None

        # The amount owed by swapper to each pool = the input amount
        # (for exactInput, amount_delta = positive amount of input token)
        amount_owed_a = optimal_input
        amount_owed_b = output_a

        # V3_A swap: send output to executor
        sqrt_limit_a = (MIN_SQRT_RATIO + 1) if zfo_a else (MAX_SQRT_RATIO - 1)
        v3_a_data = encode_v3_swap_calldata(
            recipient=executor_address,
            zero_for_one=zfo_a,
            amount_specified=optimal_input,  # exact input (positive)
            sqrt_price_limit_x96=sqrt_limit_a,
        )

        # V3_B swap: send output to executor
        sqrt_limit_b = (MIN_SQRT_RATIO + 1) if zfo_b else (MAX_SQRT_RATIO - 1)
        v3_b_data = encode_v3_swap_calldata(
            recipient=executor_address,
            zero_for_one=zfo_b,
            amount_specified=output_a,  # exact input (positive)
            sqrt_price_limit_x96=sqrt_limit_b,
        )

        payloads: list[tuple[str, bytes, bool]] = [
            (v3_a.address, v3_a_data, True),  # V3_A callbacks executor
            (v3_b.address, v3_b_data, True),  # V3_B callbacks executor (nested)
        ]

        # Pay V3_B and V3_A for their swap debts.
        # IMPORTANT: The executor's V3 callback auto-pays WETH when a pool
        # is owed WETH. We must NOT include an explicit WETH transfer for a
        # pool that will be auto-paid — otherwise the pool gets double-paid
        # (once from the explicit transfer during payload delivery, and once
        # from auto-pay after payload delivery). The second transfer would
        # fail or send more WETH than owed, causing the tx to revert.
        if token_b_owed.address != WETH_ADDRESS:
            pay_b_data = encode_erc20_transfer_calldata(
                recipient=v3_b.address,
                amount=amount_owed_b,
            )
            payloads.append((
                token_b_owed.address,
                pay_b_data,
                False,
            ))  # Pay V3_B inside V3_B's callback

        if token_a_owed.address != WETH_ADDRESS:
            pay_a_data = encode_erc20_transfer_calldata(
                recipient=v3_a.address,
                amount=amount_owed_a,
            )
            payloads.append((
                token_a_owed.address,
                pay_a_data,
                False,
            ))  # Pay V3_A inside V3_A's callback

        # ── Diagnostic: log V3-V3 encoding details ──────────────
        bot_logger.info(
            f"[encode_v3v3] v3_a={v3_a.address} zfo_a={zfo_a} "
            f"v3_b={v3_b.address} zfo_b={zfo_b} "
            f"token_a_owed={token_a_owed.address} token_b_owed={token_b_owed.address} "
            f"optimal_input={optimal_input} output_a={output_a} output_b={output_b} "
            f"amount_owed_a={amount_owed_a} amount_owed_b={amount_owed_b} "
            f"auto_pay_a={token_a_owed.address == WETH_ADDRESS} "
            f"auto_pay_b={token_b_owed.address == WETH_ADDRESS} "
            f"n_payloads={len(payloads)}"
        )

        return payloads
    except Exception as e:
        bot_logger.info(f"[encode_v3v3] {e}")
        return None


def encode_v2v2_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V2/V2 arb path.

    V2 pairs have no native callback mechanism (swap() with data=""
    just does a direct swap). But the executor's hook/uniswapV2Call
    callbacks enable flash borrows: swap with non-empty data triggers
    a callback to the executor, which delivers the next queued payload.

    Token flow (example for V2_A(zfo=True) → V2_B(zfo=False)):
      V2_A: WETH_in → forward_out (to executor via flash borrow)
      V2_B: forward_in → WETH_out (to executor)
      Pay V2_A: WETH transfer inside V2_A's callback

    Payload sequence:
      0: V2_A.swap(0, forward_out, executor, data) [will_callback=True]
         → V2_A sends forward to executor, then calls executor's V2 callback
      1: <forward>.transfer(V2_B, amount) [will_callback=False]
         → executor sends forward to V2_B (delivered inside V2_A callback)
      2: V2_B.swap(WETH_out, 0, executor, b"") [will_callback=False]
         → V2_B sends WETH to executor (no callback needed)
      3: WETH.transfer(V2_A, amount) [will_callback=False]
         → executor sends WETH to V2_A to pay flash borrow
         → (delivered inside V2_A callback, before invariant check)

    V2 invariant check uses balanceOf(self), not reserves. Tokens can
    arrive at any point during the transaction — before swap, during
    callback, or after callback — as long as they're there when the
    invariant check runs.
    """
    v2_a = path_info.hops[0].pool
    v2_b = path_info.hops[1].pool
    zfo_a = path_info.hops[0].zfo
    zfo_b = path_info.hops[1].zfo

    try:
        # Calculate V2_A output (first hop)
        token_in_a = v2_a.token0 if zfo_a else v2_a.token1
        token_out_a = v2_a.token1 if zfo_a else v2_a.token0
        output_a = v2_a.calculate_tokens_out_from_tokens_in(
            token_in=token_in_a,
            token_in_quantity=optimal_input,
        )
        if output_a <= 0:
            return None

        # Calculate V2_B output (second hop)
        token_in_b = v2_b.token0 if zfo_b else v2_b.token1
        output_b = v2_b.calculate_tokens_out_from_tokens_in(
            token_in=token_in_b,
            token_in_quantity=output_a,
        )
        if output_b <= 0:
            return None

        # V2_A flash swap: borrow forward token, will_callback to get payment
        # flash_borrow=True passes non-empty data to trigger V2 callback
        v2_a_data = encode_v2_swap_calldata(
            zero_for_one=zfo_a,
            amount_out=output_a,
            recipient=executor_address,
            flash_borrow=True,
        )

        # Transfer forward token to V2_B
        transfer_fwd_data = encode_erc20_transfer_calldata(
            recipient=v2_b.address,
            amount=output_a,
        )

        # V2_B swap: send WETH to executor
        v2_b_data = encode_v2_swap_calldata(
            zero_for_one=zfo_b,
            amount_out=output_b,
            recipient=executor_address,
        )

        # Pay V2_A flash borrow: transfer input token back to V2_A
        # The exact amount V2_A needs is optimal_input (the WETH we put in)
        transfer_pay_a_data = encode_erc20_transfer_calldata(
            recipient=v2_a.address,
            amount=optimal_input,
        )

        return [
            (v2_a.address, v2_a_data, True),  # V2_A callbacks executor
            (token_out_a.address, transfer_fwd_data, False),  # Forward to V2_B
            (v2_b.address, v2_b_data, False),  # V2_B sends WETH to executor
            (token_in_a.address, transfer_pay_a_data, False),  # Pay V2_A flash borrow
        ]
    except Exception as e:
        bot_logger.info(f"[encode_v2v2] {e}")
        return None


def encode_v2v3_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V2/V3 arb path.

    V2 flash borrows the intermediate token, V3 receives it during its
    callback and pays WETH back through the V2 callback.

    Token flow (example for V2_A(zfo=True) → V3_B(zfo=False)):
      V2_A: WETH_in → forward_out (to executor via flash borrow)
      V3_B: forward_in → WETH_out (to executor)
      Pay V3_B: forward transfer during V3_B callback
      Pay V2_A: WETH transfer during V2_A callback

    Payload sequence:
      0: V2_A.swap(0, forward_out, executor, data) [will_callback=True]
         → V2_A sends forward to executor, then calls executor's V2 callback
      1: V3_B.swap(recipient=executor, zfo=False, ...) [will_callback=True]
         → delivered inside V2_A callback
         → V3_B sends WETH to executor, callbacks executor (nested)
      2: <forward>.transfer(V3_B, amount) [will_callback=False]
         → delivered inside V3_B callback
         → pays V3_B's forward debt
      3: WETH.transfer(V2_A, amount) [will_callback=False]
         → delivered inside V2_A callback (after V3_B callback returns)
         → pays V2_A's WETH flash borrow
    """
    v2_a = path_info.hops[0].pool
    v3_b = path_info.hops[1].pool
    zfo_a = path_info.hops[0].zfo
    zfo_b = path_info.hops[1].zfo

    try:
        # Calculate V2_A output
        token_in_a = v2_a.token0 if zfo_a else v2_a.token1
        token_out_a = v2_a.token1 if zfo_a else v2_a.token0
        output_a = v2_a.calculate_tokens_out_from_tokens_in(
            token_in=token_in_a,
            token_in_quantity=optimal_input,
        )
        if output_a <= 0:
            return None

        # Calculate V3_B output
        token_in_b = v3_b.token0 if zfo_b else v3_b.token1
        output_b = v3_b.calculate_tokens_out_from_tokens_in(
            token_in=token_in_b,
            token_in_quantity=output_a,
        )
        if output_b <= 0:
            return None

        # V2_A flash swap: borrow intermediate token
        # flash_borrow=True passes non-empty data to trigger V2 callback
        v2_a_data = encode_v2_swap_calldata(
            zero_for_one=zfo_a,
            amount_out=output_a,
            recipient=executor_address,
            flash_borrow=True,
        )

        # V3_B swap: send WETH to executor
        sqrt_limit_b = (MIN_SQRT_RATIO + 1) if zfo_b else (MAX_SQRT_RATIO - 1)
        v3_b_data = encode_v3_swap_calldata(
            recipient=executor_address,
            zero_for_one=zfo_b,
            amount_specified=output_a,  # exact input (positive)
            sqrt_price_limit_x96=sqrt_limit_b,
        )

        # Pay V3_B: transfer token V3_B is owed
        token_b_owed = v3_b.token0 if zfo_b else v3_b.token1
        pay_b_data = encode_erc20_transfer_calldata(
            recipient=v3_b.address,
            amount=output_a,
        )

        # Pay V2_A: transfer input token back (flash borrow repayment)
        pay_a_data = encode_erc20_transfer_calldata(
            recipient=v2_a.address,
            amount=optimal_input,
        )

        payloads: list[tuple[str, bytes, bool]] = [
            (v2_a.address, v2_a_data, True),  # V2_A callbacks executor
            (v3_b.address, v3_b_data, True),  # V3_B callbacks executor (nested)
        ]

        # Pay V3_B: transfer token V3_B is owed.
        # IMPORTANT: The executor's V3 callback auto-pays WETH when a pool
        # is owed WETH. Skip the explicit WETH transfer if auto-pay handles it.
        auto_pay_v3 = token_b_owed.address == WETH_ADDRESS
        if not auto_pay_v3:
            pay_b_data = encode_erc20_transfer_calldata(
                recipient=v3_b.address,
                amount=output_a,
            )
            payloads.append((
                token_b_owed.address,
                pay_b_data,
                False,
            ))  # Pay V3_B during V3_B callback

        # Pay V2_A: transfer input token back (flash borrow repayment)
        pay_a_data = encode_erc20_transfer_calldata(
            recipient=v2_a.address,
            amount=optimal_input,
        )
        payloads.append((token_in_a.address, pay_a_data, False))  # Pay V2_A during V2_A callback

        # ── Diagnostic: log V2-V3 encoding details ──────────────
        bot_logger.info(
            f"[encode_v2v3] v2={v2_a.address} zfo_a={zfo_a} "
            f"v3={v3_b.address} zfo_b={zfo_b} "
            f"token_in_a={token_in_a.address} token_out_a={token_out_a.address} "
            f"token_in_b={token_in_b.address} token_b_owed={token_b_owed.address} "
            f"optimal_input={optimal_input} output_a={output_a} output_b={output_b} "
            f"auto_pay_v3={auto_pay_v3} n_payloads={len(payloads)}"
        )

        return payloads
    except Exception as e:
        bot_logger.info(f"[encode_v2v3] {e}")
        return None


def encode_v3v2_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V3/V2 arb path.

    V3 is always the first payload — its callback provides the flash borrow
    mechanism and chains subsequent operations.

    Returns a list of (target, calldata, will_callback) tuples, or None if
    encoding fails.

    Case 1 — V3 "buy forward" (zfo_v3=True, WETH→forward):
      V3 sends forward to executor (recipient=executor), callbacks executor,
      executor transfers forward tokens to V2 (raw_call — no return value check),
      then delivers V2 swap which sends WETH back to executor,
      V3 callback auto-pays WETH to V3.

    Case 2 — V3 "sell forward" (zfo_v3=False, forward→WETH):
      V3 sends WETH to executor, callbacks executor, executor then
      transfers WETH to V2 pool and delivers V2 swap which sends
      forward to V3 pool (satisfying V3's debt).
    """
    v3_pool = path_info.hops[0].pool
    v2_pool = path_info.hops[1].pool
    zfo_v3 = path_info.hops[0].zfo
    zfo_v2 = path_info.hops[1].zfo

    try:
        if zfo_v3:
            # ── Case 1: V3 "buy forward" — V2 has higher ROE ──────────
            # optimal_input = WETH going into V3
            weth_in = optimal_input

            # Calculate V3 output (forward token amount)
            forward_out = v3_pool.calculate_tokens_out_from_tokens_in(
                token_in=v3_pool.token0,  # zfo=True → selling token0=WETH
                token_in_quantity=weth_in,
            )
            if forward_out <= 0:
                return None

            # Calculate V2 output (WETH coming back)
            forward_token = v3_pool.token1  # zfo=True → token1 is forward
            weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
                token_in=forward_token,
                token_in_quantity=forward_out,
            )
            if weth_out <= 0:
                return None

            # Sanity check: weth_out must cover V3's WETH debt (= weth_in)
            if weth_out <= weth_in:
                return None

            # V3 swap: send forward to executor (not V2 directly!)
            # Sending directly to V2 triggers V3's TransferHelper.safeTransfer()
            # which reverts with "TF" on tax tokens. Sending to executor avoids this.
            v3_data = encode_v3_swap_calldata(
                recipient=executor_address,
                zero_for_one=True,
                amount_specified=weth_in,  # exact input (positive)
                sqrt_price_limit_x96=MIN_SQRT_RATIO + 1,
            )

            # Transfer forward tokens from executor to V2 (raw_call — no return check)
            transfer_data = encode_erc20_transfer_calldata(
                recipient=v2_pool.address,
                amount=forward_out,
            )

            # V2 swap: send WETH to executor (auto-pay to V3 from callback)
            v2_data = encode_v2_swap_calldata(
                zero_for_one=zfo_v2,
                amount_out=weth_out,
                recipient=executor_address,
            )

            bot_logger.info(
                f"[encode_v3v2] Case1: weth_in={weth_in} forward_out={forward_out} "
                f"weth_out={weth_out} profit={weth_out - weth_in} "
                f"v3={v3_pool.address[:10]} v2={v2_pool.address[:10]}"
            )
            return [
                (v3_pool.address, v3_data, True),  # V3 callbacks executor
                (forward_token.address, transfer_data, False),  # Transfer forward to V2
                (v2_pool.address, v2_data, False),  # V2 sends WETH to executor
            ]
        else:
            # ── Case 2: V3 "sell forward" — V3 receives WETH, sends forward out ────
            # zfo_v3=False: V3 receives token1 (input), sends token0 (output)
            # If token1=WETH: optimal_input = WETH amount going INTO V3
            #
            # Token flow:
            #   V3: receives optimal_input WETH, sends forward_out to executor
            #   P1: forward_token.transfer(executor → V2) — give V2 the forward token
            #   P2: V2.swap → sends WETH to executor
            #   Auto-pay: WETH.transfer(executor → V3, optimal_input) — pays V3's WETH debt
            #   Profit: weth_from_V2 - optimal_input, (stays in executor)
            weth_in = optimal_input  # WETH amount going into V3

            # Determine which token V3 sends OUT (the "forward" token)
            # zfo=False: V3 sends token0 out, receives token1 in (WETH)
            forward_token = v3_pool.token0  # the token V3 sends to executor
            owed_token = v3_pool.token1  # token V3 receives (WETH)

            # Verify the owed token IS WETH (required for auto-pay)
            if owed_token.address != WETH_ADDRESS:
                return None

            # Calculate V3's forward token output from the WETH input
            forward_out = v3_pool.calculate_tokens_out_from_tokens_in(
                token_in=owed_token,  # WETH going in
                token_in_quantity=weth_in,
            )
            if forward_out <= 0:
                return None

            # Calculate V2's WETH output from the forward token input
            weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
                token_in=forward_token,
                token_in_quantity=forward_out,
            )
            if weth_out <= 0:
                return None

            # Sanity check: weth_out must cover V3's WETH debt (= weth_in)
            # For profit: weth_out > weth_in
            if weth_out <= weth_in:
                return None

            # V3 swap: receive WETH, send forward to executor
            # zfo=False: token1 (WETH) is input, token0 (forward) is output
            # Recipient=executor: forward tokens go to executor for P1 transfer
            v3_data = encode_v3_swap_calldata(
                recipient=executor_address,
                zero_for_one=False,
                amount_specified=weth_in,  # exact input of WETH (positive)
                sqrt_price_limit_x96=MAX_SQRT_RATIO - 1,
            )

            # Transfer forward token from executor to V2
            transfer_data = encode_erc20_transfer_calldata(
                recipient=v2_pool.address,
                amount=forward_out,
            )

            # V2 swap: V2 receives forward token, sends WETH to executor
            # We want WETH_OUT from V2. Using V2's calculated weth_out.
            # zfo convention: True → (0, amount_out)→token1 out, False → (amount_out, 0)→token0 out
            # WETH out: if WETH=token0 → zfo=False (token0 out), if WETH=token1 → zfo=True (token1 out)
            v2_sends_weth_zfo = v2_pool.token1.address == WETH_ADDRESS

            v2_data = encode_v2_swap_calldata(
                zero_for_one=v2_sends_weth_zfo,
                amount_out=weth_out,
                recipient=executor_address,
            )

            bot_logger.info(
                f"[encode_v3v2] Case2: weth_in={weth_in} forward_out={forward_out} "
                f"weth_out={weth_out} profit={weth_out - weth_in} "
                f"v3={v3_pool.address[:10]} v2={v2_pool.address[:10]}"
            )
            return [
                (v3_pool.address, v3_data, True),  # V3 callbacks executor
                (forward_token.address, transfer_data, False),  # Transfer forward to V2
                (v2_pool.address, v2_data, False),  # V2 sends WETH to executor, auto-pay to V3
            ]
    except Exception as e:
        bot_logger.info(f"[encode_v3v2] {e}")
        return None


# ──────────────────────────────────────────────────────────────────
# V4 swap payload encoding
# ──────────────────────────────────────────────────────────────────


def _v4_pool_key_salient(pool: UniswapV4Pool) -> tuple[str, str, int, int, str]:
    """Extract V4 PoolKey fields needed for swap calldata encoding."""
    return (
        pool.token0.address,
        pool.token1.address,
        pool.fee,
        pool.tick_spacing,
        pool.hook_address,
    )


def encode_v4v4_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V4/V4 arb path.

    Both V4 pools operate through PoolManager. The entry point is
    PoolManager.unlock(), which triggers unlockCallback on the executor.
    Inside the callback, all V4 operations are delivered from the queue.

    Token flow (example for V4_A(zfo=True) → V4_B(zfo=False)):
      V4_A: WETH_in → forward_out (owe WETH to PM, PM owes forward)
      V4_B: forward_in → WETH_out (owe forward to PM, PM owes WETH)
      Profit: weth_out - weth_in (stays in executor via PM.take)

    Payload sequence:
      P0: PoolManager.unlock(data) [will_callback=True]
         → triggers unlockCallback, executor delivers remaining payloads
      P1: PoolManager.swap(V4_A_key, params, hookData) [will_callback=False]
         → V4_A executes, PM tracks deltas
      P2: PoolManager.swap(V4_B_key, params, hookData) [will_callback=False]
         → V4_B executes, PM tracks deltas
      P3: <ERC20>.transfer(PoolManager, forward_amount) [will_callback=False]
         → move forward token to PM to settle V4_B debt
      P4: PoolManager.sync(forward_token) [will_callback=False]
         → PM reads new forward balance
      P5: PoolManager.settle() [will_callback=False]
         → PM credits forward to our delta
      P6: PoolManager.take(WETH, executor, weth_profit) [will_callback=False]
         → receive profit from PM
      P7: <WETH>.transfer(PoolManager, weth_owed) [will_callback=False]
         → pay WETH to PM for V4_A debt
      P8: PoolManager.sync(WETH) [will_callback=False]
      P9: PoolManager.settle() [will_callback=False]
         → PM credits WETH; net delta should be zero for WETH

    For V4 pools where ETH is a currency (address(0)):
    - take() receives ETH (sent as value, not ERC-20)
    - settle() with msg.value pays ETH debt

    V4 amountSpecified: NEGATIVE for exact-input (opposite of V3!)
    """
    v4_a = path_info.hops[0].pool
    v4_b = path_info.hops[1].pool
    zfo_a = path_info.hops[0].zfo
    zfo_b = path_info.hops[1].zfo
    pm = UNISWAP_V4_POOL_MANAGER_ADDRESS

    try:
        # Calculate V4_A output
        token_in_a = v4_a.token0 if zfo_a else v4_a.token1
        token_out_a = v4_a.token1 if zfo_a else v4_a.token0
        output_a = v4_a.calculate_tokens_out_from_tokens_in(
            token_in=token_in_a,
            token_in_quantity=optimal_input,
        )
        if output_a <= 0:
            return None

        # Calculate V4_B output
        token_in_b = v4_b.token0 if zfo_b else v4_b.token1
        output_b = v4_b.calculate_tokens_out_from_tokens_in(
            token_in=token_in_b,
            token_in_quantity=output_a,
        )
        if output_b <= 0:
            return None

        # V4 Key fields
        key_a = _v4_pool_key_salient(v4_a)
        key_b = _v4_pool_key_salient(v4_b)

        # V4 swap calldata — amountSpecified is NEGATIVE for exact-input
        sqrt_limit_a = V4_MIN_SQRT_PRICE_X96 if zfo_a else V4_MAX_SQRT_PRICE_X96
        v4_a_data = encode_v4_swap_calldata(
            currency0=key_a[0],
            currency1=key_a[1],
            fee=key_a[2],
            tick_spacing=key_a[3],
            hooks=key_a[4],
            zero_for_one=zfo_a,
            amount_specified=-optimal_input,  # V4: negative = exact-input
            sqrt_price_limit_x96=sqrt_limit_a,
        )

        sqrt_limit_b = V4_MIN_SQRT_PRICE_X96 if zfo_b else V4_MAX_SQRT_PRICE_X96
        v4_b_data = encode_v4_swap_calldata(
            currency0=key_b[0],
            currency1=key_b[1],
            fee=key_b[2],
            tick_spacing=key_b[3],
            hooks=key_b[4],
            zero_for_one=zfo_b,
            amount_specified=-output_a,  # V4: negative = exact-input
            sqrt_price_limit_x96=sqrt_limit_b,
        )

        # Determine which tokens are ETH (address(0)) vs ERC-20
        forward_token = token_out_a  # Token from V4_A, going into V4_B
        forward_is_eth = forward_token.address == ZERO_ADDRESS
        weth_token = token_in_a  # WETH going into V4_A
        weth_is_eth = weth_token.address == ZERO_ADDRESS

        # V4 unlock entry — triggers unlockCallback
        unlock_data = encode_v4_unlock_calldata(b"")

        payloads: list[tuple[str, bytes, bool]] = [
            (pm, unlock_data, True),  # P0: Unlock — triggers callback
            (pm, v4_a_data, False),  # P1: V4_A swap
            (pm, v4_b_data, False),  # P2: V4_B swap
        ]

        # P3-P5: Settle forward token debt to PM (V4_B owes us forward credits,
        #         we owe forward to PM for V4_A's output side)
        #         After both swaps, the net forward delta should be ~0
        #         But we need to ensure PM has enough forward tokens.
        #         We take forward from PM (what V4_A gave us), then pay it
        #         to PM (what V4_B is owed). Net = 0.
        #         Actually: after V4_A swap, PM owes us forward.
        #         After V4_B swap, we owe PM forward.
        #         These cancel out automatically in PM's delta accounting.

        # Check if we need explicit settlement for the forward token.
        # If V4_A owes forward (delta > 0 for us) and V4_B wants forward
        # (delta < 0 for us), the net delta should be zero or close.
        # But for the WETH direction:
        # We owe WETH for V4_A input, PM owes WETH for V4_B output.
        # Net WETH delta = -optimal_input + output_b = profit

        # Take WETH profit from PM
        if output_b > optimal_input and not weth_is_eth:
            take_data = encode_v4_take_calldata(
                currency=WETH_ADDRESS,
                to=executor_address,
                amount=output_b - optimal_input,
            )
            payloads.append((pm, take_data, False))  # Take profit
        elif weth_is_eth:
            # For pure ETH pools, take ETH profit
            take_data = encode_v4_take_calldata(
                currency=ZERO_ADDRESS,
                to=executor_address,
                amount=output_b - optimal_input,
            )
            payloads.append((pm, take_data, False))

        # Pay WETH to PM for V4_A input debt
        # The executor already has WETH from V4_B's take, and needs to pay V4_A's debt.
        if not weth_is_eth:
            weth_to_pm = optimal_input  # What V4_A is owed
            pay_pm_data = encode_erc20_transfer_calldata(pm, weth_to_pm)
            sync_data = encode_v4_sync_calldata(WETH_ADDRESS)
            settle_data = encode_v4_settle_calldata()
            payloads.extend([
                (WETH_ADDRESS, pay_pm_data, False),  # Transfer WETH to PM
                (pm, sync_data, False),  # Sync WETH balance
                (pm, settle_data, False),  # Settle WETH debt
            ])

        # Also need to settle forward token if net delta is non-zero
        # (V4_A output = output_a, V4_B input = output_a → net forward delta = 0)
        # No explicit forward settlement needed — deltas cancel.

        bot_logger.info(
            f"[encode_v4v4] v4_a={v4_a} zfo_a={zfo_a} "
            f"v4_b={v4_b} zfo_b={zfo_b} "
            f"optimal_input={optimal_input} output_a={output_a} output_b={output_b} "
            f"profit={output_b - optimal_input} n_payloads={len(payloads)}"
        )

        return payloads
    except Exception as e:
        bot_logger.info(f"[encode_v4v4] {e}")
        return None


def encode_v4v3_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V4→V3 arb path.

    V4 is the first pool. Entry via PoolManager.unlock().
    Inside unlockCallback: V4 swap, take forward from PM, give to V3, V3 swap.
    V3 callback auto-pays WETH to V3, then unlockCallback settles WETH with PM.

    Token flow (V4_A(zfo=True) → V3_B(zfo=False)):
      V4_A: WETH_in → forward_out (PM owes forward, we owe WETH to PM)
      V3_B: forward_in → WETH_out (V3 sends WETH to executor)
      Pay V3: WETH auto-pay from callback
      Settle PM: WETH.transfer(PM) + sync + settle

    Payload sequence:
      P0: PoolManager.unlock() [will_callback=True]
      P1: PoolManager.swap(V4_A) — V4 swap
      P2: PoolManager.take(forward, executor, amount) — receive forward from PM
      P3: <forward>.transfer(V3_pool, amount) — give forward to V3
      P4: V3.swap(recipient=executor) [will_callback=True] — V3 sends WETH
      # Inside V3 callback, remaining payloads are delivered:
      # V3 auto-pay: WETH → V3_pool (if V3 is owed WETH)
      P5: WETH.transfer(PoolManager, weth_owed) — pay PM for V4_A debt
      P6: PoolManager.sync(WETH) — read PM's WETH balance
      P7: PoolManager.settle() — credit WETH to our delta
    """
    v4_pool = path_info.hops[0].pool
    v3_pool = path_info.hops[1].pool
    zfo_v4 = path_info.hops[0].zfo
    zfo_v3 = path_info.hops[1].zfo
    pm = UNISWAP_V4_POOL_MANAGER_ADDRESS

    try:
        # Calculate V4 output
        token_in_v4 = v4_pool.token0 if zfo_v4 else v4_pool.token1
        token_out_v4 = v4_pool.token1 if zfo_v4 else v4_pool.token0
        forward_out = v4_pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in_v4,
            token_in_quantity=optimal_input,
        )
        if forward_out <= 0:
            return None

        # Calculate V3 output
        token_in_v3 = v3_pool.token0 if zfo_v3 else v3_pool.token1
        weth_out = v3_pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in_v3,
            token_in_quantity=forward_out,
        )
        if weth_out <= 0:
            return None

        # V4 Key fields
        key = _v4_pool_key_salient(v4_pool)

        # V4 swap calldata — amountSpecified is NEGATIVE for exact-input
        sqrt_limit_v4 = V4_MIN_SQRT_PRICE_X96 if zfo_v4 else V4_MAX_SQRT_PRICE_X96
        v4_swap_data = encode_v4_swap_calldata(
            currency0=key[0],
            currency1=key[1],
            fee=key[2],
            tick_spacing=key[3],
            hooks=key[4],
            zero_for_one=zfo_v4,
            amount_specified=-optimal_input,  # V4: negative = exact-input
            sqrt_price_limit_x96=sqrt_limit_v4,
        )

        # Determine forward token address — may be ETH (address(0))
        forward_addr = token_out_v4.address
        forward_is_native = forward_addr == ZERO_ADDRESS

        # Take forward from PM
        take_fwd_data = encode_v4_take_calldata(
            currency=forward_addr,
            to=executor_address,
            amount=forward_out,
        )

        # V3 swap calldata — amountSpecified is POSITIVE for exact-input
        sqrt_limit_v3 = (MIN_SQRT_RATIO + 1) if zfo_v3 else (MAX_SQRT_RATIO - 1)
        v3_swap_data = encode_v3_swap_calldata(
            recipient=executor_address,
            zero_for_one=zfo_v3,
            amount_specified=forward_out,  # V3: positive = exact-input
            sqrt_price_limit_x96=sqrt_limit_v3,
        )

        # V3 token owed — for auto-pay check
        token_v3_owed = v3_pool.token0 if zfo_v3 else v3_pool.token1
        auto_pay_v3 = token_v3_owed.address == WETH_ADDRESS

        # Build payload sequence
        unlock_data = encode_v4_unlock_calldata(b"")

        payloads: list[tuple[str, bytes, bool]] = [
            (pm, unlock_data, True),  # P0: Unlock
            (pm, v4_swap_data, False),  # P1: V4 swap
        ]

        # Take forward from PM (skip if native ETH — no ERC-20 take needed)
        if not forward_is_native:
            payloads.append((pm, take_fwd_data, False))  # P2: Take forward

            # Transfer forward to V3
            transfer_fwd_data = encode_erc20_transfer_calldata(v3_pool.address, forward_out)
            payloads.append((forward_addr, transfer_fwd_data, False))  # P3: Forward → V3

        # V3 swap
        payloads.append((v3_pool.address, v3_swap_data, True))  # P4: V3 swap

        # Pay V3: skip if auto-pay handles it (V3 is owed WETH)
        if not auto_pay_v3:
            pay_v3_data = encode_erc20_transfer_calldata(v3_pool.address, forward_out)
            payloads.append((token_v3_owed.address, pay_v3_data, False))

        # Settle WETH with PM — V3 callback returns, we're still in unlockCallback
        # Pay WETH the executor owes PM for the V4 swap
        pay_pm_data = encode_erc20_transfer_calldata(pm, optimal_input)
        sync_data = encode_v4_sync_calldata(WETH_ADDRESS)
        settle_data = encode_v4_settle_calldata()
        payloads.extend([
            (WETH_ADDRESS, pay_pm_data, False),  # WETH → PM
            (pm, sync_data, False),  # Sync WETH
            (pm, settle_data, False),  # Settle
        ])

        bot_logger.info(
            f"[encode_v4v3] v4={v4_pool} zfo_v4={zfo_v4} "
            f"v3={v3_pool.address[:10]} zfo_v3={zfo_v3} "
            f"optimal_input={optimal_input} forward_out={forward_out} weth_out={weth_out} "
            f"auto_pay_v3={auto_pay_v3} n_payloads={len(payloads)}"
        )

        return payloads
    except Exception as e:
        bot_logger.info(f"[encode_v4v3] {e}")
        return None


def encode_v3v4_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V3→V4 arb path.

    V3 is the first pool (flash borrow via callback).
    Inside V3 callback: PoolManager.unlock() → V4 swap → settlement.

    Token flow (V3_A(zfo=True) → V4_B(zfo=False)):
      V3_A: WETH_in → forward_out (V3 sends forward to executor, callbacks)
      V4_B: forward_in → WETH_out (PM owes WETH, we owe forward to PM)
      Pay V3: WETH auto-pay from callback
      Settle PM: forward to PM + sync + settle, then take WETH from PM

    Payload sequence:
      P0: V3.swap(recipient=executor) [will_callback=True]
         → V3 sends forward to executor, callbacks executor
      # Inside V3 callback:
      P1: PoolManager.unlock() [will_callback=True]
         → triggers unlockCallback inside V3 callback
         → P2-P7 delivered inside unlockCallback
      P2: PoolManager.swap(V4_B) — V4 swap
      P3: <forward>.transfer(PM, amount) — pay forward to PM
      P4: PoolManager.sync(forward) — PM reads forward balance
      P5: PoolManager.settle() — credit forward to our PM delta
      P6: PoolManager.take(WETH, executor, amount) — receive WETH profit
      # unlockCallback returns
      # V3 callback auto-pay: WETH → V3_A (if V3 is owed WETH)
    """
    v3_pool = path_info.hops[0].pool
    v4_pool = path_info.hops[1].pool
    zfo_v3 = path_info.hops[0].zfo
    zfo_v4 = path_info.hops[1].zfo
    pm = UNISWAP_V4_POOL_MANAGER_ADDRESS

    try:
        # Calculate V3 output
        token_in_v3 = v3_pool.token0 if zfo_v3 else v3_pool.token1
        token_out_v3 = v3_pool.token1 if zfo_v3 else v3_pool.token0
        forward_out = v3_pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in_v3,
            token_in_quantity=optimal_input,
        )
        if forward_out <= 0:
            return None

        # Calculate V4 output
        token_in_v4 = v4_pool.token0 if zfo_v4 else v4_pool.token1
        weth_out = v4_pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in_v4,
            token_in_quantity=forward_out,
        )
        if weth_out <= 0:
            return None

        # V3 swap calldata — POSITIVE for exact-input
        sqrt_limit_v3 = (MIN_SQRT_RATIO + 1) if zfo_v3 else (MAX_SQRT_RATIO - 1)
        v3_data = encode_v3_swap_calldata(
            recipient=executor_address,
            zero_for_one=zfo_v3,
            amount_specified=optimal_input,  # V3: positive = exact-input
            sqrt_price_limit_x96=sqrt_limit_v3,
        )

        # V4 Key fields
        key = _v4_pool_key_salient(v4_pool)

        # V4 swap calldata — NEGATIVE for exact-input
        sqrt_limit_v4 = V4_MIN_SQRT_PRICE_X96 if zfo_v4 else V4_MAX_SQRT_PRICE_X96
        v4_swap_data = encode_v4_swap_calldata(
            currency0=key[0],
            currency1=key[1],
            fee=key[2],
            tick_spacing=key[3],
            hooks=key[4],
            zero_for_one=zfo_v4,
            amount_specified=-forward_out,  # V4: negative = exact-input
            sqrt_price_limit_x96=sqrt_limit_v4,
        )

        # Forward token address — may be ETH/NATIVE (address(0))
        forward_addr = token_out_v3.address
        forward_is_native = forward_addr == ZERO_ADDRESS

        # V3 token owed — for auto-pay check
        token_v3_owed = v3_pool.token0 if zfo_v3 else v3_pool.token1
        auto_pay_v3 = token_v3_owed.address == WETH_ADDRESS

        # Build payload sequence
        unlock_data = encode_v4_unlock_calldata(b"")

        payloads: list[tuple[str, bytes, bool]] = [
            (v3_pool.address, v3_data, True),  # P0: V3 swap — callbacks executor
        ]

        # Inside V3 callback: PoolManager.unlock (triggers unlockCallback)
        payloads.append((pm, unlock_data, True))  # P1: Unlock

        # Inside unlockCallback: V4 swap
        payloads.append((pm, v4_swap_data, False))  # P2: V4 swap

        # Pay forward to PM for V4_B debt
        if not forward_is_native:
            fwd_to_pm = encode_erc20_transfer_calldata(pm, forward_out)
            payloads.append((forward_addr, fwd_to_pm, False))  # P3: Transfer forward to PM
            sync_fwd = encode_v4_sync_calldata(forward_addr)
            payloads.append((pm, sync_fwd, False))  # P4: Sync forward
            settle = encode_v4_settle_calldata()
            payloads.append((pm, settle, False))  # P5: Settle forward

        # Take WETH from PM (profit + V3 repayment)
        take_data = encode_v4_take_calldata(
            currency=WETH_ADDRESS,
            to=executor_address,
            amount=weth_out,
        )
        payloads.append((pm, take_data, False))  # P6: Take WETH from PM

        # V3 callback auto-pay: if V3 is owed WETH, auto-transfer handles it.
        # The V3 callback will auto-pay after all payloads are delivered.
        # No explicit WETH transfer to V3 needed when auto-pay covers it.

        bot_logger.info(
            f"[encode_v3v4] v3={v3_pool.address[:10]} zfo_v3={zfo_v3} "
            f"v4={v4_pool} zfo_v4={zfo_v4} "
            f"optimal_input={optimal_input} forward_out={forward_out} weth_out={weth_out} "
            f"auto_pay_v3={auto_pay_v3} n_payloads={len(payloads)}"
        )

        return payloads
    except Exception as e:
        bot_logger.info(f"[encode_v3v4] {e}")
        return None


def encode_v4v2_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V4→V2 arb path.

    V4 is the first pool. Entry via PoolManager.unlock().
    Inside unlockCallback: V4 swap, take forward from PM, give to V2, V2 swap,
    then settle WETH with PM.

    Token flow (V4_A(zfo=True) → V2_B(zfo=False)):
      V4_A: WETH_in → forward_out (PM owes forward, we owe WETH to PM)
      V2_B: forward_in → WETH_out (V2 sends WETH to executor)
      Settle PM: WETH.transfer(PM) + sync + settle

    V2 has no callback — it just sends tokens and does an invariant check.
    """
    v4_pool = path_info.hops[0].pool
    v2_pool = path_info.hops[1].pool
    zfo_v4 = path_info.hops[0].zfo
    zfo_v2 = path_info.hops[1].zfo
    pm = UNISWAP_V4_POOL_MANAGER_ADDRESS

    try:
        # Calculate V4 output
        token_in_v4 = v4_pool.token0 if zfo_v4 else v4_pool.token1
        token_out_v4 = v4_pool.token1 if zfo_v4 else v4_pool.token0
        forward_out = v4_pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in_v4,
            token_in_quantity=optimal_input,
        )
        if forward_out <= 0:
            return None

        # Calculate V2 output
        token_in_v2 = v2_pool.token0 if zfo_v2 else v2_pool.token1
        weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in_v2,
            token_in_quantity=forward_out,
        )
        if weth_out <= 0:
            return None

        # V4 Key fields
        key = _v4_pool_key_salient(v4_pool)

        # V4 swap calldata — NEGATIVE for exact-input
        sqrt_limit_v4 = V4_MIN_SQRT_PRICE_X96 if zfo_v4 else V4_MAX_SQRT_PRICE_X96
        v4_swap_data = encode_v4_swap_calldata(
            currency0=key[0],
            currency1=key[1],
            fee=key[2],
            tick_spacing=key[3],
            hooks=key[4],
            zero_for_one=zfo_v4,
            amount_specified=-optimal_input,  # V4: negative = exact-input
            sqrt_price_limit_x96=sqrt_limit_v4,
        )

        forward_addr = token_out_v4.address
        forward_is_native = forward_addr == ZERO_ADDRESS

        # Take forward from PM
        take_fwd_data = encode_v4_take_calldata(
            currency=forward_addr,
            to=executor_address,
            amount=forward_out,
        )

        # V2 swap calldata
        v2_swap_data = encode_v2_swap_calldata(
            zero_for_one=zfo_v2,
            amount_out=weth_out,
            recipient=executor_address,
        )

        # Build payload sequence
        unlock_data = encode_v4_unlock_calldata(b"")
        payloads: list[tuple[str, bytes, bool]] = [
            (pm, unlock_data, True),  # P0: Unlock
            (pm, v4_swap_data, False),  # P1: V4 swap
        ]

        # Take forward from PM
        if not forward_is_native:
            payloads.append((pm, take_fwd_data, False))  # P2: Take forward
            # Transfer forward to V2
            transfer_fwd_data = encode_erc20_transfer_calldata(v2_pool.address, forward_out)
            payloads.append((forward_addr, transfer_fwd_data, False))  # P3: Forward → V2

        # V2 swap
        payloads.append((v2_pool.address, v2_swap_data, False))  # P4: V2 swap

        # Settle WETH with PM
        pay_pm_data = encode_erc20_transfer_calldata(pm, optimal_input)
        sync_data = encode_v4_sync_calldata(WETH_ADDRESS)
        settle_data = encode_v4_settle_calldata()
        payloads.extend([
            (WETH_ADDRESS, pay_pm_data, False),  # WETH → PM
            (pm, sync_data, False),  # Sync WETH
            (pm, settle_data, False),  # Settle
        ])

        bot_logger.info(
            f"[encode_v4v2] v4={v4_pool} zfo_v4={zfo_v4} "
            f"v2={v2_pool.address[:10]} zfo_v2={zfo_v2} "
            f"optimal_input={optimal_input} forward_out={forward_out} weth_out={weth_out} "
            f"n_payloads={len(payloads)}"
        )

        return payloads
    except Exception as e:
        bot_logger.info(f"[encode_v4v2] {e}")
        return None


def encode_v2v4_payloads(
    path_info: PathInfo,
    optimal_input: int,
    executor_address: str,
) -> list[tuple[str, bytes, bool]] | None:
    """Encode swap payloads for a V2→V4 arb path.

    V2 is the first pool (flash borrow). Inside V2 callback:
    PoolManager.unlock() → V4 swap → settle forward with PM → take WETH from PM.
    Then pay V2 flash borrow with WETH proceeds.

    Token flow (V2_A(zfo=True) → V4_B(zfo=False)):
      V2_A: WETH_in → forward_out (V2 sends forward, callbacks executor)
      V4_B: forward_in → WETH_out (PM owes WETH, we owe forward to PM)
      Pay V2: WETH.transfer(V2_A) inside V2 callback
      Settle PM: forward → PM + sync + settle; take WETH from PM

    Payload sequence:
      P0: V2.swap(flash_borrow=True) [will_callback=True]
         → V2 sends forward to executor, callbacks executor
      # Inside V2 callback:
      P1: PoolManager.unlock() [will_callback=True]
         → triggers unlockCallback
      P2: PoolManager.swap(V4_B)
      P3: <forward>.transfer(PM, amount)
      P4: PoolManager.sync(forward)
      P5: PoolManager.settle()
      P6: PoolManager.take(WETH, executor, amount)
      # unlockCallback returns
      # Back in V2 callback:
      P7: WETH.transfer(V2_A, weth_to_repay)
    """
    v2_pool = path_info.hops[0].pool
    v4_pool = path_info.hops[1].pool
    zfo_v2 = path_info.hops[0].zfo
    zfo_v4 = path_info.hops[1].zfo
    pm = UNISWAP_V4_POOL_MANAGER_ADDRESS

    try:
        # Calculate V2 output
        token_in_v2 = v2_pool.token0 if zfo_v2 else v2_pool.token1
        token_out_v2 = v2_pool.token1 if zfo_v2 else v2_pool.token0
        forward_out = v2_pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in_v2,
            token_in_quantity=optimal_input,
        )
        if forward_out <= 0:
            return None

        # Calculate V4 output
        token_in_v4 = v4_pool.token0 if zfo_v4 else v4_pool.token1
        weth_out = v4_pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in_v4,
            token_in_quantity=forward_out,
        )
        if weth_out <= 0:
            return None

        # V2 flash swap
        v2_swap_data = encode_v2_swap_calldata(
            zero_for_one=zfo_v2,
            amount_out=forward_out,
            recipient=executor_address,
            flash_borrow=True,
        )

        # V4 Key fields
        key = _v4_pool_key_salient(v4_pool)

        # V4 swap calldata — NEGATIVE for exact-input
        sqrt_limit_v4 = V4_MIN_SQRT_PRICE_X96 if zfo_v4 else V4_MAX_SQRT_PRICE_X96
        v4_swap_data = encode_v4_swap_calldata(
            currency0=key[0],
            currency1=key[1],
            fee=key[2],
            tick_spacing=key[3],
            hooks=key[4],
            zero_for_one=zfo_v4,
            amount_specified=-forward_out,  # V4: negative = exact-input
            sqrt_price_limit_x96=sqrt_limit_v4,
        )

        forward_addr = token_out_v2.address
        forward_is_native = forward_addr == ZERO_ADDRESS

        # Build payload sequence
        unlock_data = encode_v4_unlock_calldata(b"")
        payloads: list[tuple[str, bytes, bool]] = [
            (v2_pool.address, v2_swap_data, True),  # P0: V2 flash swap
        ]

        # Inside V2 callback: PoolManager.unlock
        payloads.append((pm, unlock_data, True))  # P1: Unlock

        # Inside unlockCallback: V4 swap
        payloads.append((pm, v4_swap_data, False))  # P2: V4 swap

        # Pay forward to PM for V4 debt
        if not forward_is_native:
            fwd_to_pm = encode_erc20_transfer_calldata(pm, forward_out)
            payloads.append((forward_addr, fwd_to_pm, False))  # P3: Forward → PM
            sync_fwd = encode_v4_sync_calldata(forward_addr)
            payloads.append((pm, sync_fwd, False))  # P4: Sync forward
            settle = encode_v4_settle_calldata()
            payloads.append((pm, settle, False))  # P5: Settle

        # Take WETH from PM
        take_data = encode_v4_take_calldata(
            currency=WETH_ADDRESS,
            to=executor_address,
            amount=weth_out,
        )
        payloads.append((pm, take_data, False))  # P6: Take WETH

        # Back in V2 callback: pay V2 flash borrow with WETH
        pay_v2_data = encode_erc20_transfer_calldata(v2_pool.address, optimal_input)
        payloads.append((WETH_ADDRESS, pay_v2_data, False))  # P7: WETH → V2

        bot_logger.info(
            f"[encode_v2v4] v2={v2_pool.address[:10]} zfo_v2={zfo_v2} "
            f"v4={v4_pool} zfo_v4={zfo_v4} "
            f"optimal_input={optimal_input} forward_out={forward_out} weth_out={weth_out} "
            f"n_payloads={len(payloads)}"
        )

        return payloads
    except Exception as e:
        bot_logger.info(f"[encode_v2v4] {e}")
        return None


# ──────────────────────────────────────────────────────────────────
# Build paths
# ──────────────────────────────────────────────────────────────────


async def build_paths(
    bot: Bot,
    engine_registry: EngineRegistry,
    current_block: int = 0,
) -> None:
    """Discover V3/V2/V4 arb paths, build Python pools, register with Rust engine.

    V4 pools are discovered via find_paths_async (like V2/V3) and built through
    bot.build_managed_pool(). Hook filtering rejects pools with amount-modifying
    hooks (mask 0xCC) and dynamic fees (fee == 0x100000) at registration time.
    """
    uniswap_v3_tracker = bot.add_tracker(
        UniswapV3PoolTracker,
        factory_address=UNISWAP_V3_MAINNET_FACTORY,
    )
    sushiswap_v3_tracker = bot.add_tracker(
        UniswapV3PoolTracker,
        factory_address=SUSHISWAP_V3_MAINNET_FACTORY,
    )
    pancakeswap_v3_tracker = bot.add_tracker(
        UniswapV3PoolTracker,
        factory_address=PANCAKESWAP_V3_MAINNET_FACTORY,
    )
    weth = bot.build_erc20token(WETH_ADDRESS)

    path_count = 0
    v4_pool_count = 0
    v4_hook_rejected = 0
    v4_dynamic_fee_rejected = 0
    registered_path_sigs: set[tuple[str, str, bool, bool]] = set()
    start = time.perf_counter()

    async for pool_step_a, pool_step_b in find_paths_async(  # noqa:PLR1702
        chain_id=bot.connections.default_chain_id,
        start_tokens=[
            WETH_ADDRESS,
            # V4 uses NATIVE_CURRENCY_ADDRESS for ETH pools (currency0=address(0)).
            ZERO_ADDRESS,
        ],
        end_tokens=[
            WETH_ADDRESS,
            ZERO_ADDRESS,
        ],
        max_depth=2,
        pool_types=[
            PancakeswapV2PoolTable,
            PancakeswapV3PoolTable,
            UniswapV2PoolTable,
            UniswapV3PoolTable,
            SushiswapV2PoolTable,
            SushiswapV3PoolTable,
            UniswapV4PoolTable,
        ],
        db=bot.db,
    ):
        await asyncio.sleep(0)

        # Determine pool types for each step
        steps = [pool_step_a, pool_step_b]
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
                    pool = bot.build_pool(step.address)
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
                                pool = bot.build_pool(step.address)
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
            continue

        # ── Token quality filter ─────────────────────────────────
        # Extract the intermediate (non-WETH) token from both pools.
        # A 2-hop arb has the pattern: WETH ↔ intermediate ↔ WETH
        # so the intermediate token appears in both pools.
        # For V4 pools, NATIVE_CURRENCY_ADDRESS (address(0)) represents ETH.
        # Treat it as equivalent to WETH for filtering purposes.
        path_tokens: set[str] = set()
        for pool in pools:
            path_tokens.add(get_checksum_address(pool.token0.address))
            path_tokens.add(get_checksum_address(pool.token1.address))
        # Replace ZERO_ADDRESS (NATIVE) with WETH for filtering
        non_weth_tokens = (path_tokens - {WETH_ADDRESS}) - {ZERO_ADDRESS}

        # Blacklist: skip paths with known scam/tax tokens
        if TOKEN_BLACKLIST_MODE:
            blocked = non_weth_tokens & KNOWN_SCAM_TOKENS
            if blocked:
                continue

        # Whitelist: only allow paths with known-good intermediate tokens
        if TOKEN_WHITELIST_MODE:
            if not non_weth_tokens.issubset(KNOWN_GOOD_TOKENS):
                continue

        # Register with Rust engine
        try:
            for pool, pt in zip(pools, pool_type_strs):
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
            bot_logger.debug(f"V4 pool skipped: {exc}")
            continue
        except Exception as exc:
            bot_logger.debug(f"Engine registration failed: {exc}")
            continue

        # Resolve directions and register path
        # V4 pools use the same token0/token1 model as V3 for direction resolution
        zfo_list = resolve_directions(pools, weth.address)
        if zfo_list is None:
            continue

        # Skip duplicate paths (same pools, same directions)
        # For V4 pools, use pool_id instead of address
        pool_sigs = []
        for p, zfo in zip(pools, zfo_list):
            if isinstance(p, UniswapV4Pool):
                pool_sigs.append(p.pool_id.to_0x_hex())
            else:
                pool_sigs.append(p.address)
        path_sig = (pool_sigs[0], pool_sigs[1], zfo_list[0], zfo_list[1])
        if path_sig in registered_path_sigs:
            continue
        registered_path_sigs.add(path_sig)

        try:
            hops = [
                HopInfo(pool=p, pool_type=pt, zfo=zfo)
                for p, pt, zfo in zip(pools, pool_type_strs, zfo_list)
            ]
            engine_registry.register_path(hops)
        except Exception as exc:
            bot_logger.debug(f"Path registration failed: {exc}")
            continue

        path_count += 1

    engine_registry.engine.freeze()

    # Initial solve: resolve all paths and solve them once.
    # Subsequent process_logs calls use dependency tracking — only
    # paths containing updated pools are re-solved.
    engine_registry.engine.initial_solve(current_block)
    initial_results = engine_registry.profitable_results()
    bot_logger.info(
        f"Initial solve: {len(initial_results)} profitable paths "
        f"(top: {initial_results[0][2] // 10**9}gwei)"
        if initial_results
        else "Initial solve: no profitable paths"
    )

    bot_logger.info(
        f"Built {path_count} paths in {time.perf_counter() - start:.1f}s — "
        f"{engine_registry.engine.v2_pool_count()} V2, "
        f"{engine_registry.engine.v3_pool_count()} V3, "
        f"{v4_pool_count} V4 pools, "
        f"{v4_hook_rejected} V4 hook-rejected, "
        f"{v4_dynamic_fee_rejected} V4 dynamic-fee-rejected, "
        f"{engine_registry.engine.path_count()} paths"
    )

    # ── V3 snapshot backfill ─────────────────────────────────────
    # Fetch Mint/Burn events between the snapshot block and the current
    # block, apply them to Python pools, and push the updated tick_data
    # to the Rust engine one final time. After this, the Rust pump owns
    # all state updates.
    sync_provider = bot.connections.get_provider(chain_id=1)
    v3_trackers = [
        ("Uniswap V3", uniswap_v3_tracker),
        ("Sushiswap V3", sushiswap_v3_tracker),
        ("PancakeSwap V3", pancakeswap_v3_tracker),
    ]
    for tracker_name, tracker in v3_trackers:
        if tracker.snapshot is None:
            continue
        snapshot = tracker.snapshot
        snapshot_block = snapshot.newest_block
        if snapshot_block >= current_block:
            continue
        bot_logger.info(
            f"[backfill] {tracker_name}: fetching Mint/Burn events "
            f"from block {snapshot_block + 1} to {current_block}"
        )
        snapshot.fetch_new_events(
            current_block,
            provider=sync_provider,
        )
        tracker.backfill_snapshot(current_block)

        # Push updated tick_data to the Rust engine one final time
        v3_updates: list[tuple[str, int, int, int, list[tuple[int, tuple[int, int]]]]] = []
        for pool_addr, pool in tracker._tracked_pools.items():
            if isinstance(pool, UniswapV3Pool) and pool_addr in engine_registry._v3_keys:
                tick_priors = [
                    (idx, (info.liquidity_gross, info.liquidity_net))
                    for idx, info in pool.tick_data.items()
                ]
                v3_updates.append((
                    pool.address,
                    pool.sqrt_price_x96,
                    pool.liquidity,
                    pool.tick,
                    tick_priors,
                ))
        if v3_updates:
            engine_registry.process_block([], v3_updates, [], current_block)
            bot_logger.info(
                f"[backfill] {tracker_name}: pushed {len(v3_updates)} pool updates to Rust engine"
            )

    uniswap_v3_tracker.unload_snapshot()
    sushiswap_v3_tracker.unload_snapshot()
    pancakeswap_v3_tracker.unload_snapshot()

    # ── V4 snapshot backfill ─────────────────────────────────────
    # Same pattern as V3: fetch ModifyLiquidity events between the
    # snapshot block and the current block, apply them to Python V4
    # pools, and push the updated tick_data to the Rust engine.
    try:
        v4_db_snapshot = V4DatabaseSnapshot(chain_id=1, db=bot.db)
        v4_snapshot = UniswapV4LiquiditySnapshot(source=v4_db_snapshot)
    except ValueError:
        # Database has no V4 snapshot data
        v4_snapshot = None

    if v4_snapshot is not None:
        v4_snapshot_block = v4_snapshot.newest_block
        if v4_snapshot_block < current_block:
            bot_logger.info(
                f"[backfill] V4: fetching ModifyLiquidity events "
                f"from block {v4_snapshot_block + 1} to {current_block}"
            )
            v4_snapshot.fetch_new_events(
                current_block,
                provider=sync_provider,
            )

            # Apply pending updates to V4 Python pools
            for pool_info in engine_registry._v4_pool_info.values():
                pool = pool_info.pool
                for liquidity_update in v4_snapshot.pending_updates(
                    pool.pool_manager_address, pool.pool_id
                ):
                    pool.update_liquidity_map(liquidity_update)

            # Push updated tick_data to the Rust engine one final time
            v4_updates: list[
                tuple[str, str, int, int, int, list[tuple[int, tuple[int, int]]]]
            ] = []
            for pool_info in engine_registry._v4_pool_info.values():
                pool = pool_info.pool
                tick_priors = [
                    (idx, (info.liquidity_gross, info.liquidity_net))
                    for idx, info in pool.tick_data.items()
                ]
                v4_updates.append((
                    pool.pool_manager_address,
                    pool.pool_id.to_0x_hex(),
                    pool.sqrt_price_x96,
                    pool.liquidity,
                    pool.tick,
                    tick_priors,
                ))
            if v4_updates:
                engine_registry.process_block([], [], v4_updates, current_block)
                bot_logger.info(
                    f"[backfill] V4: pushed {len(v4_updates)} pool updates to Rust engine"
                )

# ──────────────────────────────────────────────────────────────────
# Priority fee pricing (Slice 3)
# ──────────────────────────────────────────────────────────────────


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
    priority_fee = max(min_priority_fee, min(priority_fee, max_priority_fee))

    return priority_fee


# ──────────────────────────────────────────────────────────────────
# Dispatch pipeline (Slices 1-5)
# ──────────────────────────────────────────────────────────────────
# ──────────────────────────────────────────────────────────────────


@dataclasses.dataclass
class SubmittedTx:
    tx_hash: HexBytes
    nonce: int
    pools: set[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool]
    submission_block: int


async def monitor_pending_transaction(
    tx: SubmittedTx,
    async_w3: AsyncWeb3,
    pending_nonces: set[int],
    pending_pools: set[object],
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
    results: list[tuple[int, int, int, int]],  # (path_id, opt_input, profit, solve_block)
    engine_registry: EngineRegistry,
    async_w3: AsyncWeb3,
    executor_address: str,
    operator_address: str,
    operator_private_key: str,
    base_fee_next: int,
    current_block: int,
    operator_nonce: int,
    pending_nonces: set[int],
    pending_pools: set[object],
    active_tasks: set[asyncio.Task],
    current_block_ref: list[int],
    dry_run: bool,
    block_priority_fees: dict[int, dict[int, int]],
    observe: bool = False,
) -> None:
    """Encode, simulate, and submit profitable results from the Rust engine.

    Pipeline (Slices 1-5):
    1. Sort by engine profit descending (Slice 4 — best-path first)
    2. Fan out parallel simulation with asyncio.gather (Slice 1)
    3. Each simulation: stale check (Slice 2), encode, simulate, gas from sim (Slice 5)
    4. Market-aware priority fee with age decay (Slice 3)
    5. Submit profit-descending with mutual exclusivity (Slice 4)

    Simulation uses a 3-call pattern:
      1. WETH balanceOf(executor) — before
      2. execute_payloads(...)
      3. WETH balanceOf(executor) — after

    Gross profit = balance_after - balance_before (WETH balance increase).
    No WETH prefunding is required — V3's callback is the flash borrow.

    Submitted transactions are tracked via monitor_pending_transaction tasks
    that release nonces and pools on confirmation or expiry.
    """
    # ── Slice 8: Observe mode — log results without simulation ──────────
    if observe:
        for path_id, optimal_input, profit, solve_block in results:
            path_info = engine_registry.paths.get(path_id)
            if path_info is None:
                bot_logger.info(
                    f"[observe] path={path_id} profit={profit // 10**9}gwei "
                    f"solve_block={solve_block} SKIP=no_path_info"
                )
                continue
            profit_eth = profit / 1e18
            hops_desc = " ".join(
                f"{h.pool_type}={h.pool.address} zfo={h.zfo}" for h in path_info.hops
            )
            bot_logger.info(
                f"[observe] path={path_id} {path_info.path_type} "
                f"{hops_desc} "
                f"input={optimal_input} profit={profit_eth:.6f}ETH "
                f"solve_block={solve_block} current_block={current_block}"
            )
        return

    bot_logger.info(
        f"[dispatch] entered with {len(results)} results, dry_run={dry_run}, observe={observe}"
    )

    executor_contract = async_w3.eth.contract(
        address=executor_address,
        abi=EXECUTOR_ABI,
    )

    # Pre-build the balanceOf call for the executor
    weth_balance_calldata = encode_balanceof_calldata(executor_address)
    weth_balance_call = {
        "to": WETH_ADDRESS,
        "data": weth_balance_calldata,
    }

    # ── Slice 4: Sort by engine profit descending — best paths first ──
    results.sort(key=lambda r: r[2], reverse=True)

    # ── Slice 4: Mutual exclusivity — pools already claimed by this dispatch ──
    committed_pools: set[object] = set()

    # ── Slice 1: Parallel simulation ──────────────────────────────────────
    async def simulate_one(
        path_id: int,
        optimal_input: int,
        engine_profit: int,
        solve_block: int,
    ) -> tuple[int, int, int, int, dict, Any] | None:
        """Simulate a single path. Returns (path_id, gross_profit, net_profit, gas_used, tx_params, path_info) or None."""
        path_info = engine_registry.paths.get(path_id)
        if path_info is None:
            return None

        # ── Slice 2: Staleness check ──────────────────────────────────
        # Use the Rust engine's solve_block (not Python pool's update_block)
        # because Python pools are updated before the engine, so
        # pool.update_block >= solve_block is always true.
        if current_block > solve_block + STALENESS_TOLERANCE:
            bot_logger.info(
                f"[sim-fail] path={path_id}: stale (solve={solve_block}, current={current_block})"
            )
            return None

        # \u2500\u2500 Slice 4: Mutual exclusivity with pending + committed pools \u2500\u2500
        path_pools = {h.pool for h in path_info.hops}
        if path_pools & (pending_pools | committed_pools):
            bot_logger.debug(f"[dispatch] skip path={path_id}: pools pending or committed")
            return None

        # Encode payloads using the V3-first flash-borrow pattern
        payloads = encode_payloads(path_info, optimal_input, executor_address)
        if payloads is None:
            bot_logger.info(f"[sim-fail] path={path_id} {path_info.path_type}: encoding failed")
            return None

        # Debug: log payload details for all path types
        dep = ""
        for i, (tgt, cdata, wcb) in enumerate(payloads):
            sel = cdata[:4].hex() if len(cdata) >= 4 else "??"
            dep += f"  P{i}: target={tgt} sel=0x{sel} len={len(cdata)} will_cb={wcb}\n"
        pool_addrs = "→".join(f"{h.pool.address}(zfo={h.zfo})" for h in path_info.hops)
        bot_logger.info(
            f"[sim-debug] path={path_id} {path_info.path_type}: "
            f"{pool_addrs} input={optimal_input}\n{dep}"
        )

        # ── Diagnostic: decode V3 swap params for V2-V3/V3-V2/V3-V3 ──
        # Decode the V3 swap calldata to verify amountSpecified and recipient
        if path_info.path_type in ("V2-V3", "V3-V2", "V3-V3"):
            for i, (tgt, cdata, wcb) in enumerate(payloads):
                if len(cdata) >= 4 and cdata[:4] == V3_SWAP_SELECTOR:
                    try:
                        decoded = eth_abi.abi.decode(
                            types=["address", "bool", "int256", "uint160", "bytes"],
                            data=cdata[4:],
                        )
                        recipient, zfo, amt_spec, sqrt_limit, cb_data = decoded
                        bot_logger.info(
                            f"[sim-diag] path={path_id} P{i} V3.swap: "
                            f"recipient={recipient} zfo={zfo} "
                            f"amountSpecified={amt_spec} "
                            f"sqrtPriceLimitX96={sqrt_limit} "
                            f"cb_data_len={len(cb_data)}"
                        )
                    except Exception as e:
                        bot_logger.info(f"[sim-diag] path={path_id} P{i} V3 decode failed: {e}")
                elif len(cdata) >= 4 and cdata[:4] == V2_SWAP_SELECTOR:
                    try:
                        decoded = eth_abi.abi.decode(
                            types=["uint256", "uint256", "address", "bytes"],
                            data=cdata[4:],
                        )
                        a0_out, a1_out, to, v2_data = decoded
                        bot_logger.info(
                            f"[sim-diag] path={path_id} P{i} V2.swap: "
                            f"amount0Out={a0_out} amount1Out={a1_out} "
                            f"to={to} data_len={len(v2_data)}"
                        )
                    except Exception as e:
                        bot_logger.info(f"[sim-diag] path={path_id} P{i} V2 decode failed: {e}")
                elif len(cdata) >= 4 and cdata[:4] == ERC20_TRANSFER_SELECTOR:
                    try:
                        decoded = eth_abi.abi.decode(
                            types=["address", "uint256"],
                            data=cdata[4:],
                        )
                        to_addr, amount = decoded
                        bot_logger.info(
                            f"[sim-diag] path={path_id} P{i} transfer: "
                            f"token={tgt} to={to_addr} amount={amount}"
                        )
                    except Exception as e:
                        bot_logger.info(
                            f"[sim-diag] path={path_id} P{i} transfer decode failed: {e}"
                        )

        # Build transaction (or encode manually for code injection)
        if INJECT_EXECUTOR_CODE:
            # When code is injected, the address has no on-chain code,
            # so estimateGas in build_transaction would fail.
            # Manually encode the call and build a tx dict with pre-set gas.
            calldata = executor_contract.encode_abi("execute_payloads", [payloads, 0])
            tx_params = {
                "from": Web3.to_checksum_address(EXECUTOR_OWNER),
                "to": Web3.to_checksum_address(executor_address),
                "data": calldata,
                "chainId": 1,
                "type": 2,
                "value": 0,
                "gas": 5_000_000,  # Generous gas for V3 tick-crossing swaps
                "maxFeePerGas": 0,
                "maxPriorityFeePerGas": 0,
                "nonce": 0,
            }
        else:
            try:
                tx_params = await executor_contract.functions.execute_payloads(
                    payloads,
                ).build_transaction({
                    "from": Web3.to_checksum_address(EXECUTOR_OWNER),
                    "chainId": 1,
                    "type": 2,
                    "value": 0,
                })
            except ContractLogicError as cle:
                # Try eth_call to get the specific revert reason
                revert_reason = ""
                try:
                    await async_w3.eth.call({
                        "from": Web3.to_checksum_address(EXECUTOR_OWNER),
                        "to": executor_address,
                        "data": executor_contract.encode_abi("execute_payloads", [payloads, 0]),
                    })
                except ContractLogicError as inner_cle:
                    revert_reason = f" revert={str(inner_cle)[:120]}"

                bot_logger.info(
                    f"[sim-fail] path={path_id} {path_info.path_type}: tx build reverted{revert_reason}"
                )
                return None

            # Use generous gas for simulation (Slice 5: will override after sim)
            tx_params["gas"] = int(1.5 * tx_params["gas"])

        # Simulate with 3-call pattern
        state_overrides = build_simulation_state_overrides(
            executor_address=executor_address,
            executor_owner=EXECUTOR_OWNER,
            inject_code=INJECT_EXECUTOR_CODE,
            injected_address=INJECTED_EXECUTOR_ADDRESS if INJECT_EXECUTOR_CODE else None,
        )
        try:
            sim = await async_w3.eth.simulate_v1(
                payload={
                    "blockStateCalls": [
                        {
                            "calls": [
                                weth_balance_call,  # [0] WETH balance before
                                tx_params,  # [1] execute_payloads
                                weth_balance_call,  # [2] WETH balance after
                            ],
                            "stateOverrides": state_overrides,
                        }
                    ],
                },
                block_identifier="pending",
            )
        except Web3Exception:
            bot_logger.info(
                f"[sim-fail] path={path_id} {path_info.path_type}: simulation RPC failed (eth_simulateV1 error)"
            )
            return None

        calls = sim[0]["calls"]

        # Check all three calls succeeded — log which call failed + revert data
        failed_call = None
        for i, c in enumerate(calls):
            if c.get("status", 0) != 1:
                failed_call = c
                revert_data = c.get("returnData", b"")
                revert_hex = (
                    revert_data.hex() if isinstance(revert_data, bytes) else str(revert_data)[:128]
                )
                # Decode common revert patterns
                revert_reason = ""
                if len(revert_hex) >= 8:
                    selector = revert_hex[:8]
                    if selector == "4e487b71":  # Panic
                        revert_reason = (
                            f" PANIC(0x{revert_hex[8:]})" if len(revert_hex) > 8 else " PANIC"
                        )
                    elif selector == "08c379a0":  # Error string
                        try:
                            msg_len = int(revert_hex[8 + 64 : 8 + 128], 16) * 2
                            msg_bytes = bytes.fromhex(revert_hex[128 : 128 + msg_len])
                            revert_reason = f" {msg_bytes.decode('utf-8', errors='replace')}"
                        except Exception:
                            pass
                    elif selector == "4b9dfc58":  # !OWNER (our contract)
                        revert_reason = " !OWNER"
                    elif revert_hex.startswith(
                        "00000000000000000000000000000000000000000000000000000000"
                    ):
                        # Numeric revert
                        revert_reason = f" num=0x{revert_hex[24:]}"
                    else:
                        revert_reason = f" sel=0x{selector}"
                bot_logger.info(
                    f"[sim-fail] path={path_id} {path_info.path_type}: "
                    f"call[{i}] failed (gasUsed={c.get('gasUsed', 0)}) "
                    f"revert=0x{revert_hex[:200]}{revert_reason}"
                )

                # ── Diagnostic: debug_traceCall for IIA reverts ───────
                # When V2-V3/V3-V2/V3-V3 paths revert with IIA, run
                # debug_traceCall to identify the exact internal call that
                # triggered the revert.
                if revert_reason.strip() in ("IIA",) and path_info.path_type in (
                    "V2-V3",
                    "V3-V2",
                    "V3-V3",
                ):
                    try:
                        trace_result = await async_w3.provider.make_request(
                            "debug_traceCall",
                            [
                                {
                                    "from": Web3.to_checksum_address(EXECUTOR_OWNER),
                                    "to": Web3.to_checksum_address(executor_address),
                                    "data": tx_params.get("data", tx_params.get("input", "")),
                                    "value": "0x0",
                                    "gas": "0x4c4b40",  # 5M
                                },
                                "pending",
                                {"tracer": "callTracer", "tracerConfig": {"onlyTopCall": False}},
                            ],
                        )

                        # Walk the nested call trace to find IIA
                        def _find_iia_in_trace(trace: dict, depth: int = 0) -> None:
                            sel = ""
                            inp = trace.get("input", "")
                            out = trace.get("output", "")
                            typ = trace.get("type", "")
                            tgt = trace.get("to", "")
                            err = trace.get("error", "")
                            gas_used_t = trace.get("gasUsed", "0x0")
                            if isinstance(inp, str) and len(inp) >= 10:
                                sel = inp[:10]
                            status = "OK" if trace.get("status", True) else "REVERT"
                            if err or not trace.get("status", True):
                                bot_logger.info(
                                    f"  [trace] {'  ' * depth}{typ} {tgt} sel={sel} "
                                    f"status={status} err={err[:80]} "
                                    f"out={out[:40]} gas={gas_used_t}"
                                )
                            for sub in trace.get("calls", []):
                                _find_iia_in_trace(sub, depth + 1)

                        if isinstance(trace_result, dict):
                            _find_iia_in_trace(trace_result)
                        elif isinstance(trace_result, list) and trace_result:
                            _find_iia_in_trace(trace_result[0])
                    except Exception as trace_exc:
                        bot_logger.info(f"  [trace] failed: {trace_exc}")

                return None

        # Extract gross profit from WETH balance change
        try:
            balance_before_raw = calls[0]["returnData"]
            balance_after_raw = calls[2]["returnData"]
            balance_before = int.from_bytes(balance_before_raw, byteorder="big")
            balance_after = int.from_bytes(balance_after_raw, byteorder="big")
        except (IndexError, ValueError):
            bot_logger.info(
                f"[sim-fail] path={path_id} {path_info.path_type}: balance decode failed"
            )
            return None

        gross_profit = balance_after - balance_before
        if gross_profit <= 0:
            bot_logger.info(
                f"[sim-fail] path={path_id} {path_info.path_type}: no profit (gross={gross_profit})"
            )
            return None

        # ── Slice 5: Gas estimation from simulation ──────────────────
        # Use the simulation's actual gasUsed with a 10% safety margin
        # instead of the 1.5× heuristic that wastes ~50K gas per tx.
        gas_used = calls[1]["gasUsed"]
        tx_params["gas"] = int(gas_used * 1.1)

        # ── Slice 3: Market-aware priority fee with age decay ────────────
        priority_fee = _compute_priority_fee(
            gross_profit=gross_profit,
            gas_used=gas_used,
            base_fee_next=base_fee_next,
            solve_block=solve_block,
            current_block=current_block,
            block_priority_fees=block_priority_fees,
        )

        l2_fee = gas_used * (base_fee_next + priority_fee)
        net_profit = gross_profit - l2_fee

        # Return all gross-profitable results — the caller separates
        # gas-profitable from gas-unprofitable but onchain-valid.
        return (path_id, gross_profit, net_profit, gas_used, tx_params, path_info)

    # ── Fan out parallel simulation (Slice 1) ──────────────────────────
    candidates = results[:MAX_SIMULATE_CONCURRENT]
    # Log candidate summary for observability
    cand_types = {}
    for pid, inp, pft, sb in candidates:
        pi = engine_registry.paths.get(pid)
        pt = pi.path_type if pi else "?"
        cand_types[pt] = cand_types.get(pt, 0) + 1
    cand_types_str = " ".join(f"{k}={v}" for k, v in sorted(cand_types.items()))
    bot_logger.info(
        f"[dispatch] simulating {len(candidates)}/{len(results)} candidates: {cand_types_str}"
    )
    sim_tasks = [simulate_one(pid, inp, pft, sb) for pid, inp, pft, sb in candidates]
    sim_results = await asyncio.gather(*sim_tasks, return_exceptions=True)

    # ── Categorize simulation results ────────────────────────────────
    # Separate into gas-profitable (net >= MIN_PROFIT_NET) and
    # onchain-valid but gas-unprofitable (gross > 0, net below threshold).
    gas_profitable: list[tuple[int, int, int, int, dict, Any]] = []
    gas_unprofitable: list[tuple[int, int, int, int, dict, Any]] = []
    for result in sim_results:
        if isinstance(result, Exception):
            bot_logger.info(f"[sim-fail] simulation exception: {result}")
            continue
        if result is None:
            continue
        path_id, gross_profit, net_profit, gas_used, tx_params, path_info = result
        if net_profit >= MIN_PROFIT_NET:
            gas_profitable.append(result)
        else:
            gas_unprofitable.append(result)

    # Sort both categories by net profit descending
    gas_profitable.sort(key=lambda r: r[2], reverse=True)
    gas_unprofitable.sort(key=lambda r: r[2], reverse=True)

    # Log gas-unprofitable but onchain-valid results for observability
    for path_id, gross_profit, net_profit, gas_used, tx_params, path_info in gas_unprofitable:
        bot_logger.info(
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
        path_pools = {h.pool for h in path_info.hops}
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


# ──────────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────────


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--observe", action="store_true", help="Log engine results without simulation (Slice 8)"
    )
    args = parser.parse_args()
    dry_run = args.dry_run or DRY_RUN
    observe = args.observe  # Log every engine result without simulation

    if not dry_run and not observe:
        bot_logger.info("\n*** DRY RUN DISABLED — BOT IS LIVE! ***\n")
    if observe:
        bot_logger.info("*** OBSERVE MODE — logging engine results only, no simulation ***")

    config: dict[str, Any] = dotenv.dotenv_values("examples/mainnet.env")

    # Operator: must come from env
    operator_address = get_checksum_address(config.get("OPERATOR_ADDRESS", ""))
    operator_private_key = config.get("OPERATOR_PRIVATE_KEY", "")
    if not dry_run and not observe and (not operator_address or not operator_private_key):
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

    latest_block = await async_w3.eth.get_block("latest")
    current_block = latest_block["number"]
    base_fee_next = next_base_fee(
        parent_base_fee=latest_block["baseFeePerGas"],
        parent_gas_used=latest_block["gasUsed"],
        parent_gas_limit=latest_block["gasLimit"],
    )
    operator_nonce = await async_w3.eth.get_transaction_count(operator_address)

    await build_paths(bot, engine_registry, current_block)

    # ── Runtime state ────────────────────────────────────────────
    chain_id = 1
    pending_nonces: set[int] = set()
    pending_pools: set[object] = set()
    active_tasks: set[asyncio.Task] = set()
    current_block_ref: list[int] = [current_block]  # mutable for monitor tasks
    block_times: deque[tuple[int, int]] = deque(maxlen=60)
    block_priority_fees: dict[int, dict[int, int]] = {}
    dispatch_lock = asyncio.Lock()  # Prevents concurrent dispatches (Slice 0.5)
    last_engine_log: frozenset[tuple[int, int]] = frozenset()  # Dedup engine result logs

    # ── Start the Rust pump ──────────────────────────────────────
    # The pump subscribes to block headers via WS, fetches all relevant
    # logs (V2 Sync, V3 Swap/Mint/Burn, V4 Swap/ModifyLiquidity) in
    # one eth_getLogs call per block, decodes events, and updates the
    # Rust engine autonomously. Python reads results via latest_results().
    engine_registry.engine.start(node_ws)
    bot_logger.info("Rust pump started — engine processes events autonomously")

    async def try_dispatch() -> None:
        """Dispatch profitable results from the Rust engine.

        The Rust pump processes events autonomously. This function reads
        the latest results and dispatches profitable ones for simulation
        and submission.
        """
        nonlocal last_engine_log

        # Non-blocking: if a dispatch is already running, skip this one
        if dispatch_lock.locked():
            return

        async with dispatch_lock:
            results = engine_registry.profitable_results()
            if not results:
                return

            # Log top results only if they changed since last dispatch
            # (suppressed in observe mode — [observe] logs are more detailed)
            top_sig = frozenset((pid, pft) for pid, _, pft, _ in results[:5])
            if top_sig != last_engine_log and not observe:
                last_engine_log = top_sig
                for pid, inp, pft, sb in results[:5]:
                    pi = engine_registry.paths.get(pid)
                    desc = "↔".join(str(h.pool) for h in pi.hops) if pi else f"path={pid}"
                    bot_logger.info(
                        f"[engine] {desc} input={inp} profit={pft // 10**9}gwei solve_block={sb}"
                    )

            await dispatch_profitable_results(
                results=results,
                engine_registry=engine_registry,
                async_w3=async_w3,
                executor_address=executor_address,
                operator_address=operator_address,
                operator_private_key=operator_private_key,
                base_fee_next=base_fee_next,
                current_block=current_block_ref[0],
                operator_nonce=operator_nonce,
                pending_nonces=pending_nonces,
                pending_pools=pending_pools,
                active_tasks=active_tasks,
                current_block_ref=current_block_ref,
                dry_run=dry_run,
                block_priority_fees=block_priority_fees,
                observe=observe,
            )

    async def on_block(ctx: Any) -> None:
        nonlocal current_block, base_fee_next, operator_nonce

        block = ctx.result
        block_number = block["number"]
        block_timestamp = block["timestamp"]

        # Update fee data
        base_fee = block.get("baseFeePerGas", 0)
        base_fee_next = next_base_fee(
            parent_base_fee=base_fee,
            parent_gas_used=block.get("gasUsed", 0),
            parent_gas_limit=block.get("gasLimit", 0),
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
                block_priority_fees[block_number] = {
                    p: f for p, f in zip(FEE_PERCENTILES, reward[-1], strict=True)
                }
                if len(block_priority_fees) > FEE_HISTORY_WINDOW:
                    block_priority_fees.pop(min(block_priority_fees))
        except Web3Exception:
            pass

        block_times.append((block_number, block_timestamp))
        if len(block_times) >= 2:
            oldest_bn, oldest_ts = block_times[0]
            if block_number != oldest_bn:
                latency = time.time() - block_timestamp
                bot_logger.info(
                    f"[{block_number}][+{latency:.1f}s]"
                    f"[{base_fee / 10**9:.5f}/{base_fee_next / 10**9:.5f}]"
                )

        # New block arrival: dispatch immediately with any accumulated updates
        current_block_ref[0] = block_number
        current_block = block_number
        await try_dispatch()

    # ── Subscribe with WS reconnection ────────────────────────────
    # Python subscribes to newHeads only — the Rust pump handles all
    # event decoding and state updates autonomously.
    reconnect_delay = RECONNECT_BASE_DELAY

    while True:
        try:
            async with AsyncWeb3(web3.WebSocketProvider(node_ws)) as ws_w3:
                ws_w3.middleware_onion.clear()

                await ws_w3.subscription_manager.subscribe(NewHeadsSubscription(handler=on_block))
                bot_logger.info("Subscribed to newHeads — Rust pump handles events")
                await ws_w3.subscription_manager.handle_subscriptions()
                reconnect_delay = RECONNECT_BASE_DELAY  # Reset on clean exit
        except Exception as exc:
            bot_logger.error(f"WS connection lost: {exc}")
            bot_logger.info(f"Reconnecting in {reconnect_delay:.1f}s...")
            await asyncio.sleep(reconnect_delay)
            reconnect_delay = min(reconnect_delay * 2, RECONNECT_MAX_DELAY)
            # The Rust pump handles its own reconnection.
            # Python only needs to re-subscribe to newHeads.


if __name__ == "__main__":
    start = time.perf_counter()
    asyncio.run(main())
    bot_logger.info(f"Completed in {time.perf_counter() - start:.2f}s")
