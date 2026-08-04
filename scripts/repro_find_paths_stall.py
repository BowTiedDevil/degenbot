"""Minimal reproduction of the find_paths non-termination observed in bot_run.log.

The live bot (`eth_backrun_v2_v3_v4_rust.py`) drives `find_paths_async` from
WETH with max_depth=3 and `allowed_intermediate_tokens=ETH_MAINNET_ALLOWED_TOKENS`
(16 hub tokens) over the ~600 K-pool mainnet DB subgraph. Across a 12.5 h run
the DFS had yielded 15.27 M paths and was still climbing — `build_paths` never
reached its terminal `release_all_v3_v4_quarantined()`, so every Tracked pool
stayed Quarantined and its live Swaps returned None (no dirty).

This harness reproduces that non-termination cheaply: it runs the SAME `find_paths`
search for a bounded wall-clock window and reports graph scale + yield rate, so a
full enumeration's practicality (or lack thereof) is quantified.

Usage:  uv run python scripts/repro_find_paths_stall.py [seconds]
"""
import argparse
import os
import time
from pathlib import Path

from eth_typing import ChainId

from degenbot.config import DatabaseSettings
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.pathfinding import find_paths  # sync generator (simplest harness)
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS

# Inlined from examples/eth_backrun_v2_v3_v4_rust.py (16 hub intermediate tokens).
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

WETH = WRAPPED_NATIVE_TOKENS[ChainId.ETH]


def _pool_types_from_filter() -> list[type]:
    """No permutation filter -> include ALL V2/V3/V4 concrete table types."""
    from degenbot.database.models.pools import (  # local import to keep leaf light
        UniswapV2PoolTable,
        UniswapV3PoolTable,
        UniswapV4PoolTable,
    )
    return [UniswapV2PoolTable, UniswapV3PoolTable, UniswapV4PoolTable]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("seconds", nargs="?", type=float, default=60.0,
                        help="wall-clock budget for the sampling window")
    args = parser.parse_args()

    db_path = Path(os.path.expanduser("~/.config/degenbot/degenbot.db"))
    scoped = get_scoped_sqlite_session(db_path)
    session_mgr = DatabaseSessionManager(scoped)

    print(f"start/end : WETH (incl. native), max_depth=3, all V2/V3/V4")
    print(f"hub tokens: {len(ETH_MAINNET_ALLOWED_TOKENS)}")

    start = time.perf_counter()
    yielded = 0
    deadline = start + args.seconds
    last = start
    last_count = 0

    it = find_paths(
        chain_id=ChainId.ETH,
        start_tokens=[WETH, NATIVE_CURRENCY_ADDRESS],
        end_tokens=[WETH, NATIVE_CURRENCY_ADDRESS],
        max_depth=3,
        pool_types=_pool_types_from_filter(),
        db=session_mgr,
        allowed_intermediate_tokens=ETH_MAINNET_ALLOWED_TOKENS,
    )

    try:
        for _ in it:
            yielded += 1
            now = time.perf_counter()
            if now - last >= 30.0:
                seg = yielded - last_count
                print(f"  +{now - start:6.1f}s  cumulative={yielded:>10,}  "
                      f"segment={seg:>9,} ({seg / (now - last):,.0f}/s)")
                last = now
                last_count = yielded
            if now >= deadline:
                break
        elapsed = time.perf_counter() - start
    except KeyboardInterrupt:
        elapsed = time.perf_counter() - start
        print("interrupted by user")

    rate = yielded / elapsed if elapsed else 0.0
    print(f"\n--- window result ---")
    print(f"window      : {elapsed:.1f} s")
    print(f"paths yield : {yielded}")
    print(f"yield rate  : {rate:,.1f} paths/s")

    if rate > 0:
        print(f"\n--- extrapolation (informational) ---")
        print(f"12.5 h live run reached ~15.27 M paths, still climbing (never completed)")
        print(f"at {rate:,.0f}/s:  50 M paths -> {50e6 / rate / 3600:.1f} h")
        print(f"                 100 M paths -> {100e6 / rate / 3600:.1f} h")


if __name__ == "__main__":
    main()
