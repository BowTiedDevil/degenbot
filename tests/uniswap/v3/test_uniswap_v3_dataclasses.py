from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick


def test_tick_bitmap() -> None:
    # Test equality
    assert BitmapAtWord(bitmap=0) == BitmapAtWord(bitmap=0)

    # Test uniqueness
    assert BitmapAtWord(bitmap=0) is not BitmapAtWord(bitmap=0)

    # Test dict export method
    assert BitmapAtWord(bitmap=0).model_dump() == {"bitmap": 0, "block": 0}


def test_liquidity_data() -> None:
    # Test equality
    assert LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998
    ) == LiquidityAtTick(liquidity_net=80064092962998, liquidity_gross=80064092962998)

    # Test uniqueness
    assert LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998
    ) is not LiquidityAtTick(liquidity_net=80064092962998, liquidity_gross=80064092962998)

    # Test dict export method
    assert LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998
    ).model_dump() == {
        "liquidity_net": 80064092962998,
        "liquidity_gross": 80064092962998,
        "block": 0,
    }
