"""Balancer V2 deployment addresses and chain configuration."""
from degenbot.checksum_cache import get_checksum_address

BALANCER_V2_VAULT_ADDRESS = get_checksum_address("0xBA12222222228d8Ba445958a75a0704d566BF2C8")

# Balancer V2 Weighted Pool Factory (v3)
BALANCER_V2_WEIGHTED_POOL_FACTORY = get_checksum_address(
    "0x8E9aa87E45e92bad7D5F7F9Dd794cea12F21707B"
)

# Balancer V2 Stable Pool Factory (v1)
BALANCER_V2_STABLE_POOL_FACTORY = get_checksum_address("0x8519F5A4A85678E0e03395586E2E223d70E9E09B")

# ComposableStablePool Factory (v2)
BALANCER_V2_COMPOSABLE_STABLE_POOL_FACTORY = get_checksum_address(
    "0xA8936f4824B2E6407Fc0e94133909aeF7d48e876"
)

BALANCERQUERIES_CONTRACT_ADDRESS = get_checksum_address(
    "0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5"
)

BROKEN_BALANCER_V2_POOLS = frozenset(
    get_checksum_address(pool_address)
    for pool_address in (
        # Swaps disabled (BAL#327 SWAPS_DISABLED)
        "0x753BD6a5bF0b14ae7e5d2877e5cD6a3398aA2AAB",  # YUME/WETH 1/99
    )
)
