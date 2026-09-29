#!/usr/bin/env python3
"""Record a TRIPOOL construction cassette for the Curve A-ramp regression.

Forks Ethereum mainnet at ``--block`` via the archive URI (``tests.env`` /
``ETHEREUM_ARCHIVE_NODE_HTTP_URI``), builds the pool through the production
``Bot.build_pool`` path, and serializes the pool's constructor inputs into the
cassette schema loaded by ``tests.curve.test_curve_onchain_parity``
(``_build_curve_io_free``). The recorded cassette lets
``tests/curve/test_curve_stableswap_pool.py::test_a_ramping`` replay offline.

The cassette is only re-recorded when the pinned regression block changes:

    uv run --no-sync python scripts/record_curve_tripool_cassette.py --block 14900000

writes ``tests/fixtures/chain_data/1/curve_tripool_block_<block>.json``.

Requires a reachable archive node serving ``--block`` (record window) and the
anvil binary.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from degenbot.fork import AnvilFork
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.bot_factory import make_bot_with_provider

TRIPOOL_ADDRESS = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"


def _token_dict(tok) -> dict:
    return {
        "address": tok.address,
        "name": tok.name,
        "symbol": tok.symbol,
        "decimals": tok.decimals,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--block", type=int, required=True, help="Pin the fork to this block")
    args = parser.parse_args()
    block = args.block
    out = Path(f"tests/fixtures/chain_data/1/curve_tripool_block_{block}.json")

    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        fork_block=block,
    )
    try:
        bot = make_bot_with_provider(fork.provider)
        lp = bot.build_pool(TRIPOOL_ADDRESS)
        assert lp.update_block == block
        dp = lp._data_provider
        strategies = lp._strategies
        cassette = {
            "chain_id": 1,
            "block": block,
            "address": lp.address,
            "name": "Curve 3pool (DAI/USDC/USDT)",
            "balances": list(lp.balances),
            "a_coefficient": lp._a_coefficient,
            "fee": lp._fee,
            "admin_fee": lp._admin_fee,
            "use_lending": list(lp._use_lending),
            "virtual_price": dp.virtual_price(block),
            "block_timestamp": dp.block_timestamp(block),
            "initial_a": lp._initial_a_coefficient,
            "future_a": lp._future_a_coefficient,
            "initial_a_time": lp._initial_a_coefficient_time,
            "future_a_time": lp._future_a_coefficient_time,
            "create_timestamp": lp._create_timestamp,
            "strategies": {
                "d_variant": strategies.d_variant.value,
                "y_variant": strategies.y_variant.value,
                "yd_variant": strategies.yd_variant.value,
                "swap_style": strategies.swap_style.value,
                "metapool_rate_style": strategies.metapool_rate_style.value,
                "metapool_underlying_style": strategies.metapool_underlying_style.value,
                "lending_rate_style": strategies.lending_rate_style.value,
            },
            "tokens": [_token_dict(t) for t in lp._tokens],
            "lp_token": _token_dict(lp._lp_token),
            "lp_token_total_supply": dp.token_total_supply(lp._lp_token.address, block),
        }
    finally:
        fork.close()

    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(cassette, indent=2, sort_keys=True) + "\n")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
