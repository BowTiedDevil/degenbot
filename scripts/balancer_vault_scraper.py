#!/usr/bin/env python3
# ruff: noqa: BLE001, G004, PLR0917, PLW0603, SIM105, S110, T201, TRY400
"""Discover Balancer V2 pools from the Vault contract events.

Standalone one-off script — no degenbot imports.

Usage:
    python balancer_vault_scraper.py [--rpc URL] [--start-block N] [--end-block N]
"""

import argparse
import csv
import json
import logging
import pathlib
import sys
from collections import defaultdict
from dataclasses import dataclass

from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3
from web3.types import LogReceipt

logging.basicConfig(level=logging.INFO, format="%(message)s")
logger = logging.getLogger(__name__)

# ---------- Constants ----------

VAULT_ADDRESS = Web3.to_checksum_address(
    "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
)

# PoolRegistered(bytes32 indexed poolId, address indexed poolAddress, uint8 specialization)
POOL_REGISTERED_TOPIC = HexBytes(
    "0x3c13bc30b8e878c53fd2a36b679409c073afd75950be43d8858768e956fbc20e"
)

# TokensRegistered(bytes32 indexed poolId, address[] tokens, address[] assetManagers)
TOKENS_REGISTERED_TOPIC = HexBytes(
    "0xf5847d3f2197b16cdcd2098ec95d0905cd1abdaf415f07bb7cef2bba8ac5dec4"
)

VAULT_DEPLOY_BLOCK = 12_274_000

# ---------- Minimal ABIs ----------

VAULT_ABI = [
    {
        "inputs": [
            {"internalType": "bytes32", "name": "poolId", "type": "bytes32"}
        ],
        "name": "getPoolTokens",
        "outputs": [
            {
                "internalType": "contract IERC20[]",
                "name": "",
                "type": "address[]",
            },
            {"internalType": "uint256[]", "name": "", "type": "uint256[]"},
            {"internalType": "uint256", "name": "", "type": "uint256"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
]

POOL_ABI = [
    {
        "inputs": [],
        "name": "getNormalizedWeights",
        "outputs": [
            {"internalType": "uint256[]", "name": "", "type": "uint256[]"}
        ],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [],
        "name": "getSwapFeePercentage",
        "outputs": [
            {"internalType": "uint256", "name": "", "type": "uint256"}
        ],
        "stateMutability": "view",
        "type": "function",
    },
]


# ---------- Data classes ----------


@dataclass(frozen=True, kw_only=True, slots=True)
class DiscoveredPool:
    pool_id: bytes
    address: ChecksumAddress
    specialization: int
    block_number: int
    log_index: int


@dataclass(frozen=True, kw_only=True, slots=True)
class EnrichedPool:
    pool_id: bytes
    address: ChecksumAddress
    specialization: int
    block_number: int
    log_index: int
    tokens: tuple[ChecksumAddress, ...]
    balances: tuple[int, ...]
    weights: tuple[int, ...] | None
    swap_fee_percentage: int | None


SPECIALIZATION_NAMES = {0: "MINIMAL_SWAP_INFO", 1: "TWO_TOKEN", 2: "GENERAL"}


# ---------- Log fetching ----------

MAX_RETRIES = 5

DEFAULT_CHUNK_SIZE = 50_000


def fetch_logs_chunked(
    w3: Web3,
    address: str,
    topics: list[str],
    start_block: int,
    end_block: int,
    chunk_size: int = DEFAULT_CHUNK_SIZE,
) -> list[LogReceipt]:
    """Fetch logs in chunks to avoid RPC limits."""
    all_logs: list[LogReceipt] = []
    current = start_block

    while current <= end_block:
        chunk_end = min(current + chunk_size - 1, end_block)
        logger.info("  Fetching blocks %s-%s", current, chunk_end)

        for attempt in range(5):
            try:
                logs = w3.eth.get_logs(
                    {
                        "address": address,
                        "fromBlock": current,
                        "toBlock": chunk_end,
                        "topics": topics,
                    }
                )
                all_logs.extend(logs)
                break
            except Exception as e:
                if attempt == MAX_RETRIES - 1:
                    logger.error("Failed after 5 attempts: %s", e)
                    raise
                # Reduce chunk size on timeout
                chunk_size = max(1, chunk_size // 4)
                logger.warning(
                    f"Attempt {attempt + 1} failed, retrying with chunk_size={chunk_size}: {e}"
                )
        else:
            # All attempts exhausted for this chunk, skip
            logger.error("Skipping blocks %s-%s", current, chunk_end)

        current = chunk_end + 1

    return all_logs


# ---------- Decoding ----------


def decode_pool_registered(log: LogReceipt) -> DiscoveredPool:
    pool_id = bytes(HexBytes(log["topics"][1]))
    pool_address = Web3.to_checksum_address(
        "0x" + HexBytes(log["topics"][2])[-20:].hex()
    )
    specialization = int.from_bytes(HexBytes(log["data"]), "big")
    return DiscoveredPool(
        pool_id=pool_id,
        address=pool_address,
        specialization=specialization,
        block_number=log["blockNumber"],
        log_index=log["logIndex"],
    )


def decode_tokens_registered(
    log: LogReceipt,
) -> tuple[bytes, tuple[ChecksumAddress, ...]]:
    pool_id = bytes(HexBytes(log["topics"][1]))
    decoded = w3.codec.decode(["address[]", "address[]"], HexBytes(log["data"]))
    tokens = tuple(Web3.to_checksum_address(addr) for addr in decoded[0])
    return pool_id, tokens


# ---------- Main ----------

w3: Web3  # module-level for decode_tokens_registered


def main() -> None:
    global w3

    parser = argparse.ArgumentParser(description="Discover Balancer V2 pools")
    parser.add_argument(
        "--rpc",
        default="https://eth.llamarpc.com",
        help="Ethereum RPC URL (default: https://eth.llamarpc.com)",
    )
    parser.add_argument(
        "--start-block",
        type=int,
        default=VAULT_DEPLOY_BLOCK,
        help=f"Start block (default: {VAULT_DEPLOY_BLOCK})",
    )
    parser.add_argument(
        "--end-block",
        type=int,
        default=None,
        help="End block (default: latest)",
    )
    parser.add_argument(
        "--enrich",
        action="store_true",
        help="Enrich pools with on-chain data (tokens, balances, weights, fees)",
    )
    parser.add_argument(
        "--weights-only",
        action="store_true",
        help="Only print pools that have getNormalizedWeights (weighted pools)",
    )
    parser.add_argument(
        "--output",
        "-o",
        default=None,
        help="Output file path (.json or .csv). Writes enriched pool data.",
    )
    args = parser.parse_args()

    w3 = Web3(Web3.HTTPProvider(args.rpc))
    if not w3.is_connected():
        logger.error(f"Cannot connect to {args.rpc}")
        sys.exit(1)

    end_block = args.end_block or w3.eth.block_number

    logger.info(
        f"Discovering Balancer V2 pools: blocks {args.start_block}-{end_block} "
        f"({end_block - args.start_block:,} blocks)"
    )

    # --- Fetch PoolRegistered events ---
    pool_logs = fetch_logs_chunked(
        w3,
        address=VAULT_ADDRESS,
        topics=[POOL_REGISTERED_TOPIC.to_0x_hex()],
        start_block=args.start_block,
        end_block=end_block,
    )

    pools = [decode_pool_registered(log) for log in pool_logs]
    pools.sort(key=lambda p: (p.block_number, p.log_index))

    # Deduplicate by pool_id
    seen: set[bytes] = set()
    unique: list[DiscoveredPool] = []
    for pool in pools:
        if pool.pool_id not in seen:
            seen.add(pool.pool_id)
            unique.append(pool)

    logger.info(f"Discovered {len(unique)} pools ({len(pools)} total events)")

    if not args.enrich:
        # Just print the list
        for pool in unique:
            spec_name = SPECIALIZATION_NAMES.get(pool.specialization, str(pool.specialization))
            print(
                f"  {pool.address}  poolId=0x{pool.pool_id.hex()[:16]}...  "
                f"spec={spec_name}  block={pool.block_number}"
            )
        return

    # --- Enrich with on-chain data ---
    vault = w3.eth.contract(address=VAULT_ADDRESS, abi=VAULT_ABI)

    enriched: list[EnrichedPool] = []
    for i, pool in enumerate(unique):
        pool_contract = w3.eth.contract(address=pool.address, abi=POOL_ABI)

        # getPoolTokens
        try:
            result = vault.functions.getPoolTokens(pool.pool_id).call()
            tokens = tuple(Web3.to_checksum_address(t) for t in result[0])
            balances = tuple(result[1])
        except Exception as e:
            logger.warning(f"  [{i + 1}/{len(unique)}] {pool.address}: getPoolTokens failed: {e}")
            continue

        # getNormalizedWeights (only weighted pools)
        weights: tuple[int, ...] | None = None
        try:
            weights = tuple(pool_contract.functions.getNormalizedWeights().call())
        except Exception:
            pass

        # getSwapFeePercentage
        swap_fee: int | None = None
        try:
            swap_fee = pool_contract.functions.getSwapFeePercentage().call()
        except Exception:
            pass

        enriched.append(
            EnrichedPool(
                pool_id=pool.pool_id,
                address=pool.address,
                specialization=pool.specialization,
                block_number=pool.block_number,
                log_index=pool.log_index,
                tokens=tokens,
                balances=balances,
                weights=weights,
                swap_fee_percentage=swap_fee,
            )
        )

        if not args.weights_only or weights is not None:
            weight_str = (
                ", ".join(f"{w / 1e18 * 100:.1f}%" for w in weights) if weights else "N/A"
            )
            fee_str = f"{swap_fee / 1e16:.4f}%" if swap_fee is not None else "N/A"
            token_str = ", ".join(tokens)
            logger.info(
                f"  [{i + 1}/{len(unique)}] {pool.address}  "
                f"tokens=({token_str})  weights=({weight_str})  fee={fee_str}"
            )

    # --- Summary ---
    by_spec: dict[int, int] = defaultdict(int)
    by_token_count: dict[int, int] = defaultdict(int)
    weighted_count = 0
    for pool in enriched:
        by_spec[pool.specialization] += 1
        by_token_count[len(pool.tokens)] += 1
        if pool.weights is not None:
            weighted_count += 1

    print(f"\n{'=' * 60}")
    print("Balancer V2 Pool Discovery Summary")
    print(f"{'=' * 60}")
    print(f"Total pools: {len(enriched)}")
    print()
    for spec_val, spec_name in SPECIALIZATION_NAMES.items():
        print(f"  {spec_name}: {by_spec.get(spec_val, 0)}")
    print()
    for n_tokens in sorted(by_token_count):
        print(f"  {n_tokens}-token pools: {by_token_count[n_tokens]}")
    print()
    print(f"Weighted pools (with getNormalizedWeights): {weighted_count}")
    print(f"{'=' * 60}")

    # --- Write output file ---
    if args.output:
        _write_output(args.output, enriched)


def _write_output(path: str, pools: list[EnrichedPool]) -> None:
    """Write enriched pool data to a JSON or CSV file."""
    if path.endswith(".csv"):
        with pathlib.Path(path).open("w", newline="", encoding="utf-8") as f:
            writer = csv.writer(f)
            writer.writerow([
                "pool_id",
                "address",
                "specialization",
                "block_number",
                "tokens",
                "balances",
                "weights",
                "swap_fee_percentage",
            ])
            for pool in pools:
                writer.writerow([
                    f"0x{pool.pool_id.hex()}",
                    pool.address,
                    SPECIALIZATION_NAMES.get(pool.specialization, str(pool.specialization)),
                    pool.block_number,
                    ";".join(pool.tokens),
                    ";".join(str(b) for b in pool.balances),
                    ";".join(str(w) for w in pool.weights) if pool.weights else "",
                    str(pool.swap_fee_percentage) if pool.swap_fee_percentage is not None else "",
                ])
    else:
        # Default to JSON
        data = [
            {
                "pool_id": f"0x{pool.pool_id.hex()}",
                "address": pool.address,
                "specialization": SPECIALIZATION_NAMES.get(
                    pool.specialization, str(pool.specialization)
                ),
                "block_number": pool.block_number,
                "tokens": list(pool.tokens),
                "balances": list(pool.balances),
                "weights": list(pool.weights) if pool.weights else None,
                "swap_fee_percentage": pool.swap_fee_percentage,
            }
            for pool in pools
        ]
        with pathlib.Path(path).open("w", encoding="utf-8") as f:
            json.dump(data, f, indent=2)
    logger.info("Wrote %d pools to %s", len(pools), path)


if __name__ == "__main__":
    main()
