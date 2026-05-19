# Degenbot

Python classes to aid rapid development of Uniswap (V2, V3, V4), Curve V1, Solidly V2, Balancer V2, and Aave V3 integrations on EVM-compatible blockchains.

## Contents

- [Overview](#overview)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Supported Protocols](#supported-protocols)
- [Core Concepts](#core-concepts)
  - [I/O-Free Architecture](#io-free-architecture)
  - [The Bot Class](#the-bot-class)
- [Examples](#examples)
  - [Using the Bot Class](#using-the-bot-class-recommended)
  - [Uniswap V2 Liquidity Pools](#uniswap-v2-liquidity-pools)
  - [Uniswap V3 Liquidity Pools](#uniswap-v3-liquidity-pools)
  - [Uniswap V4 Liquidity Pools](#uniswap-v4-liquidity-pools)
  - [Forking With Anvil](#forking-with-anvil)
  - [Curve StableSwap Pools](#curve-stableswap-pools-io-free)
  - [Uniswap Arbitrage](#uniswap-arbitrage)
  - [Chainlink Price Feeds](#chainlink-price-feeds)
- [Bot API Reference](#bot-api-reference)
- [CLI Reference](#cli-reference)
- [Configuration](#configuration)
- [Rust Extension](#rust-extension)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)
- [Donation](#donation)

## Overview

Degenbot is a set of Python classes that abstract many of the implementation details of Uniswap liquidity pools and their underlying ERC-20 tokens. It uses [web3.py](https://github.com/ethereum/web3.py/) for communication with an EVM blockchain through the standard JSON-RPC interface.

These classes serve as building blocks for the lessons published by [BowTiedDevil](https://twitter.com/BowTiedDevil) on [Degen Code](https://www.degencode.com/).

## Installation

### Requirements

- Python 3.12+
- `pip`, `uv`, or similar package management tool

### From PyPI

```bash
pip install degenbot
```

### From Source

```bash
git clone https://github.com/BowTiedDevil/degenbot.git
cd degenbot
uv sync  # or: pip install -e .
```

## Quick Start

### Modern Bot-Based Approach (Recommended)

The `Bot` class is the central session object for all degenbot operations. It manages connections, registries, and provides factory methods for creating pools and tokens:

<!-- invisible-code-block: python
import degenbot
import web3
from degenbot.config import DegenbotConfig
from degenbot.provider import ProviderAdapter
-->

```python
# Initialize Bot from config file or explicit settings
bot = degenbot.Bot(
    config=DegenbotConfig(
        rpc={1: "http://node:8545"},
        database={"path": "~/.config/degenbot/degenbot.db"},
    )
)

# Register an RPC provider
w3 = web3.Web3(web3.HTTPProvider("http://node:8545"))
provider = ProviderAdapter.from_web3(w3)
bot.connections.register_provider(provider)
bot.connections.set_default_chain(1)
```

```
# Create pools and tokens through Bot (I/O-free when possible)
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
token = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")  # WETH

# Pools are I/O-free - all data injected at construction
print(f"Pool: {pool.name}")
print(f"Token: {token}")

# Calculate swaps without any network calls
amount_out = pool.calculate_tokens_out_from_tokens_in(
    token_in=pool.token0,
    token_in_quantity=10**18,
)
print(f"Output: {amount_out}")
```

### Direct Pool Construction (Advanced)

Pool classes cannot be constructed from an address alone — all state must be provided as keyword arguments. Use `Bot.build_pool()` instead:

```python
# Do NOT do this — will raise AttributeError:
# pool = degenbot.UniswapV3Pool("0x...")  ← BROKEN!

# Instead, always use Bot to construct pools:
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
```

<!-- clear-namespace -->

## Core Concepts

### I/O-Free Architecture

Degenbot pools follow an **I/O-free architecture** where on-chain data is fetched at construction time and injected into pool objects. After construction, pools are pure calculation objects with no network dependencies. Construction is handled by typed **Builder** classes (`V2PoolBuilder`, `V3PoolBuilder`, `V4PoolBuilder`, `CurvePoolBuilder`, `Erc20Builder`) that own the full I/O choreography: DB lookup → RPC fetch → decode → construct pool → register.

**Benefits:**
- **Testability**: Easy to create test fixtures with mocked data
- **Performance**: Swap calculations are pure math, no network calls
- **Reliability**: No async complexity in pool logic
- **State Management**: Pools can be snapshotted, pickled, and restored

**Current status:** All pool types (Curve, V2, V3, V4, Aerodrome, Camelot) are fully I/O-free — no pool class imports `ProviderAdapter` or carries provider-dependent methods. Construction and updates flow through builders, and all state changes enter pools via `external_update()`. Curve calculators receive a `DyCalculationInputs` frozen dataclass carrying pre-resolved data, eliminating all private member access (no `pool._xxx` patterns).

### The Bot Class

`Bot` is the central session object that owns all runtime state:

```python
import degenbot
import web3
from degenbot.config import DegenbotConfig
from degenbot.provider import ProviderAdapter

# Bot manages connections, registries, and provides factory methods
bot = degenbot.Bot(
    config=DegenbotConfig(rpc={1: "http://node:8545"}, database={"path": ":memory:"})
)
w3 = web3.Web3(web3.HTTPProvider("http://node:8545"))
bot.connections.register_provider(ProviderAdapter.from_web3(w3))
bot.connections.set_default_chain(1)
```

```python
# All pool/token creation flows through Bot
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
token = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

# Bot provides token utilities with caching
balance = bot.get_token_balance(token, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
approval = bot.get_token_approval(token, owner="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", spender="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45")
```

**Bot properties:**
- `bot.connections` - ConnectionManager for RPC providers
- `bot.pools` - PoolRegistry for created pools
- `bot.tokens` - TokenRegistry for created tokens
- `bot.managed_pools` - ManagedPoolRegistry for V4 pools
- `bot.db` - DatabaseSessionManager for state snapshots

Builders are internal to Bot and not exposed publicly. All pool/token creation goes through `Bot.build_pool()`.

### Pool Types and Builders

`build_pool(address)` is the universal entry point that auto-resolves pool type from DB, registry, and on-chain probing:

| Pool Type | Method | Supports |
|-----------|--------|----------|
| Uniswap V2 | `bot.build_pool(address)` | Standard AMM, Camelot, other forks |
| Uniswap V3 | `bot.build_pool(address)` | Full tick data, range orders |
| Uniswap V4 | `bot.build_pool(address, pool_id=...)` | Singleton architecture with hooks |
| Curve V1 | `bot.build_pool(address)` | StableSwap, metapools, lending pools |

When `build_pool` is called, type resolution proceeds in order: (1) pool registry for existing pools, (2) database `kind` column, (3) pool type registry mapping `(chain_id, factory_address) → pool class`, (4) on-chain probing via `slot0()`, `getReserves()`, or `coins()`.

### External Updates

Pools receive state updates via `external_update()` — a pure-logic method that validates the update and transitions pool state. The builder handles all I/O (fetching reserves, slot0, etc. from RPC), constructs an `ExternalUpdate` message, and pushes it to the pool:

<!-- invisible-code-block: python
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.erc20.erc20 import Erc20Token
from fractions import Fraction

_wbtc = Erc20Token(
    address='0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599',
    name='Wrapped BTC',
    symbol='WBTC',
    decimals=8,
    chain_id=1,
)
_weth = Erc20Token(
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether',
    symbol='WETH',
    decimals=18,
    chain_id=1,
)
pool = UniswapV2Pool(
    address='0xBb2b8038a1640196FbE3e38816F3e67Cba72D940',
    chain_id=1,
    token0=_wbtc,
    token1=_weth,
    factory='0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f',
    fee_token0=Fraction(3, 1000),
    fee_token1=Fraction(3, 1000),
    reserves_token0=10732489743,
    reserves_token1=2056834999904002274711,
)
block_number = 100
reserves0 = 10732455184
reserves1 = 2056841643098872755548
-->

```python
# Builder fetches state from chain (I/O), constructs update, pushes to pool
update = UniswapV2PoolExternalUpdate(
    block_number=block_number,
    reserves_token0=reserves0,
    reserves_token1=reserves1,
)
pool.external_update(update)  # Pure logic — no I/O

# Pool.simulate_swap() previews swaps without state change
# Pool.calculate_tokens_out_from_tokens_in() is pure math after construction
```

## Supported Protocols

### DEXs (Automated Market Makers)

| Protocol | Versions | Chains |
|----------|----------|--------|
| Uniswap | V2, V3, V4 | Ethereum, Base |
| Aerodrome | V2, V3 | Base |
| PancakeSwap | V2, V3 | Ethereum, Base |
| SushiSwap | V2, V3 | Ethereum, Base |
| Curve | V1 | Ethereum |
| Solidly | V2 | Ethereum, Base | *(utility functions only, no pool class)*
| Balancer | V2 | Ethereum | *(internal, not in public API)* |
| Camelot | V2 | Arbitrum |
| SwapBased | V2 | Base |

### Lending Protocols

| Protocol | Features |
|----------|----------|
| Aave V3 | Supply, Borrow, Withdraw, Repay, Liquidation, E-Mode, GHO |

### Infrastructure

| Feature | Description |
|---------|-------------|
| Chainlink Price Feeds | Oracle price data |
| Anvil Forking | Local forked blockchain for testing |

## Examples

The following examples demonstrate the recommended **Bot-based approach** for pool and token construction.

### Using the Bot Class (Recommended)

All pool and token creation should flow through the `Bot` class for proper registry management and I/O handling:

```python
import degenbot
import web3
from degenbot.config import DegenbotConfig
from degenbot.provider import ProviderAdapter

# Initialize Bot (handles config, connections, registries)
bot = degenbot.Bot(
    config=DegenbotConfig(rpc={1: "http://node:8545"}, database={"path": ":memory:"})
)
w3 = web3.Web3(web3.HTTPProvider("http://node:8545"))
bot.connections.register_provider(ProviderAdapter.from_web3(w3))
bot.connections.set_default_chain(1)
```

```python
# Build tokens (fetches from DB/RPC, cached in registry)
weth = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
usdc = bot.build_erc20token("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")

# Build pools (fetches all state from DB/RPC, returns I/O-free pool objects)
v3_pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
v2_pool = bot.build_pool("0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc")
curve_pool = bot.build_pool("0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7")  # 3Crv

# Universal builder -- auto-resolves pool type
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")  # V3, detected automatically

# Token utilities with automatic caching
balance = bot.get_token_balance(usdc, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
approval = bot.get_token_approval(usdc, owner="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", spender="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45")

# Pools are I/O-free after construction - pure calculations
amount_out = v3_pool.calculate_tokens_out_from_tokens_in(
    token_in=v3_pool.token0,
    token_in_quantity=1000_000000,  # 1000 USDC
)
```

### Direct Pool Construction (Advanced)

Pool classes cannot be constructed from an address alone — all state must be provided as keyword arguments. Use `Bot.build_pool()` instead:

```python
# Do NOT do this — will raise AttributeError:
# pool = degenbot.UniswapV3Pool("0x...")  ← BROKEN!

# Instead, always use Bot to construct pools:
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
```

---

### Uniswap V2 Liquidity Pools

V2 pools use the constant-product invariant (x·y=k) with directional fees:

<!-- invisible-code-block: python
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.erc20.erc20 import Erc20Token
from fractions import Fraction

_wbtc = Erc20Token(
    address='0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599',
    name='Wrapped BTC',
    symbol='WBTC',
    decimals=8,
    chain_id=1,
)
_weth = Erc20Token(
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether',
    symbol='WETH',
    decimals=18,
    chain_id=1,
)
lp = UniswapV2Pool(
    address='0xBb2b8038a1640196FbE3e38816F3e67Cba72D940',
    chain_id=1,
    token0=_wbtc,
    token1=_weth,
    factory='0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f',
    fee_token0=Fraction(3, 1000),
    fee_token1=Fraction(3, 1000),
    reserves_token0=10732489743,
    reserves_token1=2056834999904002274711,
)
-->

```python
# Construct an I/O-free V2 pool (all state injected at construction)
# Tokens and reserves are provided directly — no RPC calls
assert lp.token0.symbol == 'WBTC'
assert lp.token1.symbol == 'WETH'
assert lp.reserves_token0 == 10732489743
assert lp.reserves_token1 == 2056834999904002274711

# V2 directional fees (may differ per direction)
assert lp.fee_token0 == Fraction(3, 1000)
assert lp.fee_token1 == Fraction(3, 1000)

# Calculate swap outputs - pure math, no I/O
assert lp.calculate_tokens_out_from_tokens_in(
    token_in=lp.token1,
    token_in_quantity=1*10**18
) == 5199789

assert lp.calculate_tokens_in_from_tokens_out(
    token_out=lp.token0,
    token_out_quantity=5199789
) == 999999992817074189

# Pools are I/O-free: updates flow through external_update()
# The builder (internal to Bot) fetches state and pushes updates
update = UniswapV2PoolExternalUpdate(
    block_number=100,
    reserves_token0=10732455184,
    reserves_token1=2056841643098872755548,
)
lp.external_update(update)

# Reserves are updated in-place
assert lp.reserves_token0 == 10732455184
assert lp.reserves_token1 == 2056841643098872755548
```

### Uniswap V3 Liquidity Pools

V3 pools use concentrated liquidity with tick-based positions. The V3 pool uses a **sparse tick data fetcher** for on-demand liquidity loading:

<!-- invisible-code-block: python
import json
from pathlib import Path
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

_wbtc = Erc20Token(
    address='0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599',
    name='Wrapped BTC',
    symbol='WBTC',
    decimals=8,
    chain_id=1,
)
_weth = Erc20Token(
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether',
    symbol='WETH',
    decimals=18,
    chain_id=1,
)
_data_file = Path('tests/fixtures/chain_data/1/block_24947230.json')
with _data_file.open() as _f:
    _data = json.load(_f)
_pk = 'v3_0xcbcdf9626bc03e24f779434178a73a0b4bad62ed'
_tbm_raw = _data.get(f'{_pk}_tick_bitmap', {})
_td_raw = _data.get(f'{_pk}_tick_data', {})
_tick_bitmap = {int(k): BitmapAtWord(bitmap=int(v['bitmap']), block=v['block']) for k, v in _tbm_raw.items()}
_tick_data = {int(k): LiquidityAtTick(liquidity_gross=int(v['liquidity_gross']), liquidity_net=int(v['liquidity_net']), block=v['block']) for k, v in _td_raw.items()}
lp = UniswapV3Pool(
    address='0xCBCdF9626bC03E24f779434178A73a0B4bad62eD',
    chain_id=1,
    state_block=24947230,
    token0=_wbtc,
    token1=_weth,
    factory='0x1F98431c8aD98523631AE4a59f267346ea31F984',
    fee=3000,
    tick_spacing=60,
    sqrt_price_x96=34048891009198980752047510166697902,
    tick=259432,
    liquidity=544425151051415575,
    tick_bitmap=_tick_bitmap,
    tick_data=_tick_data,
)
-->

```python
# Construct an I/O-free V3 pool (all state injected at construction)
assert lp.token0.symbol == 'WBTC'
assert lp.token1.symbol == 'WETH'
assert lp.fee == 3000
assert lp.liquidity == 544425151051415575
assert lp.sqrt_price_x96 == 34048891009198980752047510166697902
assert lp.tick == 259432

# Calculate inputs and outputs - pure math, no I/O
assert lp.calculate_tokens_out_from_tokens_in(
    token_in=lp.token1,
    token_in_quantity=1*10**18
) == 5398169

# Tick bitmap and tick data are injected at construction
assert 0 in lp.tick_bitmap
assert 0 in lp.tick_data
```

### Uniswap V4 Liquidity Pools

V4 uses a singleton pool manager with hooks. Pools are identified by `pool_id` instead of address:

<!-- invisible-code-block: python
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool, UniswapV4PoolKey
from degenbot.erc20.erc20 import Erc20Token

_eth = Erc20Token(
    address='0x0000000000000000000000000000000000000000',
    name='Ether',
    symbol='ETH',
    decimals=18,
    chain_id=8453,
)
_usdc = Erc20Token(
    address='0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913',
    name='USD Coin',
    symbol='USDC',
    decimals=6,
    chain_id=8453,
)
lp = UniswapV4Pool(
    pool_id='0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a',
    pool_manager_address='0x498581fF718922c3f8e6A244956aF099B2652b2b',
    token0=_eth,
    token1=_usdc,
    fee=500,
    tick_spacing=10,
    state_view_address='0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71',
    chain_id=8453,
    sqrt_price_x96=4220772448119892035402666,
    tick=-196812,
    liquidity=60429069420043934,
    protocol_fee_zero_for_one=0,
    protocol_fee_one_for_zero=0,
    lp_fee=500,
)
-->

```python
# Construct an I/O-free V4 pool (all state injected at construction)
assert lp.token0.symbol == 'ETH'
assert lp.token1.symbol == 'USDC'
assert lp.liquidity == 60429069420043934
assert lp.sqrt_price_x96 == 4220772448119892035402666
assert lp.tick == -196812

# V4 features: hooks, protocol fees, dynamic LP fees
assert lp.active_hooks == frozenset()
assert lp.pool_key == UniswapV4PoolKey(
    currency0='0x0000000000000000000000000000000000000000',
    currency1='0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913',
    fee=500,
    tick_spacing=10,
    hooks='0x0000000000000000000000000000000000000000',
)
```

### Forking With Anvil

The `AnvilFork` class is used to launch a fork with `anvil` from the [Foundry](https://github.com/foundry-rs/foundry) toolkit. The object provides a `w3` attribute, connected to an IPC socket, which can be used to communicate with the fork like a typical RPC.

<!-- skip: start "requires running anvil process" -->

```python
>>> fork = degenbot.AnvilFork(fork_url='http://localhost:8545')
>>> fork.w3.eth.chain_id
1
>>> fork.w3.eth.block_number
22675736

# The `AnvilFork` instance also exposes HTTP and WS endpoints that can be used to make a
# separate connection from a remote machine.
>>> import web3
>>> _w3 = web3.Web3(web3.HTTPProvider(fork.http_url))
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

#### Anvil Options

The Anvil client offers [many options](https://getfoundry.sh/anvil/reference/anvil/). The most common ones are exposed by constructor options to `AnvilFork`.

Users wanting fine-grained control over **all** client options may pass them through the `anvil_opts` argument, which takes a list of strings. These will be passed directly to the client after all of the managed options.

```python
# Launch with the Optimism feature set, which enables special transaction types.
>>> fork = degenbot.AnvilFork(
    fork_url='http://localhost:8544',
    anvil_opts=['--optimism']
)

# Launch with a non-default hardfork, which may be necessary for accurate simulation on a
# historical block.
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

<!-- skip: end -->

### Curve StableSwap Pools (I/O-Free)

Curve pools follow the I/O-free architecture with a single `CurveDataProvider` seam. The Bot handles metapool detection, lending token identification, and data provider injection:

<!-- invisible-code-block: python
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20.erc20 import Erc20Token

_dai = Erc20Token(
    address='0x6B175474E89094C44Da98b954EedeAC495271d0F',
    name='Dai Stablecoin',
    symbol='DAI',
    decimals=18,
    chain_id=1,
)
_usdc = Erc20Token(
    address='0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48',
    name='USD Coin',
    symbol='USDC',
    decimals=6,
    chain_id=1,
)
_usdt = Erc20Token(
    address='0xdAC17F958D2ee523a2206206994597C13D831ec7',
    name='Tether USD',
    symbol='USDT',
    decimals=6,
    chain_id=1,
)
_3crv = Erc20Token(
    address='0x6c3F90f043a72FA6529E0151d6e9a6e37df9E3e5',
    name='Curve 3Pool Token',
    symbol='3Crv',
    decimals=18,
    chain_id=1,
)
tripool = CurveStableswapPool(
    address='0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7',
    tokens=[_dai, _usdc, _usdt],
    lp_token=_3crv,
    a_coefficient=2000,
    fee=4000000,
    admin_fee=5000000000,
    balances=[29792690991444656395059310, 27440491064, 27440490397],
    chain_id=1,
    state_block=18900000,
    precision_multipliers=[1000000000000000000, 1000000000000, 1000000000000],
)
-->

```python
# Construct an I/O-free Curve StableSwap pool
assert [t.symbol for t in tripool.tokens] == ['DAI', 'USDC', 'USDT']
assert tripool.a_coefficient == 2000
assert tripool.fee == 4000000

# For lending pools (cTokens), rates are resolved before calculation
# Pool's get_dy() pre-resolves all I/O via CurveDataProvider, then passes
# pre-resolved data to calculators via DyCalculationInputs (pure math, no private access)
```

### Uniswap Arbitrage

Calculate optimal arbitrage amounts for a cyclic sequence of pools using `ArbitragePath`, the replacement for the deprecated `UniswapLpCycle`:

<!-- invisible-code-block: python
import json
from pathlib import Path
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from fractions import Fraction

_wbtc = Erc20Token(
    address='0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599',
    name='Wrapped BTC',
    symbol='WBTC',
    decimals=8,
    chain_id=1,
)
_weth = Erc20Token(
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether',
    symbol='WETH',
    decimals=18,
    chain_id=1,
)
v2_pool = UniswapV2Pool(
    address='0xBb2b8038a1640196FbE3e38816F3e67Cba72D940',
    chain_id=1,
    token0=_wbtc,
    token1=_weth,
    factory='0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f',
    fee_token0=Fraction(3, 1000),
    fee_token1=Fraction(3, 1000),
    reserves_token0=10732489743,
    reserves_token1=2056834999904002274711,
)
_data_file = Path('tests/fixtures/chain_data/1/block_24947230.json')
with _data_file.open() as _f:
    _data = json.load(_f)
_pk = 'v3_0xcbcdf9626bc03e24f779434178a73a0b4bad62ed'
_tbm_raw = _data.get(f'{_pk}_tick_bitmap', {})
_td_raw = _data.get(f'{_pk}_tick_data', {})
_tick_bitmap = {int(k): BitmapAtWord(bitmap=int(v['bitmap']), block=v['block']) for k, v in _tbm_raw.items()}
_tick_data = {int(k): LiquidityAtTick(liquidity_gross=int(v['liquidity_gross']), liquidity_net=int(v['liquidity_net']), block=v['block']) for k, v in _td_raw.items()}
v3_pool = UniswapV3Pool(
    address='0xCBCdF9626bC03E24f779434178A73a0B4bad62eD',
    chain_id=1,
    state_block=24947230,
    token0=_wbtc,
    token1=_weth,
    factory='0x1F98431c8aD98523631AE4a59f267346ea31F984',
    fee=3000,
    tick_spacing=60,
    sqrt_price_x96=34048891009198980752047510166697902,
    tick=259432,
    liquidity=544425151051415575,
    tick_bitmap=_tick_bitmap,
    tick_data=_tick_data,
)
-->

```python
from degenbot.arbitrage.path.arbitrage_path import ArbitragePath
from degenbot.arbitrage.optimizers.solver import ArbSolver
from degenbot.exceptions.arbitrage import OptimizationError

# Create an arbitrage path with I/O-free pools
arb_path = ArbitragePath(
    pools=[v2_pool, v3_pool],
    input_token=v2_pool.token1,  # WETH
    solver=ArbSolver(),
)

# Calculate optimal input amount (I/O-free calculation)
# When no profitable solution exists, OptimizationError is raised
try:
    result = arb_path.calculate()
    result.optimal_input  # Access the optimal input amount
    result.profit  # Access the estimated profit
except OptimizationError:
    pass  # No profitable arbitrage at current state
```

```python
# Example output at a specific block where the path was profitable
# SolveResult(
#     optimal_input=69600394635598,
#     profit=-623178922742,
#     iterations=15,
#     method=SolverMethod.PIECEWISE_MOBIUS,
#     solve_time_ns=120000,
# )
#
# arb_path.last_result.optimal_input
# 69600394635598
```

> **Note:** `UniswapLpCycle` and `UniswapCurveCycle` are deprecated. They have been moved to `degenbot.arbitrage._legacy/` and emit `DeprecationWarning` on import. Use `ArbitragePath` for all new code. See the [migration guide](docs/migration-guides/legacy-cycles-to-arbitrage-path.md) for transitioning.

#### Swap Encoding & On-Chain Execution

Each `SwapAmounts` subclass (V2, V3, V4, Curve) encodes its own per-hop calldata via `encode(recipient=)` that produces an `EncodedCall(to, data, value)`. Generic amount extraction is available via `input_amount()` / `output_amount()` methods on the base class. Pool classes implement `build_swap_amount()` from the `ArbitragePathPool` protocol, keeping per-pool swap-amount construction local. The `generate_payloads()` function wires a three-layer pipeline:

1. **Per-hop encoding** — `SwapAmounts.encode()` (pool-type-specific ABI encoding)
2. **Approval injection** — `ApprovalStrategy` protocol (default: `NoApprovals`)
3. **Call composition** — `PayloadComposer` protocol (default: `FlatComposer`)

```python
from degenbot.arbitrage.encoding import (
    generate_payloads,
    EncodedCall,
    ApprovalStrategy,
    PayloadComposer,
)
```

<!-- invisible-code-block: python
from degenbot.arbitrage.types import AbstractSwapAmounts

class _FakeSwapAmounts(AbstractSwapAmounts):
    def input_amount(self):
        return 1000
    def output_amount(self):
        return 2000
    def encode(self, *, recipient):
        return EncodedCall(
            to='0x0000000000000000000000000000000000000001',
            data=b'\x00\x01',
            value=0,
        )

swap_amounts = [_FakeSwapAmounts()]
bot_address = '0x0000000000000000000000000000000000000001'
-->

```python
# Encode swap amounts into on-chain calldata
payloads = generate_payloads(
    swap_amounts,
    recipient=bot_address,
)
# Returns list[EncodedCall] — each has .to, .data, .value
assert len(payloads) == 1
assert payloads[0].to == '0x0000000000000000000000000000000000000001'


# With a custom approval strategy (e.g., ERC-20 approvals)
class ExactApproval:
    def approvals_for(self, swap_amounts, calls):
        # Return approval calls to prepend before each swap
        return []


payloads = generate_payloads(
    swap_amounts,
    recipient=bot_address,
    approval_strategy=ExactApproval(),
)
assert len(payloads) == 1


# With a custom composer (e.g., wrapping in Multicall3)
class Multicall3Composer:
    def compose(self, calls):
        # Aggregate calls into Multicall3 format
        return calls  # placeholder


payloads = generate_payloads(
    swap_amounts,
    recipient=bot_address,
    composer=Multicall3Composer(),
)
assert len(payloads) == 1
```

**Supported pool types for encoding:**
- Uniswap V2: `swap(uint256,uint256,address,bytes)`
- Uniswap V3: `swap(address,bool,int256,uint160,bytes)`
- Curve V1: `exchange(int128,int128,uint256,uint256)` / `exchange_underlying(...)`
- Uniswap V4: requires a custom `PayloadComposer` (V4 uses an unlock/swap callback pattern). The `V4PoolKey` dataclass is available on `UniswapV4PoolSwapAmounts.pool_key` for V4 dispatch.

**Pluggable layers:**

| Layer | Protocol | Default | Purpose |
|-------|----------|---------|--------|
| Per-hop encoding | `SwapAmounts.encode()` | Pool-type-specific ABI encoding | V2 `swap()`, V3 `swap()`, Curve `exchange()` |
| Approval injection | `ApprovalStrategy` | `NoApprovals` | Add ERC-20 `approve()` calls before swaps |
| Call composition | `PayloadComposer` | `FlatComposer` | Wrap calls for target contract (Multicall3, custom executor, flash loan) |

## Bot API Reference

The `Bot` class is the primary entry point for degenbot usage. Access factories, registries, and utilities through Bot.

### Initialization

```python
import degenbot
import web3
from degenbot.config import DegenbotConfig
from degenbot.provider import ProviderAdapter

# With explicit config
bot = degenbot.Bot(
    config=DegenbotConfig(
        rpc={
            1: "http://node:8545",
        },
        database={"path": "~/.config/degenbot/degenbot.db"},
    )
)
# Register an RPC provider for chain ID 1
w3 = web3.Web3(web3.HTTPProvider("http://node:8545"))
bot.connections.register_provider(ProviderAdapter.from_web3(w3))
bot.connections.set_default_chain(1)
```

### Universal Pool Builder

```python
# Universal builder — auto-resolves pool type from DB, registry, or on-chain probing
pool = bot.build_pool(
    "0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8",
    chain_id=1,
    state_block=18900000,  # Optional, defaults to current block
)
```

<!-- skip: start "requires Base chain RPC node" -->

```python
# For V4 pools, pass pool_id for V4-specific dispatch
pool = bot.build_pool(
    "0x...",
    pool_id="0x...",
    pool_manager_address="0x...",
)
```

<!-- skip: end -->

### Pool Construction by Type

```python
# V2 pool (auto-detected from factory)
pool = bot.build_pool(
    "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
    chain_id=1,
    state_block=18900000,  # Optional, defaults to current block
)

# V3 pool (auto-detected from factory)
pool = bot.build_pool(
    "0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8",
    chain_id=1,
)

# Curve pool (auto-detected from on-chain probing)
pool = bot.build_pool(
    "0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7",
    chain_id=1,
)
```

<!-- skip: start "requires Base chain RPC node" -->

```python
# V4 pool (singleton architecture with pool_id)
pool = bot.build_pool(
    pool_id="0x...",
    pool_manager_address="0x...",
    state_view_address="0x...",
    tokens=["0x...", "0x..."],
    fee=500,
    tick_spacing=10,
)
```

<!-- skip: end -->

### Token Factory

```python
# ERC-20 token (fetches name, symbol, decimals from DB/RPC)
token = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

# Token lookup (from registry if cache hit)
token = bot.get_token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
```

### Token Utilities (With Caching)

```python
# Get balance at block (cached per-bot)
balance = bot.get_token_balance(token, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
balance_at_block = bot.get_token_balance(token, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", block_identifier=10000000)

# Get approval amount (cached)
approval = bot.get_token_approval(token, owner="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", spender="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45")

# Get total supply (cached)
total_supply = bot.get_token_total_supply(token)

# Get native ETH balance
eth_balance = bot.get_ether_balance(chain_id=1, address="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
```

### Accessing Bot Components

```python
# Connection management
provider = bot.connections.get_provider(chain_id=1)

# Registries (check if already created)
existing_pool = bot.pools.get(pool_address="0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8", chain_id=1)
existing_token = bot.tokens.get(token_address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", chain_id=1)

# Database session
with bot.db() as session:
    # SQLAlchemy operations
    pass
```

### Chainlink Price Feeds

Chainlink price feeds provide reliable oracle data for various assets. The `ChainlinkPriceContract` class simplifies access to these feeds.

<!-- invisible-code-block: python
import degenbot
price_feed = degenbot.ChainlinkPriceContract(
    '0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419',
    decimals=8,
)
-->

```python
# Load the price feed for ETH/USD
# decimals can be provided to avoid a live RPC call
assert price_feed.address == '0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419'
assert price_feed.decimals == 8

# price_feed.price requires a Bot instance with RPC access for live data
```

## CLI Reference

Degenbot provides a command-line interface for managing blockchain data and pool state.

### Installation

The CLI is installed automatically with the package:

```bash
pip install degenbot
degenbot --help
```

### Commands

#### Database Management

```bash
# Back up the database
degenbot database backup

# Reset database (creates fresh schema)
degenbot database reset

# Upgrade database schema to latest version
degenbot database upgrade [--force]

# Compact database to reclaim space
degenbot database compact
```

#### Pool State Management

```bash
# Update pool metadata and liquidity positions for all active exchanges
degenbot pool update [--chunk SIZE] [--to-block BLOCK]

# Activate an exchange for tracking
degenbot exchange activate base_uniswap_v3

# Deactivate an exchange
degenbot exchange deactivate base_uniswap_v3
```

**Supported exchanges:**
- Base: `base_aerodrome_v2`, `base_aerodrome_v3`, `base_pancakeswap_v2`, `base_pancakeswap_v3`, `base_sushiswap_v2`, `base_sushiswap_v3`, `base_swapbased_v2`, `base_uniswap_v2`, `base_uniswap_v3`, `base_uniswap_v4`
- Ethereum: `ethereum_pancakeswap_v2`, `ethereum_pancakeswap_v3`, `ethereum_sushiswap_v2`, `ethereum_sushiswap_v3`, `ethereum_uniswap_v2`, `ethereum_uniswap_v3`, `ethereum_uniswap_v4`

#### Aave State Management

```bash
# Update Aave V3 positions for all active markets
degenbot aave update [--chunk SIZE] [--to-block BLOCK]

# Activate an Aave market
degenbot aave activate ethereum_aave_v3

# Deactivate an Aave market
degenbot aave deactivate ethereum_aave_v3

# Show a user's position in a market
degenbot aave position show <ADDRESS> [--market MARKET] [--chain-id CHAIN_ID]

# Show risk parameters for a user's position
degenbot aave position risk <ADDRESS> [--market MARKET] [--chain-id CHAIN_ID]

# Show market state
degenbot aave market show [--chain-id CHAIN_ID] [--name NAME]
```

### Block Identifiers

Commands accepting `--to-block` support the following formats:

| Format | Example | Description |
|--------|---------|-------------|
| `latest` | `latest` | Latest block |
| `latest:-N` | `latest:-64` | N blocks before latest (default) |
| `safe:+N` | `safe:128` | N blocks after safe block |
| Number | `18900000` | Specific block number |

## Configuration

### Environment Variables

| Variable | Values | Description |
|----------|--------|-------------|
| `DEGENBOT_DEBUG` | `1`, `true`, `yes` | Enable debug-level logging output |
| `DEGENBOT_DEBUG_FUNCTION_CALLS` | `1`, `true`, `yes` | Enable function call trace logging |
| `DEGENBOT_COVERAGE` | `1` | Enable CLI code coverage tracking (dev use) |

```bash
DEGENBOT_DEBUG=1 python my_script.py
```

### Configuration File

Degenbot uses a TOML configuration file located at `~/.config/degenbot/config.toml`:

```toml
[rpc]
# Chain ID to RPC endpoint mapping
1 = "https://eth-mainnet.example.com"
8453 = "https://base-mainnet.example.com"

[database]
# SQLite database path (optional, defaults to platform-specific location)
path = "/path/to/degenbot.db"
```

## Rust Extension

Degenbot includes a high-performance Rust extension module (`degenbot_rs`) that provides optimized implementations of performance-critical operations. The extension is built automatically during installation using [maturin](https://www.maturin.rs/).

### Key Dependencies

| Crate | Purpose |
|-------|--------|
| [alloy](https://github.com/alloy-rs/alloy) | Ethereum primitives (Address, U256, B256), RPC types, keccak256 |
| [pyo3](https://pyo3.rs) | Python bindings with `abi3-py312` for Python 3.12+ support |
| [tokio](https://tokio.rs) | Multi-threaded async runtime for concurrent RPC calls |
| [parking_lot](https://github.com/Amanieu/parking_lot) | High-performance RwLock for thread-safe caching |
| [thiserror](https://github.com/dtolnay/thiserror) | Derivative error types |
| [serde](https://serde.rs) | Serialization/deserialization |
| [lru](https://github.com/jaemk/lru) | LRU cache implementation |

### Available Functions

#### Tick Math

Uniswap V3 tick-to-price conversions:

```python
from degenbot import get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio

# Convert tick to sqrt price (X96 format)
sqrt_price = get_sqrt_ratio_at_tick(253320)  # Returns: 56736275128821120...

# Convert sqrt price back to tick
tick = get_tick_at_sqrt_ratio(56736275128821120)  # Returns: 253320
```

#### ABI Decoding

High-performance ABI decoding for contract data:

```python
from degenbot.degenbot_rs import decode, decode_single, encode

# Encode then decode multiple values
types = ["address", "uint256", "uint256"]
data = encode(types, ["0x0000000000000000000000000000000000000001", 100, 200])
values = decode(types, data)  # Returns list of decoded values

# Decode a single value
address = decode_single("address", data[:32])
```

#### Address Utilities

EIP-55 checksummed address conversion:

```python
from degenbot import to_checksum_address

checksummed = to_checksum_address("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
# Returns: "0xDeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF"
```

#### ABI Encoding & Selectors

Encode function calls and compute selectors:

```python
from degenbot import encode_function_call, get_function_selector, decode_return_data
```

<!-- invisible-code-block: python
from degenbot import encode_function_call, get_function_selector, decode_return_data
-->

```python
# Get a 4-byte function selector
selector = get_function_selector("transfer(address,uint256)")
assert selector == "0xa9059cbb"

# Encode a function call (selector + encoded args)
calldata = encode_function_call(
    "transfer(address,uint256)",
    ["0x0000000000000000000000000000000000000001", "100"],
)

# Decode return data from a contract call
values = decode_return_data(calldata[4:], ["address", "uint256"])
```

### Provider Classes

The extension includes synchronous and async Ethereum RPC providers:

<!-- invisible-code-block: python
from degenbot.degenbot_rs import AlloyProvider, Contract
-->

```python
# Create provider with connection pooling
provider = AlloyProvider("http://node:8545")

# Contract interaction
contract = Contract(
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
    provider_url="http://node:8545",
)

# Query blockchain
block_number = provider.get_block_number()
chain_id = provider.get_chain_id()
logs = provider.get_logs(
    from_block=block_number - 10,
    to_block=block_number,
    addresses=["0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"],
)
result = contract.call(
    "balanceOf(address)",
    ["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"],
    block_number,
)

provider.close()
```

#### Async Provider

The extension also includes async wrappers for use with `asyncio`:

<!-- skip: start "await outside async; uses placeholder addresses" -->

```python
from degenbot.degenbot_rs import AsyncAlloyProvider, AsyncContract

# Create an async provider
async_provider = await AsyncAlloyProvider.create(
    rpc_url="https://eth-mainnet.example.com",
    max_connections=10,
    timeout=30.0,
)

# Async contract interaction
async_contract = AsyncContract("0x...", provider_url="https://...")
result = await async_contract.call("balanceOf(address)", ["0x..."])

# Batch multiple contract calls
results = await async_contract.batch_call(
    [("balanceOf(address)", ["0x..."]), ("totalSupply()", [])],
)
```

<!-- skip: end -->

#### Log Filtering

```python
from degenbot.degenbot_rs import LogFilter

# Build a log filter
log_filter = LogFilter(
    from_block=1000000,
    to_block=1000100,
    addresses=["0x0000000000000000000000000000000000000001"],
    topics=[["0x0000000000000000000000000000000000000000000000000000000000000001"]],
)
```

### Performance Benefits

| Operation | Pure Python | Rust Extension |
|-----------|-------------|----------------|
| Tick math | ~50μs | ~0.1μs |
| ABI decode (10 values) | ~200μs | ~5μs |
| Address checksum | ~10μs | ~0.5μs |
| Log query (1000 logs) | ~100ms | ~20ms |

### Build Requirements

The extension is pre-built in published packages. For source builds:

- Rust 1.70+ (stable toolchain)
- maturin (installed automatically with `uv sync`)

```bash
# Build the extension
cargo build --release --features extension-module --manifest-path rust/Cargo.toml

# Or use the justfile
just dev  # Build and install Python extension
```

## Documentation

Additional documentation is available in the [`docs/`](docs/) directory:

- **[Architecture](docs/architecture/)**: High-level architectural patterns
  - [I/O-Free Pool Architecture](docs/architecture/io-free-pools.md) — The CurveDataProvider seam for decoupled I/O
  - [Semantic Matching](docs/architecture/semantic-matching.md) — Event processing patterns for Aave
- **[Aave V3](docs/aave/)**: Comprehensive control flow diagrams and amount transformations for Aave operations
- **[Arbitrage](docs/arbitrage/)**: Multi-pool cycle testing documentation
- **[CLI](docs/cli/)**: Detailed CLI command reference
- **[Configuration](docs/config.md)**: Configuration options

### Domain Context Files

Each module has a `CONTEXT.md` defining domain terminology:

- [Pool Types & Trackers](src/degenbot/types/CONTEXT.md) — Pool, Pool State, Reserves, Tick, Fee representations
- [Uniswap](src/degenbot/uniswap/CONTEXT.md) — V2/V3/V4 pools, Pool Tracker, Managed Pool, Pool ID
- [Curve StableSwap](src/degenbot/curve/CONTEXT.md) — Metapools, lending pools, CurveDataProvider seam, DyCalculationInputs, DyCalculator, A coefficient
- [Aave](src/degenbot/aave/CONTEXT.md) — Market, Asset, Reserve, Enrichment, Liquidation
- [Arbitrage](src/degenbot/arbitrage/CONTEXT.md) — Arbitrage Cycle, Solver, Optimizer, Hop State
- [Registries](src/degenbot/registry/CONTEXT.md) — Pool, Token, Managed Pool registries
- [Connection](src/degenbot/connection/CONTEXT.md) — Provider management, RPC routing
- [Chainlink](src/degenbot/chainlink/CONTEXT.md) — price feeds, aggregators, round data
- [Builders](src/degenbot/builders/CONTEXT.md) — pool builders, PoolIO seam, BuilderContext, PoolBuilder/AsyncPoolBuilder protocols
- [Context Map](CONTEXT-MAP.md) — Cross-module relationships and ambiguity rulings

## Contributing

Contributions are welcome! Please submit issues and pull requests to the [GitHub repository](https://github.com/BowTiedDevil/degenbot).

### Development Setup

```bash
git clone https://github.com/BowTiedDevil/degenbot.git
cd degenbot
uv sync

# Run tests
uv run pytest
```

## License

This code is published under a permissive MIT license. See [LICENSE](LICENSE) for details.

## Donation

If you find this code valuable, please fund continuing development by donating to [`0xADAf500b965545C8A766CD9Cdeb3BF3FBef073e5`](https://etherscan.io/address/0xadaf500b965545c8a766cd9cdeb3bf3fbef073e5) on any EVM compatible chain.
