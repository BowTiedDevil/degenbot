"""Balancer V2 deployment addresses and chain configuration."""

from degenbot.checksum_cache import get_checksum_address

BALANCER_V2_VAULT_ADDRESS = get_checksum_address("0xBA12222222228d8Ba445958a75a0704d566BF2C8")

# Balancer V2 pool factory addresses are now sourced from the shipped
# ``registry/deployments.json`` (all factory revisions live there). The
# constants below pin the *latest* revision of each family as a legacy
# convenience for callers that want a single canonical address; see the JSON
# for the full versioned set. Addresses verified against Balancer's official
# mainnet deployment table (docs-v2.balancer.fi) + on-chain bytecode.

# Weighted Pool Factory — latest revision (rev4, 20230320-weighted-pool-v4)
BALANCER_V2_WEIGHTED_POOL_FACTORY = get_checksum_address(
    "0x897888115Ada5773E02aA29F775430BFB5F34c51",
)

# Stable Pool Factory — latest revision (rev2, 20220609-stable-pool-v2)
BALANCER_V2_STABLE_POOL_FACTORY = get_checksum_address(
    "0x8df6EfEc5547e31B0eb7d1291B511FF8a2bf987c",
)

# ComposableStablePool Factory — latest revision (rev6, 20240223-composable-stable-pool-v6)
BALANCER_V2_COMPOSABLE_STABLE_POOL_FACTORY = get_checksum_address(
    "0x5B42eC6D40f7B7965BE5308c70e2603c0281C1E9",
)

BALANCERQUERIES_CONTRACT_ADDRESS = get_checksum_address(
    "0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5",
)

BROKEN_BALANCER_V2_POOLS = frozenset(
    get_checksum_address(pool_address)
    for pool_address in (
        # Swaps disabled (BAL#327 SWAPS_DISABLED)
        "0x753BD6a5bF0b14ae7e5d2877e5cD6a3398aA2AAB",  # YUME/WETH 1/99
    )
)
