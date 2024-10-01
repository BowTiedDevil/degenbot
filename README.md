# Overview
Degenbot is a set of Python classes that abstract many of the implementation details of Uniswap liquidity pools and their underlying ERC-20 tokens. It is an abstraction that uses web3.py for communication with an EVM blockchain.

These classes serve as a building blocks for the lessons published by [BowTiedDevil](https://twitter.com/BowTiedDevil) on [Degen Code](https://www.degencode.com/).

The classes originally relied on [Brownie](https://github.com/eth-brownie/brownie), but have evolved to use [web3.py](https://github.com/ethereum/web3.py/) more generally following Brownie's transition to "maintenance mode". The degenbot classes still operate when loaded within a Brownie console or when connected to a Python script with a connected `chain` object.

## Prerequisites
Python version 3.10 or newer.

## Installation
There are two ways to install degenbot, both require `pip` or similar package management tool.

### From PyPI
`pip install degenbot` will fetch the latest version from [PyPI](https://pypi.org/project/degenbot/) with dependencies.

### From Source
Use `git clone` to create a local copy of this repo, then install with `pip install -e /path/to/repo`. This creates an editable installation that can be imported into a script or Python REPL using `import degenbot`.

## Examples
The following snippets assume a connected `Web3` instance with a working provider on Ethereum mainnet (chain ID #1), and the classes imported under the `degenbot` namespace.

### Uniswap V2 Liquidity Pools
```
# Create `UniswapV2Pool` object from on-chain data at the given address and current chain height
>>> lp = degenbot.UniswapV2Pool('0xBb2b8038a1640196FbE3e38816F3e67Cba72D940')
• WBTC (Wrapped BTC)
• WETH (Wrapped Ether)
WBTC-WETH (V2, 0.30%)
• Token 0: WBTC - Reserves: 10732489743
• Token 1: WETH - Reserves: 2056834999904002274711

# Inspect the tokens held by the pool
>>> lp.token0
Erc20Token(address=0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599, symbol='WBTC', name='Wrapped BTC', decimals=8)

>>> lp.token1
Erc20Token(address=0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2, symbol='WETH', name='Wrapped Ether', decimals=18)

>>> lp.fee_token0
Fraction(3, 1000)

>>> lp.fee_token1
Fraction(3, 1000)

# Predict the input and output values for swaps through the pool, accounting for fees
>>> lp.calculate_tokens_out_from_tokens_in(
    token_in=lp.token1, 
    token_in_quantity=1*10**18
)
5199789

>>> lp.calculate_tokens_in_from_tokens_out(
    token_out=lp.token0, 
    token_out_quantity=5199789
)
999999992817074189

# Update the current reserves from the live blockchain (`True` if updated reserves found, `False` otherwise)
>>> lp.update_reserves(silent=False)
[WBTC-WETH (V2, 0.30%)]
WBTC: 10732455184
WETH: 2056841643098872755548
WBTC/WETH: 0.05217929741946638
WETH/WBTC: 19.164688860431703
True
       
>>> lp.reserves_token0
10732455184

>>> lp.reserves_token1
2056841643098872755548
```

### Uniswap V3 Liquidity Pools
```
>>> lp = degenbot.UniswapV3Pool('0xCBCdF9626bC03E24f779434178A73a0B4bad62eD')
WBTC-WETH (V3, 0.30%)
• Token 0: WBTC
• Token 1: WETH
• Fee: 3000
• Liquidity: 544425151051415575
• SqrtPrice: 34048891009198980752047510166697902
• Tick: 259432

# Calculate inputs and outputs
>>> lp.calculate_tokens_out_from_tokens_in(
    token_in=lp.token1, 
    token_in_quantity=1*10**18
)
5398169

>>> lp.calculate_tokens_in_from_tokens_out(
    token_out=lp.token0, 
    token_out_quantity=5398169
)
999999871563434214

# Inspect the known liquidity positions
>>> lp.tick_bitmap
{
    0: UniswapV3BitmapAtWord(
        bitmap=1,
        block=18517665
        ),
    16: UniswapV3BitmapAtWord(
        bitmap=115792089237316195423570985008687907853268655437644779123584680198630541352072,block=18517670
        )
}

# NOTE: the V3 liquidity pool helper is optimized for fast instantiation, and will 
lazy-load liquidity data for positions outside of the current range as needed.

>>> lp.tick_data
{
    0: UniswapV3LiquidityAtTick(
        liquidityNet=10943161472679, liquidityGross=10943161472679, block=18517665
    ),
    261060: UniswapV3LiquidityAtTick(
        liquidityNet=-910396189679465, liquidityGross=910396189679465, block=18517670
    ),
    261000: UniswapV3LiquidityAtTick(
        liquidityNet=-3774266260841234, liquidityGross=3774266260841234, block=18517670
    ),
   
    ...

    246360: UniswapV3LiquidityAtTick(
        liquidityNet=1235001955603188, liquidityGross=1235001955603188, block=18517670
    ),
    246180: UniswapV3LiquidityAtTick(
        liquidityNet=4890971540, liquidityGross=4890971540, block=18517670
    ),
    245940: UniswapV3LiquidityAtTick(
        liquidityNet=76701235421656, liquidityGross=76701235421656, block=18517670
    ),
}
```

### Uniswap Arbitrage
TBD

### Local Forking With Anvil
TBD