import ujson
from eth_utils.address import to_checksum_address

from degenbot import AerodromeV3Pool, Erc20Token
from degenbot.anvil_fork import AnvilFork
from degenbot.config import set_web3
from degenbot.uniswap.v3_libraries.tick_math import MAX_SQRT_RATIO, MIN_SQRT_RATIO

AERODROME_V3_FACTORY_ADDRESS = to_checksum_address("0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A")
CBETH_WETH_POOL_ADDRESS = to_checksum_address("0x47cA96Ea59C13F72745928887f84C9F52C3D7348")
WETH_CONTRACT_ADDRESS = to_checksum_address("0x4200000000000000000000000000000000000006")
CBETH_CONTRACT_ADDRESS = to_checksum_address("0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22")
AERODROME_QUOTER_ADDRESS = to_checksum_address("0x254cF9E1E6e233aa1AC962CB9B05b2cfeAaE15b0")
AERODROME_QUOTER_ABI = ujson.loads(
    """
    [{"inputs":[{"internalType":"address","name":"_factory","type":"address"},{"internalType":"address","name":"_WETH9","type":"address"}],"stateMutability":"nonpayable","type":"constructor"},{"inputs":[],"name":"WETH9","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"factory","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes","name":"path","type":"bytes"},{"internalType":"uint256","name":"amountIn","type":"uint256"}],"name":"quoteExactInput","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint160[]","name":"sqrtPriceX96AfterList","type":"uint160[]"},{"internalType":"uint32[]","name":"initializedTicksCrossedList","type":"uint32[]"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"address","name":"tokenIn","type":"address"},{"internalType":"address","name":"tokenOut","type":"address"},{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"uint160","name":"sqrtPriceLimitX96","type":"uint160"}],"internalType":"struct IQuoterV2.QuoteExactInputSingleParams","name":"params","type":"tuple"}],"name":"quoteExactInputSingle","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint160","name":"sqrtPriceX96After","type":"uint160"},{"internalType":"uint32","name":"initializedTicksCrossed","type":"uint32"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes","name":"path","type":"bytes"},{"internalType":"uint256","name":"amountOut","type":"uint256"}],"name":"quoteExactOutput","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint160[]","name":"sqrtPriceX96AfterList","type":"uint160[]"},{"internalType":"uint32[]","name":"initializedTicksCrossedList","type":"uint32[]"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"address","name":"tokenIn","type":"address"},{"internalType":"address","name":"tokenOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"uint160","name":"sqrtPriceLimitX96","type":"uint160"}],"internalType":"struct IQuoterV2.QuoteExactOutputSingleParams","name":"params","type":"tuple"}],"name":"quoteExactOutputSingle","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint160","name":"sqrtPriceX96After","type":"uint160"},{"internalType":"uint32","name":"initializedTicksCrossed","type":"uint32"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"int256","name":"amount0Delta","type":"int256"},{"internalType":"int256","name":"amount1Delta","type":"int256"},{"internalType":"bytes","name":"path","type":"bytes"}],"name":"uniswapV3SwapCallback","outputs":[],"stateMutability":"view","type":"function"}]
    """  # noqa:E501
)


def test_aerodrome_v3_pool_creation(fork_base: AnvilFork) -> None:
    set_web3(fork_base.w3)

    AerodromeV3Pool(address=CBETH_WETH_POOL_ADDRESS)
    AerodromeV3Pool(
        address=CBETH_WETH_POOL_ADDRESS,
        factory_address=AERODROME_V3_FACTORY_ADDRESS,
    )
    AerodromeV3Pool(
        address=CBETH_WETH_POOL_ADDRESS,
        tokens=[
            Erc20Token(WETH_CONTRACT_ADDRESS),
            Erc20Token(CBETH_CONTRACT_ADDRESS),
        ],
    )
    assert (
        AerodromeV3Pool(
            address=CBETH_WETH_POOL_ADDRESS, tick_bitmap={}, tick_data={}
        ).sparse_liquidity_map
        is False
    )


def test_aerodrome_v3_pool_calculation(fork_base: AnvilFork) -> None:
    set_web3(fork_base.w3)

    quoter = fork_base.w3.eth.contract(address=AERODROME_QUOTER_ADDRESS, abi=AERODROME_QUOTER_ABI)
    lp = AerodromeV3Pool(address="0x98c7A2338336d2d354663246F64676009c7bDa97")

    max_reserves_token0 = max_reserves_token1 = 3_000_000 * 10**6

    TOKEN_AMOUNT_MULTIPLIERS = [
        0.000000001,
        0.00000001,
        0.0000001,
        0.000001,
        0.00001,
        0.0001,
        0.001,
        0.01,
        0.1,
        0.125,
        0.25,
        0.5,
        0.75,
    ]

    for token_mult in TOKEN_AMOUNT_MULTIPLIERS:
        token_in_amount = int(token_mult * max_reserves_token0)
        if token_in_amount == 0:
            continue

        helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token0,
            token_in_quantity=token_in_amount,
        )
        quoter_amount_out, *_ = quoter.functions.quoteExactInputSingle(
            [
                lp.token0.address,  # tokenIn
                lp.token1.address,  # tokenOut
                token_in_amount,  # amountIn
                lp.tick_spacing,  # tickSpacing
                MIN_SQRT_RATIO + 1,  # sqrtPriceLimitX96
            ]
        ).call()

        assert quoter_amount_out == helper_amount_out

        token_in_amount = int(token_mult * max_reserves_token1)
        if token_in_amount == 0:
            continue

        helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token1,
            token_in_quantity=token_in_amount,
        )
        quoter_amount_out, *_ = quoter.functions.quoteExactInputSingle(
            [
                lp.token1.address,  # tokenIn
                lp.token0.address,  # tokenOut
                token_in_amount,  # amountIn
                lp.tick_spacing,  # tickSpacing
                MAX_SQRT_RATIO - 1,  # sqrtPriceLimitX96
            ]
        ).call()

        assert quoter_amount_out == helper_amount_out
