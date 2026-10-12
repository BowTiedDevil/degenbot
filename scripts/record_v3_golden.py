#!/usr/bin/env python3
"""Record a full-range V3 liquidity golden for one pool at one block.

The walk: build the pool (the sparse snapshot), drive each direction to
range exhaustion with 1e38-scale inputs — the down-range capacity toward
a zero price is effectively unbounded, so realistic amounts never reach
the tick bounds — retrying each directional walk until the frontier
converges. A failed word fetch aborts the simulation without marking the
word known, so every retry advances it; the map persists across attempts
on the pool object. Set DEGENBOT_TICK_FETCH_MIN_INTERVAL_MS (the rate
governor) to keep the retry burst under the RPC plan limit.

The boundary check refuses to write a partial map: both protocol bound
words (MIN_WORD/MAX_WORD) must be present, mirroring the anti-truncation
rule the recorded-pool tests assert.

Usage:
    DEGENBOT_TICK_FETCH_MIN_INTERVAL_MS=20 uv run python scripts/record_v3_golden.py \
        --pool 0xCBCdF9626bC03E24f779434178A73a0B4bad62eD \
        --block 17600000 --tick-spacing 60 \
        --out tests/golden/data/uniswap/v3/wbtc_weth/17600000.json.gz

Requires the live archive RPC (TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI).
"""

from __future__ import annotations

import argparse
import time
from pathlib import Path
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import IncompleteSwap, LiquidityPoolError, MissingLiquidityData
from degenbot.fork import AnvilFork, ForkLaunchConfig
from tests.conftest import TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.golden.recorded_pool import record_pool
from tests.helpers.bot_factory import make_bot_with_provider

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.uniswap.v3_liquidity_pool import UniswapV3LiquidityPool

PROTOCOL_TICK_BOUND = 887_272


def parse_args() -> argparse.Namespace:
    """Parse the pool, block, spacing, output, and retry-budget arguments.

    Returns:
        The parsed argument namespace.

    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pool", required=True, help="Checksummed pool address")
    parser.add_argument("--block", required=True, type=int, help="Archive block to fork")
    parser.add_argument("--tick-spacing", required=True, type=int)
    parser.add_argument("--out", required=True, type=Path, help="Golden path (.json.gz)")
    parser.add_argument("--max-attempts", type=int, default=40)
    return parser.parse_args()


def walk_to_exhaustion(
    lp: UniswapV3LiquidityPool,
    token_in: Erc20Token,
    *,
    max_attempts: int,
) -> None:
    """Drive one directional walk to range exhaustion, retrying fetch failures.

    Raises:
        SystemExit: If the frontier does not converge within the budget.

    """
    for attempt in range(max_attempts):
        try:
            lp.calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=10**38,
            )
        except IncompleteSwap:
            print(f"range exhausted for {token_in.symbol} after {attempt} attempts", flush=True)
            return
        except (MissingLiquidityData, LiquidityPoolError) as e:
            print(f"attempt {attempt}: {type(e).__name__}: {e}", flush=True)
            time.sleep(1)
    msg = f"walk did not converge for {token_in.symbol} after {max_attempts} attempts"
    print(msg, flush=True)
    raise SystemExit(1)


def main() -> None:
    """Record the full-range golden: build, walk both directions, verify, write."""
    args = parse_args()
    pool = get_checksum_address(args.pool)
    min_word = -PROTOCOL_TICK_BOUND // (args.tick_spacing * 256)
    max_word = PROTOCOL_TICK_BOUND // (args.tick_spacing * 256)

    fork = AnvilFork(
        fork_url=TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        fork_block=args.block,
        launch=ForkLaunchConfig(storage_caching=False, anvil_opts=["--accounts=0"]),
    )
    try:
        bot = make_bot_with_provider(fork.provider)
        lp = bot.build_pool(pool)
        print(f"sparse build: {len(lp.tick_data)} ticks", flush=True)

        for token_in in (lp.token0, lp.token1):
            walk_to_exhaustion(lp, token_in, max_attempts=args.max_attempts)

        words = lp.tick_bitmap
        assert min_word in words, (
            f"down walk did not reach MIN_TICK word {min_word}: min_tick={min(lp.tick_data)}"
        )
        assert max_word in words, (
            f"up walk did not reach MAX_TICK word {max_word}: max_tick={max(lp.tick_data)}"
        )
        print(f"boundary check OK: ticks={len(lp.tick_data)} words={len(words)}", flush=True)

        args.out.parent.mkdir(parents=True, exist_ok=True)
        state = record_pool(lp, args.out, block=args.block)
        print(
            f"WROTE {args.out} {args.out.stat().st_size} bytes; "
            f"{len(state['tick_data'])} ticks; words: {len(state['tick_bitmap'])}",
            flush=True,
        )
    finally:
        fork.close()


if __name__ == "__main__":
    main()
