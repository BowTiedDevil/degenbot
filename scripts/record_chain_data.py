"""
Record on-chain data to JSON files for offline testing.

This script fetches chain data (token metadata, pool states) from an RPC endpoint
and saves it to JSON files for use in tests without requiring a live RPC connection.

Usage:
    python scripts/record_chain_data.py --config record_config.json --output tests/fixtures/chain_data

Example config file (record_config.json):
{
    "chain_id": 1,
    "blocks": [24945700, 24945800],
    "rpc_url": "http://localhost:8545",
    "tokens": [
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    ],
    "pools": {
        "v2": [
            {
                "address": "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940",
                "name": "UniswapV2",
                "token0": "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
                "token1": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                "factory": "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
                "init_hash": "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
            }
        ],
        "v3": [
            {
                "address": "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD",
                "name": "UniswapV3",
                "token0": "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
                "token1": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                "factory": "0x1F98431c8aD98523631AE4a59f267346ea31F984",
                "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
            }
        ],
        "v4": [
            {
                "pool_id": "0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27",
                "name": "UniswapV4",
                "pool_manager": "0x000000000004444c5dc75cB358380D2e3dE08A90",
                "state_view": "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227",
                "token0": "0x0000000000000000000000000000000000000000",
                "token1": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                "fee": 500,
                "tick_spacing": 10
            }
        ],
        "curve": [
            {
                "address": "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
                "name": "Curve3Pool"
            }
        ]
    }
}
"""

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path

from eth_abi.abi import decode
from web3 import Web3

from degenbot.functions import encode_function_calldata
from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK


@dataclass(frozen=True, slots=True)
class RecordedCall:
    """Container for a recorded eth_call."""

    to: str
    data: str
    result: str


@dataclass(frozen=True, slots=True)
class RecordedCode:
    """Container for recorded contract code."""

    address: str
    code: str


def fetch_token_calls(w3: Web3, token_address: str, block_number: int) -> list[RecordedCall]:
    """Fetch and record all calls needed for a token at a specific block."""

    checksum_address = w3.to_checksum_address(token_address)
    calls: list[RecordedCall] = []

    name_calldata = "0x" + encode_function_calldata("name()", None).hex()
    try:
        result = w3.eth.call(
            {"to": checksum_address, "data": name_calldata}, block_identifier=block_number
        )
        calls.append(RecordedCall(token_address, name_calldata, result.hex()))
    except Exception:
        pass

    symbol_calldata = "0x" + encode_function_calldata("symbol()", None).hex()
    try:
        result = w3.eth.call(
            {"to": checksum_address, "data": symbol_calldata}, block_identifier=block_number
        )
        calls.append(RecordedCall(token_address, symbol_calldata, result.hex()))
    except Exception:
        pass

    decimals_calldata = "0x" + encode_function_calldata("decimals()", None).hex()
    try:
        result = w3.eth.call(
            {"to": checksum_address, "data": decimals_calldata}, block_identifier=block_number
        )
        calls.append(RecordedCall(token_address, decimals_calldata, result.hex()))
    except Exception:
        pass

    total_supply_calldata = "0x" + encode_function_calldata("totalSupply()", None).hex()
    try:
        result = w3.eth.call(
            {"to": checksum_address, "data": total_supply_calldata},
            block_identifier=block_number,
        )
        calls.append(RecordedCall(token_address, total_supply_calldata, result.hex()))
    except Exception:
        pass

    return calls


def fetch_v2_pool_calls(w3: Web3, pool_config: dict, block_number: int) -> list[RecordedCall]:
    """Fetch and record all calls needed for a V2 pool at a specific block."""

    address = pool_config["address"]
    checksum_address = w3.to_checksum_address(address)
    calls: list[RecordedCall] = []

    # getReserves()
    reserves_calldata = "0x" + encode_function_calldata("getReserves()", None).hex()
    result = w3.eth.call(
        {"to": checksum_address, "data": reserves_calldata}, block_identifier=block_number
    )
    calls.append(RecordedCall(address, reserves_calldata, result.hex()))

    # factory() (only if not provided)
    if pool_config.get("factory") is None:
        factory_calldata = "0x" + encode_function_calldata("factory()", None).hex()
        try:
            result = w3.eth.call(
                {"to": checksum_address, "data": factory_calldata},
                block_identifier=block_number,
            )
            calls.append(RecordedCall(address, factory_calldata, result.hex()))
        except Exception:
            pass

    return calls


def _safe_eth_call(
    w3: Web3,
    address: str,
    data: str,
    block_number: int,
    pool_address: str | None = None,
) -> str | None:
    """
    Safely execute an eth_call, returning None on revert.

    Returns the result hex string on success, or None if the call reverted.
    """
    try:
        result = w3.eth.call(
            {"to": w3.to_checksum_address(address), "data": data},
            block_identifier=block_number,
        )
        return result.hex()
    except Exception:
        return None


def fetch_v3_pool_calls(w3: Web3, pool_config: dict, block_number: int) -> list[RecordedCall]:
    """Fetch and record all calls needed for a V3 pool at a specific block."""

    address = pool_config["address"]
    checksum_address = w3.to_checksum_address(address)
    calls: list[RecordedCall] = []

    # slot0()
    slot0_calldata = "0x" + encode_function_calldata("slot0()", None).hex()
    result = _safe_eth_call(w3, address, slot0_calldata, block_number)
    calls.append(RecordedCall(address, slot0_calldata, result))

    # liquidity()
    liquidity_calldata = "0x" + encode_function_calldata("liquidity()", None).hex()
    result = _safe_eth_call(w3, address, liquidity_calldata, block_number)
    calls.append(RecordedCall(address, liquidity_calldata, result))

    # Fetch token addresses and fee if not provided
    if pool_config.get("token0") is None:
        token0_calldata = "0x" + encode_function_calldata("token0()", None).hex()
        result = _safe_eth_call(w3, address, token0_calldata, block_number)
        calls.append(RecordedCall(address, token0_calldata, result))

    if pool_config.get("token1") is None:
        token1_calldata = "0x" + encode_function_calldata("token1()", None).hex()
        result = _safe_eth_call(w3, address, token1_calldata, block_number)
        calls.append(RecordedCall(address, token1_calldata, result))

    if pool_config.get("fee") is None:
        fee_calldata = "0x" + encode_function_calldata("fee()", None).hex()
        result = _safe_eth_call(w3, address, fee_calldata, block_number)
        calls.append(RecordedCall(address, fee_calldata, result))

    if pool_config.get("tick_spacing") is None:
        tick_spacing_calldata = "0x" + encode_function_calldata("tickSpacing()", None).hex()
        result = _safe_eth_call(w3, address, tick_spacing_calldata, block_number)
        calls.append(RecordedCall(address, tick_spacing_calldata, result))

    return calls


def fetch_v3_pool_tick_data(
    w3: Web3, pool_address: str, tick_spacing: int, block_number: int
) -> dict | None:
    """
    Fetch complete tick bitmap and tick data for a V3 pool.

    Returns a dict with:
    - tick_spacing: The pool's tick spacing
    - tick_bitmap: Dict of word_position -> bitmap
    - tick_data: Dict of tick -> {liquidity_gross, liquidity_net}

    Returns None if the pool doesn't exist at the block.
    """
    checksum_address = w3.to_checksum_address(pool_address)

    # Check if contract exists
    try:
        code = w3.eth.get_code(checksum_address, block_identifier=block_number)
        if len(code) == 0:
            return None
    except Exception:
        return None

    tick_bitmap: dict[str, dict] = {}
    tick_data: dict[str, dict] = {}

    # Calculate word range for V3 (min/max tick is +/-887272)
    # For efficiency, we'll scan a reasonable range around likely active ticks
    # Full scan would be from -3474 to 3474 for tick_spacing=60

    min_word = MIN_TICK // tick_spacing // 256
    max_word = MAX_TICK // tick_spacing // 256

    print(f"    Fetching tick data for words {min_word} to {max_word}...")
    initialized_words = 0
    total_ticks = 0

    for word_pos in range(min_word, max_word + 1):
        # Call tickBitmap(int16 wordPos)
        call_data = "0x" + encode_function_calldata("tickBitmap(int16)", [word_pos]).hex()

        try:
            result = w3.eth.call(
                {"to": checksum_address, "data": call_data},
                block_identifier=block_number,
            )
            (bitmap,) = decode(["uint256"], result)
        except Exception:
            continue

        if bitmap == 0:
            continue

        # Store the bitmap
        tick_bitmap[str(word_pos)] = {
            "bitmap": str(bitmap),
            "block": block_number,
        }
        initialized_words += 1

        # Find all initialized ticks in this word
        # Each bit represents a tick: tick = (word_pos * 256 + bit_pos) * tick_spacing
        for bit_pos in range(256):
            if bitmap & (1 << bit_pos):
                tick = (word_pos * 256 + bit_pos) * tick_spacing

                # Call ticks(int24 tick) to get liquidity data
                ticks_call_data = "0x" + encode_function_calldata("ticks(int24)", [tick]).hex()

                try:
                    tick_result = w3.eth.call(
                        {"to": checksum_address, "data": ticks_call_data},
                        block_identifier=block_number,
                    )
                    # ticks() returns: (liquidityGross, liquidityNet, feeGrowthOutside0X128,
                    # feeGrowthOutside1X128, tickCumulativeOutside, secondsPerLiquidityOutsideX128,
                    # secondsOutside, initialized)
                    decoded = decode(
                        [
                            "uint128",
                            "int128",
                            "uint256",
                            "uint256",
                            "int56",
                            "uint160",
                            "uint32",
                            "bool",
                        ],
                        tick_result,
                    )
                    liquidity_gross = decoded[0]
                    liquidity_net = decoded[1]

                    tick_data[str(tick)] = {
                        "liquidity_gross": str(liquidity_gross),
                        "liquidity_net": str(liquidity_net),
                        "block": block_number,
                    }
                    total_ticks += 1
                except Exception:
                    pass

    if initialized_words == 0:
        return None

    print(f"    Found {initialized_words} initialized words with {total_ticks} ticks")

    return {
        "tick_spacing": tick_spacing,
        "tick_bitmap": tick_bitmap,
        "tick_data": tick_data,
    }


def fetch_v4_pool_calls(w3: Web3, pool_config: dict, block_number: int) -> list[RecordedCall]:
    """Fetch and record all calls needed for a V4 pool at a specific block."""

    pool_id = pool_config["pool_id"]
    state_view = pool_config["state_view"]
    checksum_state_view = w3.to_checksum_address(state_view)
    calls: list[RecordedCall] = []

    # StateView uses pool ID (bytes32)
    pool_id_bytes = bytes.fromhex(pool_id[2:])

    # getSlot0(bytes32)
    slot0_calldata = "0x" + encode_function_calldata("getSlot0(bytes32)", [pool_id_bytes]).hex()
    result = _safe_eth_call(w3, state_view, slot0_calldata, block_number)
    calls.append(RecordedCall(state_view, slot0_calldata, result))

    return calls


def fetch_curve_pool_calls(w3: Web3, pool_config: dict, block_number: int) -> list[RecordedCall]:
    """Fetch and record all calls needed for a Curve pool at a specific block."""

    address = pool_config["address"]
    checksum_address = w3.to_checksum_address(address)
    calls: list[RecordedCall] = []

    # Probe for coins and balances
    for i in range(8):
        # Try coins(i)
        coins_calldata = "0x" + encode_function_calldata("coins(uint256)", [i]).hex()
        try:
            result = w3.eth.call(
                {"to": checksum_address, "data": coins_calldata},
                block_identifier=block_number,
            )
            calls.append(RecordedCall(address, coins_calldata, result.hex()))
        except Exception:
            break

        # Try balances(i)
        balances_calldata = "0x" + encode_function_calldata("balances(uint256)", [i]).hex()
        try:
            result = w3.eth.call(
                {"to": checksum_address, "data": balances_calldata},
                block_identifier=block_number,
            )
            calls.append(RecordedCall(address, balances_calldata, result.hex()))
        except Exception:
            pass

    # A()
    a_calldata = "0x" + encode_function_calldata("A()", None).hex()
    try:
        result = w3.eth.call(
            {"to": checksum_address, "data": a_calldata}, block_identifier=block_number
        )
        calls.append(RecordedCall(address, a_calldata, result.hex()))
    except Exception:
        pass

    return calls


def fetch_contract_code(w3: Web3, address: str, block_number: int) -> RecordedCode | None:
    """Fetch contract code at a specific block."""
    try:
        code = w3.eth.get_code(w3.to_checksum_address(address), block_identifier=block_number)
        if len(code) > 0:
            return RecordedCode(address, code.hex())
    except Exception:
        pass
    return None


def record_chain_data(config_path: Path, output_dir: Path) -> None:
    """Main function to record chain data based on config."""
    # Load config
    with Path(config_path).open(encoding="utf-8") as f:
        config = json.load(f)

    chain_id = config["chain_id"]
    blocks = config["blocks"]
    rpc_url = config["rpc_url"]

    print(f"Connecting to {rpc_url}...")

    w3 = Web3(Web3.HTTPProvider(rpc_url))
    if not w3.is_connected():
        print(f"Failed to connect to RPC at {rpc_url}", file=sys.stderr)
        sys.exit(1)

    # Verify we're on the right chain
    actual_chain_id = w3.eth.chain_id
    if actual_chain_id != chain_id:
        print(
            f"Warning: Config specifies chain {chain_id} but RPC reports {actual_chain_id}",
            file=sys.stderr,
        )

    # Create output directory
    chain_dir = output_dir / str(chain_id)
    chain_dir.mkdir(parents=True, exist_ok=True)

    # Collect all unique contract addresses that need code
    all_addresses: set[str] = set()
    all_addresses.update(config.get("tokens", []))

    pool_config = config.get("pools", {})
    for pool_type in ["v2", "v3", "curve"]:
        for pool in pool_config.get(pool_type, []):
            all_addresses.add(pool["address"])
            if "token0" in pool:
                all_addresses.add(pool["token0"])
            if "token1" in pool:
                all_addresses.add(pool["token1"])

    # Fetch static token info (name, symbol, decimals are immutable)
    print("Fetching token metadata...")
    tokens_data: dict = {}
    for token_address in config.get("tokens", []):
        print(f"  - {token_address}")
        token_info = fetch_token_static_info(w3, token_address)
        tokens_data[token_address] = token_info

    total_calls = 0
    total_v3_pools = 0

    # Record data for each block (one file per block)
    for block_number in blocks:
        print(f"\nRecording block {block_number}...")

        # Get block timestamp
        try:
            block = w3.eth.get_block(block_number)
            timestamp = block["timestamp"]
        except Exception as e:
            print(f"  Warning: Could not fetch block {block_number}: {e}")
            timestamp = 0

        # Build single-block output structure (flattened)
        block_data: dict = {
            "chain_id": chain_id,
            "block_number": block_number,
            "timestamp": timestamp,
            "calls": {},  # Flat dict: "0xto:0xdata" -> "0xresult"
            "code": {},  # "0xaddress" -> "0xcode"
        }

        # Record contract code for all addresses
        print("  Recording contract code...")
        for address in all_addresses:
            code = fetch_contract_code(w3, address, block_number)
            if code:
                block_data["code"][address.lower()] = code.code

        # Record token calls
        print("  Recording token data...")
        for token_address in config.get("tokens", []):
            calls = fetch_token_calls(w3, token_address, block_number)
            for call in calls:
                key = f"{call.to.lower()}:{call.data}"
                block_data["calls"][key] = call.result

        # Record pool calls
        for pool_type in ["v2", "v3", "v4", "curve"]:
            pools = pool_config.get(pool_type, [])
            if not pools:
                continue
            print(f"  Recording {pool_type.upper()} pool data...")
            for pool in pools:
                # Determine which address to check for code
                if pool_type == "v4":
                    # V4 uses state_view address for calls
                    address = pool.get("state_view")
                else:
                    address = pool.get("address")

                if not address:
                    print(f"    Warning: No address for {pool_type} pool")
                    continue

                checksum_address = w3.to_checksum_address(address)
                try:
                    code = w3.eth.get_code(checksum_address, block_identifier=block_number)
                    if len(code) == 0:
                        print(
                            f"    Warning: Contract {address} not deployed at block {block_number}"
                        )
                        continue
                except Exception as e:
                    print(f"    Warning: Could not check contract {address}: {e}")
                    continue

                try:
                    match pool_type:
                        case "v2":
                            calls = fetch_v2_pool_calls(w3, pool, block_number)
                        case "v3":
                            calls = fetch_v3_pool_calls(w3, pool, block_number)
                        case "v4":
                            calls = fetch_v4_pool_calls(w3, pool, block_number)
                        case "curve":
                            calls = fetch_curve_pool_calls(w3, pool, block_number)
                        case _:
                            continue

                    for call in calls:
                        key = f"{call.to.lower()}:{call.data}"
                        block_data["calls"][key] = call.result
                except Exception as e:
                    pool_id = pool.get("address") or pool.get("pool_id") or address
                    print(
                        f"    Warning: Failed to record calls for {pool_type} pool {pool_id}: {e}"
                    )
                    continue

        # Record V3 pool tick data (flattened structure)
        v3_pools = pool_config.get("v3", [])
        if v3_pools:
            print("  Recording V3 pool tick data...")

            for pool in v3_pools:
                address = pool.get("address")
                if not address:
                    continue

                # Get tick spacing from config or fetch it
                tick_spacing = pool.get("tick_spacing")
                if tick_spacing is None:
                    # Try to fetch from chain
                    try:
                        tick_spacing_calldata = (
                            "0x" + encode_function_calldata("tickSpacing()", None).hex()
                        )
                        result = w3.eth.call(
                            {"to": w3.to_checksum_address(address), "data": tick_spacing_calldata},
                            block_identifier=block_number,
                        )
                        (tick_spacing,) = decode(["int24"], result)
                    except Exception:
                        print(f"    Warning: Could not fetch tick_spacing for {address}")
                        continue

                tick_data = fetch_v3_pool_tick_data(w3, address, tick_spacing, block_number)
                if tick_data:
                    # Flatten V3 tick data into top-level keys
                    pool_key = f"v3_{address.lower()}"
                    block_data[f"{pool_key}_tick_spacing"] = tick_data["tick_spacing"]
                    block_data[f"{pool_key}_tick_bitmap"] = tick_data["tick_bitmap"]
                    block_data[f"{pool_key}_tick_data"] = tick_data["tick_data"]
                    print(f"    Recorded tick data for {address}")
                    total_v3_pools += 1

        # Write single-block output file
        output_file = chain_dir / f"block_{block_number}.json"
        with Path(output_file).open("w", encoding="utf-8") as f:
            json.dump(block_data, f, indent=2)

        total_calls += len(block_data["calls"])
        print(f"  Written to: {output_file}")

    print("\nData recording complete:")
    print(f"  Chain: {chain_id}")
    print(f"  Blocks: {len(blocks)}")
    print(f"  Total calls: {total_calls}")
    if total_v3_pools > 0:
        print(f"  V3 pools with tick data: {total_v3_pools}")


def fetch_token_static_info(w3: Web3, address: str) -> dict:
    """Fetch immutable token metadata (name, symbol, decimals)."""
    checksum_address = w3.to_checksum_address(address)

    info = {"address": address, "name": "Unknown", "symbol": "UNK", "decimals": 18}

    # Try name()
    try:
        name_calldata = "0x" + encode_function_calldata("name()", None).hex()
        result = w3.eth.call({"to": checksum_address, "data": name_calldata})

        (name,) = decode(["string"], result)
        info["name"] = name
    except Exception:
        pass

    # Try symbol()
    try:
        symbol_calldata = "0x" + encode_function_calldata("symbol()", None).hex()
        result = w3.eth.call({"to": checksum_address, "data": symbol_calldata})

        (symbol,) = decode(["string"], result)
        info["symbol"] = symbol
    except Exception:
        pass

    # Try decimals()
    try:
        decimals_calldata = "0x" + encode_function_calldata("decimals()", None).hex()
        result = w3.eth.call({"to": checksum_address, "data": decimals_calldata})

        (decimals,) = decode(["uint256"], result)
        info["decimals"] = decimals
    except Exception:
        pass

    return info


def main() -> None:
    """CLI entry point."""
    parser = argparse.ArgumentParser(
        description="Record on-chain data to JSON files for offline testing"
    )
    parser.add_argument(
        "--config",
        type=Path,
        required=True,
        help="Path to JSON config file specifying what data to record",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("tests/fixtures/chain_data"),
        help="Output directory for recorded data (default: tests/fixtures/chain_data)",
    )

    args = parser.parse_args()

    if not args.config.exists():
        print(f"Config file not found: {args.config}", file=sys.stderr)
        sys.exit(1)

    record_chain_data(args.config, args.output)


if __name__ == "__main__":
    main()
