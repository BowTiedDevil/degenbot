# Overview
Degenbot is a set of Python classes that abstract many of the implementation details of Uniswap liquidity pools and their underlying ERC-20 tokens. It is an abstraction that uses web3.py for communication with an EVM blockchain.

These classes serve as a building blocks for the lessons published by [BowTiedDevil](https://twitter.com/BowTiedDevil) on [Degen Code](https://www.degencode.com/).

The classes originally relied on [Brownie](https://github.com/eth-brownie/), but have evolved to use [web3.py](https://github.com/ethereum/web3.py/) more generally following Brownie's transition to "maintenance mode". The degenbot classes still operate when loaded within a Brownie console or when connected to a Python script with a connected `chain` object.

## Prerequisites
Python version 3.10 or newer.

## Installation
There are two ways to install degenbot, both require `pip`.

### From PyPI
`pip install degenbot` will fetch the latest version from [PyPI](https://pypi.org/project/degenbot/) with dependencies.

### From Source
Use `git clone` to create a local copy of this repo, then install with `pip install -e /path/to/repo`. This creates an editable installation that can be imported into a script or Python REPL using `import degenbot`.

## Examples
The following snippets assume a connected `Web3` instance with a working provider on Ethereum mainnet (chain ID #1), and the classes imported under the `degenbot` namespace.

### Uniswap V2 Liquidity Pools

```
# Create `LiquidityPool` object from on-chain data at the given address and current chain height
>>> lp = degenbot.LiquidityPool('0xBb2b8038a1640196FbE3e38816F3e67Cba72D940')
• WBTC (Wrapped BTC)
• WETH (Wrapped Ether)
WBTC-WETH (V2, 0.30%)
• Token 0: WBTC - Reserves: 10732489743
• Token 1: WETH - Reserves: 2056834999904002274711

# Get information about the tokens held by the pool
>>> lp.token0.name
'Wrapped BTC'

>>> lp.token1.name
'Wrapped Ether'

>>> lp.fee_token0
Fraction(3, 1000)

>>> lp.fee_token1
Fraction(3, 1000)

# Predict the input and output values for swaps through the pool, accounting for fees
>>> lp.calculate_tokens_out_from_tokens_in(token_in=lp.token1, token_in_quantity=1*10**18)
5199789

>>> lp.calculate_tokens_in_from_tokens_out(token_out=lp.token0, token_out_quantity=5199789)
999999992817074189

# Update the current reserves from the live blockchain (`True` if updated reserves found, `False` otherwise)
>>> lp.update_reserves()
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

TBD

### Uniswap Arbitrage

TBD

### Local Forking With Anvil

TBD