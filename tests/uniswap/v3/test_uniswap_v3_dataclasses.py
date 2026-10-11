from degenbot.uniswap.concentrated.types import LiquidityAtTick


def test_liquidity_data() -> None:
    # Test equality
    assert LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998, block=0
    ) == LiquidityAtTick(liquidity_net=80064092962998, liquidity_gross=80064092962998, block=0)

    # Test uniqueness
    assert LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998, block=0
    ) is not LiquidityAtTick(liquidity_net=80064092962998, liquidity_gross=80064092962998, block=0)

    # Test dict export method
    assert LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998, block=0
    ).model_dump() == {
        "liquidity_net": 80064092962998,
        "liquidity_gross": 80064092962998,
        "block": 0,
    }
