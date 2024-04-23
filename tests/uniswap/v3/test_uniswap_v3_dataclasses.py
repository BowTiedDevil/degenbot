from degenbot.uniswap.v3_dataclasses import UniswapV3BitmapAtWord, UniswapV3LiquidityAtTick


def test_tick_bitmap() -> None:
    # Test equality
    assert UniswapV3BitmapAtWord(bitmap=0) == UniswapV3BitmapAtWord(bitmap=0)

    # Test uniqueness
    assert UniswapV3BitmapAtWord(bitmap=0) is not UniswapV3BitmapAtWord(bitmap=0)

    # Test dict export method
    assert UniswapV3BitmapAtWord(bitmap=0).to_dict() == {"bitmap": 0, "block": None}


def test_liquidity_data() -> None:
    # Test equality
    assert UniswapV3LiquidityAtTick(
        liquidityNet=80064092962998, liquidityGross=80064092962998
    ) == UniswapV3LiquidityAtTick(liquidityNet=80064092962998, liquidityGross=80064092962998)

    # Test uniqueness
    assert UniswapV3LiquidityAtTick(
        liquidityNet=80064092962998, liquidityGross=80064092962998
    ) is not UniswapV3LiquidityAtTick(liquidityNet=80064092962998, liquidityGross=80064092962998)

    # Test dict export method
    assert UniswapV3LiquidityAtTick(
        liquidityNet=80064092962998, liquidityGross=80064092962998
    ).to_dict() == {"liquidityNet": 80064092962998, "liquidityGross": 80064092962998, "block": None}
