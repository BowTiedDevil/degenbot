from degenbot.uniswap.v3_types import UniswapV3BitmapAtWord, UniswapV3LiquidityAtTick


def test_tick_bitmap() -> None:
    # Test equality
    assert UniswapV3BitmapAtWord(bitmap=0) == UniswapV3BitmapAtWord(bitmap=0)

    # Test uniqueness
    assert UniswapV3BitmapAtWord(bitmap=0) is not UniswapV3BitmapAtWord(bitmap=0)

    # Test dict export method
    assert UniswapV3BitmapAtWord(bitmap=0).model_dump() == {"bitmap": 0, "block": 0}


def test_liquidity_data() -> None:
    # Test equality
    assert UniswapV3LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998
    ) == UniswapV3LiquidityAtTick(liquidity_net=80064092962998, liquidity_gross=80064092962998)

    # Test uniqueness
    assert UniswapV3LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998
    ) is not UniswapV3LiquidityAtTick(liquidity_net=80064092962998, liquidity_gross=80064092962998)

    # Test dict export method
    assert UniswapV3LiquidityAtTick(
        liquidity_net=80064092962998, liquidity_gross=80064092962998
    ).model_dump() == {
        "liquidity_net": 80064092962998,
        "liquidity_gross": 80064092962998,
        "block": 0,
    }
