# Overview
Degenbot is a set of Python classes that abstract many of the implementation details of Uniswap liquidity pools and their underlying ERC-20 tokens. It uses [web3.py](https://github.com/ethereum/web3.py/) for communication with an EVM blockchain through the standard JSON-RPC interface.

These classes serve as a building blocks for the lessons published by [BowTiedDevil](https://twitter.com/BowTiedDevil) on [Degen Code](https://www.degencode.com/).

The classes originally relied on [Brownie](https://github.com/eth-brownie/brownie), but have evolved to use [web3.py](https://github.com/ethereum/web3.py/) more generally following Brownie's transition to "maintenance mode". The degenbot classes may be used within a Brownie or [Ape Framework](https://github.com/ApeWorX/ape/) console by passing a connected `Web3` object.

# License
This code is published under a permissive MIT license.

# Donation
If you find this code valuable, please fund continuing development by donating to [`0xADAf500b965545C8A766CD9Cdeb3BF3FBef073e5`](https://etherscan.io/address/0xadaf500b965545c8a766cd9cdeb3bf3fbef073e5) on any EVM compatible chain.

# Installation
There are two ways to install degenbot, both require `pip` or similar package management tool.

## From PyPI
`pip install degenbot` will fetch the latest version from [PyPI](https://pypi.org/project/degenbot/) with dependencies.

## From Source
Use `git clone` to create a local copy of this repo, then install with `pip install -e /path/to/repo`. This creates an editable installation that can be imported into a script or Python REPL using `import degenbot`.

# Examples
The following snippets assume a connected `Web3` instance with a working provider on Ethereum mainnet (chain ID #1), and the classes imported under the `degenbot` namespace.

## Uniswap V2 Liquidity Pools
```
# Create `UniswapV2Pool` object from on-chain data at the given address and 
# current chain height
>>> lp = degenbot.UniswapV2Pool('0xBb2b8038a1640196FbE3e38816F3e67Cba72D940')
• WBTC (Wrapped BTC)
• WETH (Wrapped Ether)
• Token 0: WBTC - Reserves: 10732489743
• Token 1: WETH - Reserves: 2056834999904002274711

# Inspect the tokens held by the pool
>>> lp.token0
Erc20Token(
    address=0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599, 
    symbol='WBTC', 
    name='Wrapped BTC', 
    decimals=8
)

>>> lp.token1
Erc20Token(
    address=0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2, 
    symbol='WETH', 
    name='Wrapped Ether', 
    decimals=18
)

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

# Update the current reserves from the live blockchain
>>> lp.auto_update(silent=False)
[WBTC-WETH (V2, 0.30%)]
WBTC: 10732455184
WETH: 2056841643098872755548
       
>>> lp.reserves_token0
10732455184

>>> lp.reserves_token1
2056841643098872755548
```

## Uniswap V3 Liquidity Pools
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
        bitmap=115792089237316195423570985008687907853268655437644779123584680198630541352072,
        block=18517670
        )
}

# The V3 liquidity pool helper is optimized for fast instantiation, and will lazy-load 
# liquidity data for positions outside of the current range as needed.
>>> lp.tick_data
{
    0: UniswapV3LiquidityAtTick(
        liquidityNet=10943161472679, 
        liquidityGross=10943161472679, 
        block=18517665
    ),
    261060: UniswapV3LiquidityAtTick(
        liquidityNet=-910396189679465, 
        liquidityGross=910396189679465, 
        block=18517670
    ),
    261000: UniswapV3LiquidityAtTick(
        liquidityNet=-3774266260841234, 
        liquidityGross=3774266260841234, 
        block=18517670
    ),
   
    ...

    246360: UniswapV3LiquidityAtTick(
        liquidityNet=1235001955603188, 
        liquidityGross=1235001955603188, 
        block=18517670
    ),
    246180: UniswapV3LiquidityAtTick(
        liquidityNet=4890971540, 
        liquidityGross=4890971540, 
        block=18517670
    ),
    245940: UniswapV3LiquidityAtTick(
        liquidityNet=76701235421656, 
        liquidityGross=76701235421656, 
        block=18517670
    ),
}
```

## Forking With Anvil
The `AnvilFork` class is used to launch a fork with `anvil` from the [Foundry](https://github.com/foundry-rs/foundry) toolkit. The object provides a `w3` attribute, connected to an IPC socket, which can be used to communicate with the fork like a typical RPC.

```
>>> fork = degenbot.AnvilFork(fork_url='http://localhost:8545')
>>> fork.w3.eth.chain_id
1
>>> fork.w3.eth.block_number
22675736

# The `AnvilFork` instance also exposes HTTP and WS endpoints that can be used to make a 
# separate connection from a remote machine.
>>> import web3
>>> _w3 = web3.Web3(web3.HTTPProvider(fork.http))
>>> _w3.is_connected()
True
>>> _w3 = web3.Web3(web3.LegacyWebSocketProvider(fork.ws_url))
>>> _w3.is_connected()
True

# The fork can be reset to a different endpoint, which defaults to the latest block.
>>> fork.reset(fork_url='http://localhost:8544')
>>> fork.w3.eth.chain_id
8453

# The fork can also be reset with a specified block number or a transaction hash.
>>> fork.reset(fork_url='http://localhost:8545', block_number=22_675_800)
>>> fork.w3.eth.chain_id
1
>>> fork.w3.eth.block_number
22675800

>>> fork.reset(fork_url='http://localhost:8545', block_number=22_675_800)
>>> fork.w3.eth.chain_id
1
>>> fork.w3.eth.block_number
22675800

# The fork can also be reset to an imaginary block after a specific transaction 
# hash. See the [Anvil reference](https://getfoundry.sh/anvil/reference/) for the 
# associated `--fork-transaction-hash` option.
>>> fork.reset(
    fork_url='http://localhost:8545',
    transaction_hash='0xc16e63e693a2748559c0fd653ade195be426472dddc5bfa3fcc769c4c88c249c'
)
>>> fork.w3.eth.block_number
22675814

# Blocks can be manually mined
>>> fork.mine()
>>> fork.w3.eth.block_number
22675815

# Byte code can be set for an arbitrary address.
>>> fork.set_code(
    address='0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045', 
    bytecode=bytes.fromhex('45')
)
>>> fork.w3.eth.get_code('0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045')
HexBytes('0x45')
```

### Anvil Options
The Anvil client offers [many options](https://getfoundry.sh/anvil/reference/anvil/). The most common ones are exposed by constructor options to `AnvilFork`. 

Users wanting fine-grained control over **all** client options may pass them through the `anvil_opts` argument, which takes a list of strings. These will be passed directly to the client after all of the managed options. 

```
# Launch with the Optimism feature set, which enables special transaction types.
>>> fork = degenbot.AnvilFork(
    fork_url='http://localhost:8544',
    anvil_opts=['--optimism']
)

# Launch with a non-default hardfork, which may be necessary for accurate simulation on a historical block.
>>> fork = degenbot.AnvilFork(
    fork_url='http://localhost:8545',
    fork_block=12_980_000,
    anvil_opts=['--hardfork=london']
)

# Launch with a non-default transaction pool ordering scheme
>>> fork = degenbot.AnvilFork(
    fork_url='http://localhost:8545',
    anvil_opts=['--order=fifo']
)

# Launch with certain debugging features enabled
>>> fork = degenbot.AnvilFork(
    fork_url='http://localhost:8545',
    anvil_opts=[
        '--disable-block-gas-limit',
        '--disable-code-size-limit',
        '--disable-min-priority-fee',
    ]
)
```

## Uniswap Arbitrage
Several classes are provided to simplify the calculation of optimal arbitrage amounts for a given sequence of pools.

```
>>> v2_lp = degenbot.UniswapV2Pool('0xBb2b8038a1640196FbE3e38816F3e67Cba72D940')
• WBTC (Wrapped BTC)
• WETH (Wrapped Ether)
• Token 0: WBTC - Reserves: 6390612659
• Token 1: WETH - Reserves: 2534027291379197003140

>>> v3_lp = degenbot.UniswapV3Pool('0xCBCdF9626bC03E24f779434178A73a0B4bad62eD')
WBTC-WETH (UniswapV3Pool, 0.30%)
• Address: 0xCBCdF9626bC03E24f779434178A73a0B4bad62eD           
• Token 0: WBTC           
• Token 1: WETH
• Fee: 3000               
• Liquidity: 261799575241796322         
• SqrtPrice: 49883600179466982678044042954714957         
• Tick: 267070              
• State Block (Initial): 22676748
                                                                       
>>> weth = v2_lp.token1 

>>> arb = degenbot.UniswapLpCycle(
    id="test", 
    input_token=weth, 
    swap_pools=[v2_lp, v3_lp]
)

# The minimum rate of exchange for a profitable arbitrage is 1.0. The pool states at
# a given block are likely to be less, so override the minimum for illustration.
# The `ArbitrageCalculationResult` must be encoded as a properly-formed transaction 
# by the user and broadcast to the network to secure the opportunity.
>>> arb.calculate(min_rate_of_exchange=0.8)
ArbitrageCalculationResult(
    id='test', 
    input_token=Erc20Token(
        address=0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2, 
        symbol='WETH', 
        name='Wrapped Ether', 
        decimals=18
    ), 
    profit_token=Erc20Token(
        address=0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2, 
        symbol='WETH', 
        name='Wrapped Ether', 
        decimals=18
    ), 
    input_amount=69600394635598,
    profit_amount=-623178922742, 
    swap_amounts=(
        UniswapV2PoolSwapAmounts(
            pool='0xBb2b8038a1640196FbE3e38816F3e67Cba72D940', 
            amounts_in=(0, 69600394635598), 
            amounts_out=(175, 0), 
            recipient=None
        ), 
        UniswapV3PoolSwapAmounts(
            pool='0xCBCdF9626bC03E24f779434178A73a0B4bad62eD',
            amount_specified=175,
            zero_for_one=True, 
            sqrt_price_limit_x96=4295128740, 
            recipient=None
        )
    ), 
    state_block=22676748
)
```