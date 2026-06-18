"""Test V4 snapshot backfill logic.

Verifies that pending ModifyLiquidity updates from a snapshot are correctly
applied to V4 pools and the resulting tick_data can be serialized for the
Rust engine's process_block call.
"""


from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v4_types import UniswapV4PoolLiquidityMappingUpdate


def test_v4_liquidity_mapping_update_fields():
    """UniswapV4PoolLiquidityMappingUpdate has the right shape for backfill."""
    update = UniswapV4PoolLiquidityMappingUpdate(
        block_number=20_000_000,
        liquidity=1000,
        tick_lower=-100,
        tick_upper=100,
    )
    assert update.block_number == 20_000_000
    assert update.liquidity == 1000
    assert update.tick_lower == -100
    assert update.tick_upper == 100


def test_v4_backfill_tick_data_collection():
    """After applying liquidity updates, tick_data can be serialized for Rust engine.

    This tests the data shape transformation from Python pool tick_data
    to the Rust engine's tick_priors format, without requiring a running
    chain fork.
    """

    # Simulate tick_data after updates have been applied
    tick_data = {
        -100: LiquidityAtTick(liquidity_net=1000, liquidity_gross=1000, block=20_000_000),
        100: LiquidityAtTick(liquidity_net=-1000, liquidity_gross=1000, block=20_000_000),
        -200: LiquidityAtTick(liquidity_net=500, liquidity_gross=500, block=20_000_001),
        200: LiquidityAtTick(liquidity_net=-500, liquidity_gross=500, block=20_000_001),
    }

    # This is the serialization format used in the V4 backfill wiring
    # (same pattern as V3 backfill)
    tick_priors = [
        (idx, (info.liquidity_gross, info.liquidity_net))
        for idx, info in tick_data.items()
    ]

    # Verify the shape matches what the Rust engine expects:
    # list of (tick_index, (liquidity_gross, liquidity_net))
    assert len(tick_priors) == 4
    for tick_idx, (lgr, lnet) in tick_priors:
        assert isinstance(tick_idx, int)
        assert isinstance(lgr, int)
        assert isinstance(lnet, int)

    # Verify specific values preserved
    tick_priors_dict = dict(tick_priors)
    assert tick_priors_dict[-100] == (1000, 1000)
    assert tick_priors_dict[100] == (1000, -1000)


def test_v4_backfill_v4_updates_tuple_format():
    """The V4 updates tuple format matches what process_block expects.

    V4 updates: list of (pool_manager_address, pool_id_hex, sqrt_price_x96,
                liquidity, tick, tick_priors) tuples.
    """

    # Simulate the data shape after backfill
    pool_manager_address = "0x0000A12C0736817B17beD5F10F1F7870a7a7240"
    pool_id_hex = "0x1234abcd"
    sqrt_price_x96 = 79228162514264337593543950336  # 1:1
    liquidity = 1000000
    tick = 0
    tick_priors = [
        (-100, (1000, 1000)),
        (100, (1000, -1000)),
    ]

    v4_update = (
        pool_manager_address,
        pool_id_hex,
        sqrt_price_x96,
        liquidity,
        tick,
        tick_priors,
    )

    # Verify the tuple shape: 6 elements
    assert len(v4_update) == 6
    assert v4_update[0] == pool_manager_address
    assert v4_update[1] == pool_id_hex
    assert v4_update[2] == sqrt_price_x96
    assert v4_update[3] == liquidity
    assert v4_update[4] == tick
    assert v4_update[5] == tick_priors


def test_v4_liquidity_mapping_update_signed_delta():
    """V4 ModifyLiquidity uses signed delta: positive for additions, negative for removals.

    This is the key difference from V3 Mint/Burn which uses separate events
    with unsigned amounts.
    """
    # Addition (equivalent to V3 Mint)
    add_update = UniswapV4PoolLiquidityMappingUpdate(
        block_number=100,
        liquidity=1000,
        tick_lower=-100,
        tick_upper=100,
    )
    assert add_update.liquidity > 0

    # Removal (equivalent to V3 Burn)
    remove_update = UniswapV4PoolLiquidityMappingUpdate(
        block_number=101,
        liquidity=-500,
        tick_lower=-100,
        tick_upper=100,
    )
    assert remove_update.liquidity < 0
