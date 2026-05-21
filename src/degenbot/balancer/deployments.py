from degenbot.checksum_cache import get_checksum_address

BALANCER_V2_VAULT_ADDRESS = get_checksum_address("0xBA12222222228d8Ba445958a75a0704d566BF2C8")

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
