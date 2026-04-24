"""
Verify recorded V3 tick data against TickLens contract.

Usage:
    python scripts/verify_v3_tick_data.py --data tests/fixtures/chain_data/1/blocks_24945920_24945920.json --rpc-url https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY

The script compares recorded tick data against the TickLens contract
which provides an authoritative source for V3 pool tick information.
"""

import argparse
import json
import sys
from pathlib import Path

from web3 import Web3

TICKLENS_ADDRESS = "0x5F20f123a7390b27c6885Ace2d4D1D7018D4Ce59"

# Minimal ABI for TickLens getPopulatedTicksInWord
TICKLENS_ABI = [
    {
        "inputs": [
            {"internalType": "address", "name": "pool", "type": "address"},
            {"internalType": "int16", "name": "tickBitmapIndex", "type": "int16"},
        ],
        "name": "getPopulatedTicksInWord",
        "outputs": [
            {
                "components": [
                    {"internalType": "int24", "name": "tick", "type": "int24"},
                    {"internalType": "int128", "name": "liquidityNet", "type": "int128"},
                    {"internalType": "uint128", "name": "liquidityGross", "type": "uint128"},
                ],
                "internalType": "struct ITickLens.PopulatedTick[]",
                "name": "populatedTicks",
                "type": "tuple[]",
            }
        ],
        "stateMutability": "view",
        "type": "function",
    }
]

# Uniswap V3 Pool ABI for tickBitmap and ticks
POOL_ABI = [
    {
        "inputs": [{"internalType": "int16", "name": "", "type": "int16"}],
        "name": "tickBitmap",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [{"internalType": "int24", "name": "", "type": "int24"}],
        "name": "ticks",
        "outputs": [
            {"internalType": "uint128", "name": "liquidityGross", "type": "uint128"},
            {"internalType": "int128", "name": "liquidityNet", "type": "int128"},
            {"internalType": "uint256", "name": "feeGrowthOutside0X128", "type": "uint256"},
            {"internalType": "uint256", "name": "feeGrowthOutside1X128", "type": "uint256"},
            {"internalType": "int56", "name": "tickCumulativeOutside", "type": "int56"},
            {
                "internalType": "uint160",
                "name": "secondsPerLiquidityOutsideX128",
                "type": "uint160",
            },
            {"internalType": "uint32", "name": "secondsOutside", "type": "uint32"},
            {"internalType": "bool", "name": "initialized", "type": "bool"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
]


def verify_v3_tick_data(data_file: Path, rpc_url: str) -> bool:
    """Verify recorded V3 tick data against on-chain TickLens."""
    print(f"Loading recorded data from {data_file}...")
    with Path(data_file).open(encoding="utf-8") as f:
        data = json.load(f)

    chain_id = data.get("chain_id", 1)
    print(f"Chain ID: {chain_id}")

    print(f"\nConnecting to {rpc_url}...")
    w3 = Web3(Web3.HTTPProvider(rpc_url))
    if not w3.is_connected():
        print("Failed to connect to RPC", file=sys.stderr)
        return False

    actual_chain_id = w3.eth.chain_id
    if actual_chain_id != chain_id:
        print(
            f"Warning: Data file specifies chain {chain_id} but RPC reports {actual_chain_id}",
            file=sys.stderr,
        )

    ticklens = w3.eth.contract(address=TICKLENS_ADDRESS, abi=TICKLENS_ABI)

    all_passed = True

    # Verify V3 liquidity data for each block
    for block_number_str, block_data in data.get("blocks", {}).items():
        block_number = int(block_number_str)
        v3_liquidity = block_data.get("v3_liquidity", {})

        if not v3_liquidity:
            print(f"\nBlock {block_number}: No V3 liquidity data to verify")
            continue

        print(f"\nVerifying block {block_number}...")

        for pool_address, pool_data in v3_liquidity.items():
            print(f"  Pool {pool_address}:")

            tick_spacing = pool_data.get("tick_spacing")
            recorded_bitmap = pool_data.get("tick_bitmap", {})
            recorded_ticks = pool_data.get("tick_data", {})

            print(f"    Tick spacing: {tick_spacing}")
            print(f"    Recorded bitmap words: {len(recorded_bitmap)}")
            print(f"    Recorded ticks: {len(recorded_ticks)}")

            # Verify each recorded word
            for word_pos_str, word_data in recorded_bitmap.items():
                word_pos = int(word_pos_str)
                recorded_map = int(word_data.get("bitmap", "0"))

                # Get tick bitmap from chain via direct pool call
                try:
                    pool_contract = w3.eth.contract(address=pool_address, abi=POOL_ABI)
                    chain_bitmap = pool_contract.functions.tickBitmap(word_pos).call(
                        block_identifier=block_number
                    )
                except Exception as e:
                    print(f"    ERROR: Failed to fetch tickBitmap({word_pos}): {e}")
                    all_passed = False
                    continue

                if chain_bitmap != recorded_map:
                    print(
                        f"    MISMATCH: tickBitmap[{word_pos}] "
                        f"recorded={recorded_map}, chain={chain_bitmap}"
                    )
                    all_passed = False

            # Verify each recorded tick
            for tick_str, tick_info in recorded_ticks.items():
                tick = int(tick_str)
                recorded_gross = int(tick_info.get("liquidity_gross", "0"))
                recorded_net = int(tick_info.get("liquidity_net", "0"))

                # Get tick data from chain
                try:
                    pool_contract = w3.eth.contract(address=pool_address, abi=POOL_ABI)
                    chain_tick = pool_contract.functions.ticks(tick).call(
                        block_identifier=block_number
                    )
                    chain_gross = chain_tick[0]
                    chain_net = chain_tick[1]
                except Exception as e:
                    print(f"    ERROR: Failed to fetch ticks({tick}): {e}")
                    all_passed = False
                    continue

                if chain_gross != recorded_gross or chain_net != recorded_net:
                    print(
                        f"    MISMATCH: ticks[{tick}] "
                        f"recorded=({recorded_gross}, {recorded_net}), "
                        f"chain=({chain_gross}, {chain_net})"
                    )
                    all_passed = False

            # Cross-check: verify TickLens returns same data
            print("    Cross-checking with TickLens...")
            verified_ticks = 0
            for word_pos_str in recorded_bitmap:
                word_pos = int(word_pos_str)
                try:
                    lens_ticks = ticklens.functions.getPopulatedTicksInWord(
                        pool_address, word_pos
                    ).call(block_identifier=block_number)

                    for lens_tick in lens_ticks:
                        tick = lens_tick[0]
                        lens_net = lens_tick[1]
                        lens_gross = lens_tick[2]

                        tick_str = str(tick)
                        if tick_str in recorded_ticks:
                            rec = recorded_ticks[tick_str]
                            if (
                                int(rec["liquidity_net"]) != lens_net
                                or int(rec["liquidity_gross"]) != lens_gross
                            ):
                                print(
                                    f"      MISMATCH: TickLens tick {tick} "
                                    f"recorded=({rec['liquidity_gross']}, {rec['liquidity_net']}), "
                                    f"lens=({lens_gross}, {lens_net})"
                                )
                                all_passed = False
                            else:
                                verified_ticks += 1
                except Exception as e:
                    print(f"    ERROR: TickLens call failed for word {word_pos}: {e}")

            print(f"    Verified {verified_ticks} ticks against TickLens")

    if all_passed:
        print("\n✓ All V3 tick data verified successfully!")
    else:
        print("\n✗ Some verifications failed. See above for details.")

    return all_passed


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Verify recorded V3 tick data against TickLens contract"
    )
    parser.add_argument(
        "--data",
        type=Path,
        required=True,
        help="Path to recorded chain data JSON file",
    )
    parser.add_argument(
        "--rpc-url",
        type=str,
        required=True,
        help="RPC URL for verification (must support historical queries)",
    )

    args = parser.parse_args()

    if not args.data.exists():
        print(f"Error: Data file not found: {args.data}", file=sys.stderr)
        sys.exit(1)

    success = verify_v3_tick_data(args.data, args.rpc_url)
    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()
