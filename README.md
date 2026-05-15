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
        database={"path": "~/.config/degenbot/degenbot.db"}
    )
)

# Register an RPC provider
w3 = web3.Web3(web3.HTTPProvider("http://node:8545"))
provider = ProviderAdapter.from_web3(w3)
bot.connections.register_provider(provider)
bot.connections.set_default_chain(1)
```

<!-- skip: start "requires live RPC node" -->

```python
# Create pools and tokens through Bot (I/O-free when possible)
pool = bot.build_v3_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
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

<!-- skip: end -->

### Direct Pool Construction (Advanced)

Pool classes cannot be constructed from an address alone — all state must be provided as keyword arguments. Use `Bot.build_pool()` or the typed `build_*` methods instead:

```python
# Do NOT do this — will raise AttributeError:
# pool = degenbot.UniswapV3Pool("0x...")  ← BROKEN!

# Instead, always use Bot to construct pools:
pool = bot.build_v3_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
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

**Current status:** All pool types (Curve, V2, V3, V4, Aerodrome, Camelot) are fully I/O-free — no pool class imports `ProviderAdapter` or carries provider-dependent methods. Construction and updates flow through builders, and all state changes enter pools via `external_update()`.

### The Bot Class

`Bot` is the central session object that owns all runtime state:

```python
import degenbot
import web3
from degenbot.config import DegenbotConfig
from degenbot.provider import ProviderAdapter

# Bot manages connections, registries, and provides factory methods
bot = degenbot.Bot(config=DegenbotConfig(rpc={1: "http://node:8545"}, database={"path": ":memory:"}))
w3 = web3.Web3(web3.HTTPProvider("http://node:8545"))
bot.connections.register_provider(ProviderAdapter.from_web3(w3))
bot.connections.set_default_chain(1)
```

<!-- skip: start "uses placeholder addresses" -->

```python
# All pool/token creation flows through Bot
pool = bot.build_v3_pool("0x...")
token = bot.build_erc20token("0x...")

# Bot provides token utilities with caching
balance = bot.get_token_balance(token, "0x...")
approval = bot.get_token_approval(token, owner="0x...", spender="0x...")
```

<!-- skip: end -->

**Bot properties:**
- `bot.connections` - ConnectionManager for RPC providers
- `bot.pools` - PoolRegistry for created pools
- `bot.tokens` - TokenRegistry for created tokens
- `bot.managed_pools` - ManagedPoolRegistry for V4 pools
- `bot.db` - DatabaseSessionManager for state snapshots

Builders are internal to Bot and not exposed publicly. All pool/token creation goes through Bot's `build_*` methods.

### Pool Types and Builders

`build_pool(address)` is the universal entry point that auto-resolves pool type from DB, registry, and on-chain probing. Typed builders are also available for callers who already know the type:

| Pool Type | Universal | Typed Builder Method | Supports |
|-----------|-----------|----------------------|----------|
| Uniswap V2 | `bot.build_pool(address)` | `bot.build_v2_pool(address)` | Standard AMM, Camelot, other forks |
| Uniswap V3 | `bot.build_pool(address)` | `bot.build_v3_pool(address)` | Full tick data, range orders |
| Uniswap V4 | `bot.build_pool(address, pool_id=...)` | `bot.build_v4_pool(pool_id=..., pool_manager_address=...)` | Singleton architecture with hooks |
| Curve V1 | `bot.build_pool(address)` | `bot.build_curve_pool(address)` | StableSwap, metapools, lending pools |

When `build_pool` is called, type resolution proceeds in order: (1) pool registry for existing pools, (2) database `kind` column, (3) pool type registry mapping `(chain_id, factory_address) → pool class`, (4) on-chain probing via `slot0()`, `getReserves()`, or `coins()`.

### External Updates

Pools receive state updates via `external_update()` — a pure-logic method that validates the update and transitions pool state. The builder handles all I/O (fetching reserves, slot0, etc. from RPC), constructs an `ExternalUpdate` message, and pushes it to the pool:

<!-- invisible-code-block: python
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
-->

<!-- skip: start "uses undefined variables from prior I/O context" -->

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

<!-- skip: end -->

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

<!-- skip: start "requires live RPC node" -->

```python
# Build tokens (fetches from DB/RPC, cached in registry)
weth = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
usdc = bot.build_erc20token("0xA0b86a33E6441e727684caC3E2B9Dd76E1Ee29c6")

# Build pools (fetches all state from DB/RPC, returns I/O-free pool objects)
v3_pool = bot.build_v3_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
v2_pool = bot.build_v2_pool("0xB4e16d92F1E0F5E4F1A5B5F5d0b9D8c7b6A5F4E3")
curve_pool = bot.build_curve_pool("0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7")  # 3Crv

# Universal builder -- auto-resolves pool type
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")  # V3, detected automatically

# Token utilities with automatic caching
balance = bot.get_token_balance(usdc, "0x...")
approval = bot.get_token_approval(usdc, owner="0x...", spender="0x...")

# Pools are I/O-free after construction - pure calculations
amount_out = v3_pool.calculate_tokens_out_from_tokens_in(
    token_in=v3_pool.token0,
    token_in_quantity=1000_000000,  # 1000 USDC
)
```

<!-- skip: end -->

### Direct Pool Construction (Advanced)

Pool classes cannot be constructed from an address alone — all state must be provided as keyword arguments. Use `Bot.build_pool()` or the typed `build_*` methods instead:

<!-- skip: next "requires live RPC node" -->

```python
# Do NOT do this — will raise AttributeError:
# pool = degenbot.UniswapV3Pool("0x...")  ← BROKEN!

# Instead, always use Bot to construct pools:
pool = bot.build_v3_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
```

---

### Uniswap V2 Liquidity Pools

V2 pools use the constant-product invariant (x·y=k) with directional fees:

<!-- skip: start "requires live RPC node; mixed output" -->

```python
# Build V2 pool via Bot (fetches reserves, tokens, fees from chain)
>>> lp = bot.build_v2_pool('0xBb2b8038a1640196FbE3e38816F3e67Cba72D940')
• WBTC (Wrapped BTC)
• WETH (Wrapped Ether)
• Token 0: WBTC - Reserves: 10732489743
• Token 1: WETH - Reserves: 2056834999904002274711

# Inspect the tokens held by the pool - already fetched by Bot
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

# V2 directional fees (may differ per direction)
>>> lp.fee_token0
Fraction(3, 1000)

>>> lp.fee_token1
Fraction(3, 1000)

# Calculate swap outputs - pure math, no I/O
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

# Pools are I/O-free: updates flow through external_update()
# The builder (internal to Bot) fetches state and pushes updates

# Reserves are updated in-place
>>> lp.reserves_token0
10732455184

>>> lp.reserves_token1
2056841643098872755548
```

<!-- skip: end -->

### Uniswap V3 Liquidity Pools

V3 pools use concentrated liquidity with tick-based positions. The V3 pool uses a **sparse tick data fetcher** for on-demand liquidity loading:

<!-- skip: start "requires live RPC node; mixed output" -->

```python
# Build V3 pool via Bot (fetches slot0, tick data around current price)
>>> lp = bot.build_v3_pool('0xCBCdF9626bC03E24f779434178A73a0B4bad62eD')
WBTC-WETH (V3, 0.30%)
• Address: 0xCBCdF9626bC03E24f779434178A73a0B4bad62eD
• Token 0: WBTC
• Token 1: WETH
• Fee: 3000
• Liquidity: 544425151051415575
• SqrtPrice: 34048891009198980752047510166697902
• Tick: 259432
• State Block (Initial): 22676748

# Calculate inputs and outputs
>>> lp.calculate_tokens_out_from_tokens_in(
    token_in=lp.token1,
    token_in_quantity=1*10**18
)
5398169

# Tick data is fetched on-demand from the chain via Bot's tick_data_fetcher
# This is injected at construction and lazily fetches missing tick data
>>> lp.tick_bitmap
{
    0: UniswapV3BitmapAtWord(bitmap=1, block=18517665),
    16: UniswapV3BitmapAtWord(bitmap=..., block=18517670)
}

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
    ...
}
```

<!-- skip: end -->

### Uniswap V4 Liquidity Pools

V4 uses a singleton pool manager with hooks. Pools are identified by `pool_id` instead of address:

<!-- skip: start "requires live RPC node; mixed output" -->

```python
# Build V4 pool via Bot (requires pool_id, manager address, state view)
>>> lp = bot.build_v4_pool(
...     pool_id='0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a',
...     pool_manager_address='0x498581fF718922c3f8e6A244956aF099B2652b2b',
...     state_view_address='0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71',
...     tokens=[
...         '0x0000000000000000000000000000000000000000',
...         '0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913',
...     ],
...     fee=500,
...     tick_spacing=10
... )
ETH-USDC (UniswapV4Pool, id=0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a)
• ID: 0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a
• Token 0: ETH
• Token 1: USDC
• Liquidity: 60429069420043934
• SqrtPrice: 4220772448119892035402666
• Tick: -196812

# Calculate output (I/O-free after construction)
>>> lp.calculate_tokens_out_from_tokens_in(
...     token_in=lp.token0,
...     token_in_quantity=1*10**18,
... )
2834164215

# V4 features: hooks, protocol fees, dynamic LP fees
>>> lp.active_hooks
frozenset()

>>> lp.pool_key
UniswapV4PoolKey(
    currency0='0x0000000000000000000000000000000000000000',
    currency1='0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913',
    fee=500,
    tick_spacing=10,
    hooks='0x0000000000000000000000000000000000000000'
)
```

<!-- skip: end -->

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

Curve pools follow the I/O-free architecture with fetcher callbacks. The Bot handles metapool detection, lending token identification, and fetcher injection:

<!-- skip: start "requires live RPC node; mixed output" -->

```python
# Build 3Crv pool (standard stableswap)
>>> tripool = bot.build_curve_pool('0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7')
3Crv Curve StableSwap Pool
• Address: 0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7
• Tokens: DAI, USDC, USDT
• A: 2000
• Fee: 4000000
• State Block: 18900000

# Build a metapool (e.g., crvUSD/USDC built on 3Crv as base)
>>> metapool = bot.build_curve_pool('0x4DEce678cfBce0e2f2CBCcF407231D5a33d97614')
crvUSD/USDC Curve Metapool
• Address: 0x4DEce678cfBce0e2f2CBCcF407231D5a33d97614
• Coins: crvUSD, 3Crv (LP token)
• Base Pool: 0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7
• A: 1000

# Calculate swaps using StableSwap invariant
>>> metapool.calculate_tokens_out_from_tokens_in(
...     token_in=crvusd,  # Erc20Token from registry
...     token_out=metapool.tokens[1],  # 3Crv LP token
...     token_in_quantity=1000 * 10**18,  # 1000 crvUSD
... )
987654321098765432109  # ~987 3Crv LP tokens

# For lending pools (cTokens), rates are fetched on-demand via the injected RateFetcher
# No I/O during calculation - pool calls fetcher when needed
```

<!-- skip: end -->

### Uniswap Arbitrage

Calculate optimal arbitrage amounts for a cyclic sequence of pools using `ArbitragePath`, the modern replacement for the deprecated `UniswapLpCycle`:

<!-- skip: start "requires live RPC node; mixed output" -->

```python
from degenbot.arbitrage.path.arbitrage_path import ArbitragePath
from degenbot.arbitrage.optimizers.solver import ArbSolver

# Build pools via Bot (I/O happens here)
>>> v2_pool = bot.build_v2_pool('0xBb2b8038a1640196FbE3e38816F3e67Cba72D940')
>>> v3_pool = bot.build_v3_pool('0xCBCdF9626bC03E24f779434178A73a0B4bad62eD')

# Create an arbitrage path (requires pools, input_token, and solver)
>>> arb_path = ArbitragePath(
...     pools=[v2_pool, v3_pool],
...     input_token=v2_pool.token1,  # WETH
...     solver=ArbSolver(),
... )

# Calculate optimal input amount (I/O-free calculation)
>>> result = arb_path.calculate()
>>> result
SolveResult(
    optimal_input=69600394635598,
    profit=-623178922742,
    iterations=15,
    method=SolverMethod.PIECEWISE_MOBIUS,
    solve_time_ns=120000,
)

# Access the last result
>>> arb_path.last_result.optimal_input
69600394635598
```

<!-- skip: end -->

> **Note:** `UniswapLpCycle` is deprecated and emits a `DeprecationWarning`. Use `ArbitragePath` for all new code.

#### Swap Encoding & On-Chain Execution

Each `SwapAmounts` subclass (V2, V3, V4, Curve) encodes its own per-hop calldata via `encode(recipient=)` that produces an `EncodedCall(to, data, value)`. The `generate_payloads()` function wires a three-layer pipeline:

1. **Per-hop encoding** — `SwapAmounts.encode()` (pool-type-specific ABI encoding)
2. **Approval injection** — `ApprovalStrategy` protocol (default: `NoApprovals`)
3. **Call composition** — `PayloadComposer` protocol (default: `FlatComposer`)

```python
from degenbot.arbitrage.encoding import generate_payloads, EncodedCall, ApprovalStrategy, PayloadComposer
```

<!-- skip: start "uses undefined variables" -->

```python
# Encode swap amounts into on-chain calldata
payloads = generate_payloads(
    swap_amounts,
    recipient=bot_address,
)
# Returns list[EncodedCall] — each has .to, .data, .value

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
```

<!-- skip: end -->

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
        database={"path": "~/.config/degenbot/degenbot.db"}
    )
)
# Register an RPC provider for chain ID 1
w3 = web3.Web3(web3.HTTPProvider("http://node:8545"))
bot.connections.register_provider(ProviderAdapter.from_web3(w3))
bot.connections.set_default_chain(1)
```

### Universal Pool Builder

<!-- skip: start "uses placeholder addresses" -->

```python
# Universal builder — auto-resolves pool type from DB, registry, or on-chain probing
pool = bot.build_pool(
    "0x...",
    chain_id=1,
    state_block=18900000,  # Optional, defaults to current block
)

# For V4 pools, pass pool_id to route to build_v4_pool
pool = bot.build_pool(
    "0x...",
    pool_id="0x...",
    pool_manager_address="0x...",
)
```

### Typed Pool Builders

```python
# V2 pool factory
pool = bot.build_v2_pool(
    "0x...", 
    chain_id=1,
    state_block=18900000,  # Optional, defaults to current block
)

# V3 pool factory
pool = bot.build_v3_pool(
    "0x...",
    chain_id=1,
    tick_bitmap=tick_bitmap,  # Optional: preload tick data
    tick_data=tick_data,
)

# V4 pool factory (singleton architecture with pool_id)
pool = bot.build_v4_pool(
    pool_id="0x...",
    pool_manager_address="0x...",
    state_view_address="0x...",
    tokens=["0x...", "0x..."],
    fee=500,
    tick_spacing=10,
)

# Curve pool factory
pool = bot.build_curve_pool(
    "0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7",
    chain_id=1,
)
```

### Token Factory

```python
# ERC-20 token (fetches name, symbol, decimals from DB/RPC)
token = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

# Token lookup (from registry if cache hit)
token = bot.get_token("0x...")
```

### Token Utilities (With Caching)

```python
# Get balance at block (cached per-bot)
balance = bot.get_token_balance(token, "0x...")
balance_at_block = bot.get_token_balance(token, "0x...", block_identifier=10000000)

# Get approval amount (cached)
approval = bot.get_token_approval(token, owner="0x...", spender="0x...")

# Get total supply (cached)
total_supply = bot.get_token_total_supply(token)

# Get native ETH balance
eth_balance = bot.get_ether_balance(chain_id=1, address="0x...")
```

### Accessing Bot Components

```python
# Connection management
provider = bot.connections.get_provider(chain_id=1)
web3 = bot.connections.get_web3(chain_id=1)

# Registries (check if already created)
existing_pool = bot.pools.get(pool_address="0x...", chain_id=1)
existing_token = bot.tokens.get(token_address="0x...", chain_id=1)

# Database session
with bot.db() as session:
    # SQLAlchemy operations
    pass
```

<!-- skip: end -->

### Chainlink Price Feeds

Chainlink price feeds provide reliable oracle data for various assets. The `ChainlinkPriceContract` class simplifies access to these feeds.

<!-- skip: start "requires live RPC node; mixed output" -->

```python
# Load the price feed for ETH/USD (requires a Bot instance for RPC access)
>>> price_feed = degenbot.ChainlinkPriceContract(
...     '0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419',
...     bot=bot,
... )

>>> price_feed.price
2836.68731709

# Check the decimals used by the price feed
>>> price_feed.decimals
8
```

<!-- skip: end -->

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

<!-- skip: start "import name may not exist in current version" -->

```python
from degenbot import decode, decode_single

# Decode multiple values
types = ["address", "uint256", "uint256"]
data = bytes.fromhex("...")
values = decode(types, data)  # Returns list of decoded values

# Decode a single value
address = decode_single("address", bytes.fromhex("..."))
```

<!-- skip: end -->

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

<!-- skip: start "uses placeholder addresses" -->

```python
# Get a 4-byte function selector
selector = get_function_selector("transfer(address,uint256)")
# Returns: "0xa9059cbb"

# Encode a function call (selector + encoded args)
calldata = encode_function_call("transfer(address,uint256)", ["0x...", "100"])

# Decode return data from a contract call
values = decode_return_data(bytes.fromhex("..."), ["uint256", "address"])
```

<!-- skip: end -->

### Provider Classes

The extension includes synchronous and async Ethereum RPC providers:

<!-- skip: start "API signature may differ; uses placeholder addresses" -->

```python
from degenbot.degenbot_rs import AlloyProvider, Contract

# Create provider with connection pooling
provider = AlloyProvider(
    rpc_url="https://eth-mainnet.example.com",
    max_connections=10,
    timeout=30.0,
    max_retries=10,
    max_blocks_per_request=5000,
)

# Query blockchain
block_number = provider.get_block_number()
chain_id = provider.get_chain_id()
logs = provider.get_logs(from_block=1000000, to_block=1000100, addresses=["0x..."])

# Contract interaction
contract = Contract("0x...", provider_url="https://...")
result = contract.call("balanceOf(address)", ["0x..."])

provider.close()
```

<!-- skip: end -->

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
    addresses=["0x..."],
    topics=[["0x..."]],
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
  - [I/O-Free Pool Architecture](docs/architecture/io-free-pools.md) — The fetcher callback pattern for decoupled I/O
  - [Semantic Matching](docs/architecture/semantic-matching.md) — Event processing patterns for Aave
- **[Aave V3](docs/aave/)**: Comprehensive control flow diagrams and amount transformations for Aave operations
- **[Arbitrage](docs/arbitrage/)**: Multi-pool cycle testing documentation
- **[CLI](docs/cli/)**: Detailed CLI command reference
- **[Configuration](docs/config.md)**: Configuration options

### Domain Context Files

Each module has a `CONTEXT.md` defining domain terminology:

- [Pool Types & Trackers](src/degenbot/types/CONTEXT.md) — Pool, Pool State, Reserves, Tick, Fee representations
- [Uniswap](src/degenbot/uniswap/CONTEXT.md) — V2/V3/V4 pools, Pool Tracker, Managed Pool, Pool ID
- [Curve StableSwap](src/degenbot/curve/CONTEXT.md) — Metapools, lending pools, fetchers, A coefficient
- [Aave](src/degenbot/aave/CONTEXT.md) — Market, Asset, Reserve, Enrichment, Liquidation
- [Arbitrage](src/degenbot/arbitrage/CONTEXT.md) — Arbitrage Cycle, Solver, Optimizer, Hop State
- [Registries](src/degenbot/registry/CONTEXT.md) — Pool, Token, Managed Pool registries
- [Connection](src/degenbot/connection/CONTEXT.md) — Provider management, RPC routing
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

# Run linting
uv run ruff check
uv run mypy
```

## License

This code is published under a permissive MIT license. See [LICENSE](LICENSE) for details.

## Donation

If you find this code valuable, please fund continuing development by donating to [`0xADAf500b965545C8A766CD9Cdeb3BF3FBef073e5`](https://etherscan.io/address/0xadaf500b965545c8a766cd9cdeb3bf3fbef073e5) on any EVM compatible chain.
