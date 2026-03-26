"""
Helper script to gather debug information for Aave investigations.

This script extracts useful data in a single execution for Aave debug investigations:
- Next available issue ID from debug/aave/
- RPC URL for the specified chain
- Pool contract revision from database
- AToken and VToken revisions for all assets
"""

import argparse
import json
import re
import sys
from pathlib import Path

from sqlalchemy.orm import joinedload

from degenbot.database import db_session
from degenbot.database.models.aave import AaveV3Asset, AaveV3Contract, AaveV3Market
from degenbot.logging import logger


def get_next_issue_id(debug_report_path: Path = Path("debug/aave")) -> str:
    """
    Scan debug/aave directory and return the next available 4-digit issue ID.

    Creates the directory if it does not exist.

    Args:
        debug_report_path: Path to the debug report directory

    Returns:
        Next available issue ID as a 4-digit string (e.g., "0030")
    """

    # Create the directory if it doesn't exist
    debug_report_path.mkdir(parents=True, exist_ok=True)

    issue_ids = []
    pattern = re.compile(r"^(\d{4}) - .+\.md$")

    for file_path in debug_report_path.glob("*.md"):
        match = pattern.match(file_path.name)
        if match:
            issue_ids.append(int(match.group(1)))

    if not issue_ids:
        return "0001"

    next_id = max(issue_ids) + 1
    return f"{next_id:04d}"


def get_rpc_url(chain_id: int, config_path: Path = Path(".opencode/rpc-config.json")) -> str | None:
    """
    Load RPC config and return URL for the specified chain ID.

    Args:
        chain_id: The chain ID to look up
        config_path: Path to the RPC config JSON file

    Returns:
        RPC URL string or None if not found
    """
    if not config_path.exists():
        return None

    with config_path.open(encoding="utf-8") as f:
        config = json.load(f)

    # Try chain_id as string first, then common names
    chain_key = str(chain_id)
    if chain_key in config:
        return config[chain_key]

    # Try common chain names
    chain_names = {
        1: ["ethereum", "mainnet"],
        137: ["polygon"],
        42161: ["arbitrum"],
        10: ["optimism"],
        43114: ["avalanche"],
        250: ["fantom"],
        56: ["bsc", "bnb", "binance"],
        8453: ["base"],
        324: ["zksync", "zksync_era"],
        59144: ["linea"],
        1088: ["metis"],
        100: ["gnosis", "xdai"],
    }

    for name in chain_names.get(chain_id, []):
        if name in config:
            return config[name]

    return None


def get_aave_debug_info(market_id: int) -> dict:
    """
    Gather comprehensive debug information for an Aave market investigation.

    Args:
        market_id: The database ID of the Aave market to investigate

    Returns:
        Dictionary containing:
        - next_issue_id: Next available issue ID for debug reports
        - market: Market info (id, name, chain_id)
        - rpc_url: RPC URL for the chain
        - pool_revision: Pool contract revision number
        - assets: List of assets with their token revisions
    """

    result = {
        "next_issue_id": get_next_issue_id(),
        "market": None,
        "rpc_url": None,
        "pool_revision": None,
        "assets": [],
    }

    with db_session() as session:
        # Get market info
        market = session.query(AaveV3Market).filter(AaveV3Market.id == market_id).first()
        if not market:
            msg = f"Market with ID {market_id} not found in database"
            raise ValueError(msg)

        result["market"] = {
            "id": market.id,
            "name": market.name,
            "chain_id": market.chain_id,
        }

        # Get RPC URL
        result["rpc_url"] = get_rpc_url(market.chain_id)

        # Get Pool contract revision
        pool_contract = (
            session
            .query(AaveV3Contract)
            .filter(AaveV3Contract.market_id == market_id, AaveV3Contract.name == "POOL")
            .first()
        )
        if pool_contract:
            result["pool_revision"] = pool_contract.revision

        # Get all assets with their token revisions
        assets = (
            session
            .query(AaveV3Asset)
            .filter(AaveV3Asset.market_id == market_id)
            .options(
                joinedload(AaveV3Asset.underlying_token),
                joinedload(AaveV3Asset.a_token),
                joinedload(AaveV3Asset.v_token),
            )
            .all()
        )

        for asset in assets:
            asset_info = {
                "underlying_symbol": (
                    asset.underlying_token.symbol if asset.underlying_token else "Unknown"
                ),
                "a_token_address": (str(asset.a_token.address) if asset.a_token else None),
                "a_token_revision": asset.a_token_revision,
                "v_token_address": (str(asset.v_token.address) if asset.v_token else None),
                "v_token_revision": asset.v_token_revision,
            }
            result["assets"].append(asset_info)

    return result


def format_output(data: dict) -> str:
    """Format the debug info as a readable string."""
    lines = []
    lines.extend((f"Next Issue ID: {data['next_issue_id']}", ""))

    if data["market"]:
        lines.extend((
            f"Market: {data['market']['name']} (ID: {data['market']['id']})",
            f"Chain ID: {data['market']['chain_id']}",
        ))
    lines.extend((
        f"RPC URL: {data['rpc_url'] or 'Not configured'}",
        "",
        f"Pool Contract Revision: {data['pool_revision'] or 'Not found'}",
        "",
        "Assets:",
        "-" * 98,
        f"{'Symbol':<10} {'AToken Rev':<12} {'VToken Rev':<12} "
        f"{'AToken Address':<32} {'VToken Address':<32}",
        "-" * 98,
    ))

    for asset in data["assets"]:
        symbol = asset["underlying_symbol"] or "Unknown"
        a_token_addr = (asset["a_token_address"] or "N/A")[:32]
        v_token_addr = (asset["v_token_address"] or "N/A")[:32]
        lines.append(
            f"{symbol:<10} "
            f"{asset['a_token_revision']:<12} "
            f"{asset['v_token_revision']:<12} "
            f"{a_token_addr:<32} "
            f"{v_token_addr:<32}"
        )

    return "\n".join(lines)


def main() -> None:
    """CLI entry point for the helper script."""
    parser = argparse.ArgumentParser(
        description="Gather debug information for Aave market investigations"
    )
    parser.add_argument(
        "--market-id",
        type=int,
        required=True,
        help="Database ID of the Aave market to investigate",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Output as JSON instead of formatted text",
    )

    args = parser.parse_args()

    try:
        data = get_aave_debug_info(args.market_id)

        if args.json:
            logger.info(json.dumps(data, indent=2, default=str))
        else:
            logger.info(format_output(data))

    except ValueError as e:
        logger.info(f"Error: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
