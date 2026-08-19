# Degenbot

A Rust MEV-bot core with a first-class Python driver shell, for Uniswap (V2, V3, V4), Curve V1, Solidly V2, Balancer V2, and Aave V3 integrations on EVM-compatible blockchains.

Degenbot has two equally first-class consumers sharing one Rust core:

- **Pure-Rust MEV bot** — `cargo add degenbot` (the umbrella crate re-exporting the cores; a git/path dependency until the workspace is published to crates.io) and build a fully functional MEV bot in Rust only.
- **Python-driven MEV bot** — drive the same Rust core from Python through a thin [PyO3](https://pyo3.rs) layer that translates Python calls into Rust calls.

The Rust core is the engine; Python is a driver shell, not a co-implementation. Pool/token state, swap math, event decoding, solvers, the pump loop, and swap encoding all live in Rust core crates; the Python layer provides the user-facing API, orchestration, and configuration. See [`docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`](docs/adr/ADR-005-polars-inspired-three-layer-architecture.md) for the architectural vision.

## Contents

- [Overview](#overview)
- [Architecture: The Python-Rust Split](#architecture-the-python-rust-split)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Supported Protocols](#supported-protocols)
- [Core Concepts](#core-concepts)
  - [I/O-Free Architecture](#io-free-architecture)
  - [The Bot Class](#the-bot-class)
  - [Pool Types and Builders](#pool-types-and-builders)
  - [External Updates](#external-updates)
- [Examples](#examples)
  - [Using the Bot Class](#using-the-bot-class-recommended)
  - [Uniswap V2 Liquidity Pools](#uniswap-v2-liquidity-pools)
  - [Uniswap V3 Liquidity Pools](#uniswap-v3-liquidity-pools)
  - [Uniswap V4 Liquidity Pools](#uniswap-v4-liquidity-pools)
  - [Forking With Anvil](#forking-with-anvil)
  - [Curve StableSwap Pools](#curve-stableswap-pools-io-free)
  - [Balancer V2 Weighted Pools](#balancer-v2-weighted-pools)
  - [Uniswap Arbitrage](#uniswap-arbitrage)
    - [Swap Encoding & On-Chain Execution](#swap-encoding--on-chain-execution)
  - [Running the Settlement-Arbitrage Bot](#running-the-settlement-arbitrage-bot)
- [Bot API Reference](#bot-api-reference)
  - [Chainlink Price Feeds](#chainlink-price-feeds)
- [CLI Reference](#cli-reference)
- [Configuration](#configuration)
- [The Rust Core](#the-rust-core-degenbot_rs-rust-crate-degenbot_ffi-python-module)
  - [Engine and Dispatch Surface](#engine-and-dispatch-surface)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)
- [Donation](#donation)

> **Debugging a failing settlement-arbitrage path, or building your own
> simulation harness?** See [`INVESTIGATIONS.md`](INVESTIGATIONS.md)** —
> the simulation oracle driver, the per-contract scaffolder, and the
> path-fixture toolkit.

## Overview

Degenbot abstracts the implementation details of Uniswap liquidity pools and their underlying ERC-20 tokens into a set of Rust core crates exposed to Python through a thin PyO3 binding layer. The Rust core owns all performance-critical and stateful logic — pool/token state, swap math, event decoding, solvers, the pump loop, and swap encoding — while the Python companion provides the user-facing API, docstrings, and I/O orchestration.

As of the 0.6.x series the Rust core also owns the operator-facing infrastructure: the settlement-arbitrage engine and pump loop, the in-process revm simulation engine, on-chain price readers, the DB-aware pool/Aave updaters, EIP-1559 transaction signing and submission, and WS/HTTP pub-sub. Python still owns the user-facing API, config, and registries, and — until the ADR-010 0.7 cutover — the SQLAlchemy ORM with its Alembic-stamped session; the Rust `degenbot-db` crate already owns the schema DDL and file operations behind it (see `degenbot database cutover` / `degenbot database heal`).

These classes serve as building blocks for the lessons published by [BowTiedDevil](https://twitter.com/BowTiedDevil) on [Degen Code](https://www.degencode.com/).

## Architecture: The Python-Rust Split

Ownership is strict, which is what makes both consumption paths first-class: the Rust core owns everything stateful and performance-critical — pool/token state, swap math, event decoding, solvers, the pump loop, and swap encoding — while the Python side owns the user-facing API, orchestration, and configuration. The core crates contain **no PyO3 code at all**, so a pure-Rust bot (`cargo add degenbot`) runs without any Python machinery; an in-repo proof is `rust/crates/degenbot/examples/standalone_consumer.rs`. Architectural decisions — state ownership (ADR-003), FFI topology (ADR-005), schema cutover (ADR-010) — are recorded in the [ADR design log](docs/adr/), with the crate sources as the last word.

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
from degenbot.config import DegenbotConfig
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI as RPC_URL
-->

```python
# Initialize Bot from config file or explicit settings
bot = degenbot.Bot(
    config=DegenbotConfig(
        default_chain_id=1,
        rpc={1: RPC_URL},
        database={"path": "~/.config/degenbot/degenbot.db"},
    )
)

# Bot constructs the RPC provider from config and enforces its
# eth_chainId matches default_chain_id (fail-fast). No manual provider
# registration is needed.
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

Pool classes are Python companions over **Rust-owned pool state** — direct construction is impossible (any constructor call raises `TypeError`); a pool comes into being only by registering in a `Bot`'s Rust state. Use `Bot.build_pool()` in production (or the `make_*_pool` test helpers in tests):

<!-- live-rpc: start "requires live RPC" -->

```python
# Do NOT do this — the constructor always raises TypeError:
try:
    degenbot.UniswapV3Pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")  # ← BROKEN!
    raise AssertionError("direct construction of a pool should be impossible")
except TypeError:
    pass

# Instead, always use Bot to construct pools (registers in Rust state,
# returns the Python companion wrapper):
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
```

<!-- live-rpc: end -->

<!-- clear-namespace -->

## Core Concepts

### I/O-Free Architecture

Degenbot pools follow an **I/O-free architecture** where on-chain data is fetched at construction time and injected into pool objects. After construction, pools are pure calculation objects with no network dependencies. For the Uniswap V2/V3/V4 families (including Aerodrome and Balancer), `Bot.build_pool()` delegates the full I/O choreography — DB lookup → RPC fetch → decode → construct → register — to the **Rust core's `PoolBuilder`**, and the Python shell only wraps the returned Rust-owned pool handle and registers the pool's ERC-20 tokens. Curve keeps a Python `CurvePoolBuilder`; token metadata goes through `Erc20Builder`. All construction/refresh I/O runs through the Rust [`BotIo`](#engine-and-dispatch-surface) seam (`degenbot._ffi.BotIo`).

**Benefits:**
- **Testability**: Easy to create test fixtures with mocked data
- **Performance**: Swap calculations are pure math, no network calls
- **Reliability**: No async complexity in pool logic
- **State Management**: Pools can be snapshotted, pickled, and restored

**Current status:** all pool types (V2, V3, V4, Aerodrome, Camelot, Balancer, Curve) are fully I/O-free — no pool class carries provider-dependent methods. For the Uniswap V2/V3/V4 families (including Aerodrome and Balancer), `Bot.build_pool()` performs the full fetch-and-register choreography and `bot.update(pool)` refreshes state from chain; Curve pools and token metadata use the remaining Python builders. Either way, state changes enter a pool only as a validated `external_update()` message — the pool itself never does I/O.

### The Bot Class

`Bot` is the central session object that owns all runtime state:

<!-- invisible-code-block: python
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI as RPC_URL
-->

```python
import degenbot
from degenbot.config import DegenbotConfig

# Bot manages connections, registries, and provides factory methods
bot = degenbot.Bot(
    config=DegenbotConfig(
        default_chain_id=1,
        rpc={1: RPC_URL},
        database={"path": ":memory:"},
    )
)
# The RPC provider is built from config; eth_chainId is enforced to equal
# default_chain_id at construction.
bot.provider  # ProviderAdapter for chain 1
bot.chain_id  # 1
```

<!-- live-rpc: start "requires live RPC" -->

```python
# All pool/token creation flows through Bot
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
token = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

# Bot provides token utilities with caching
balance = bot.get_token_balance(token, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
approval = bot.get_token_approval(token, owner="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", spender="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45")
```

<!-- live-rpc: end -->

**Bot properties:**
- `bot.chain_id` - the configured chain ID for this single-chain session
- `bot.provider` / `bot.get_provider()` - the chain's `AlloyProvider` (chain_id enforced at construction)
- `bot.pools` - PoolRegistry for created pools
- `bot.tokens` - TokenRegistry for created tokens
- `bot.managed_pools` - ManagedPoolRegistry for V4 pools
- `bot.db` - DatabaseSessionManager for state snapshots

**Lifecycle & refresh:** `Bot` is a context manager — `with degenbot.Bot(config=...) as bot:` (or an explicit, idempotent `bot.close()`) tears down the provider, the scoped DB session, and the Rust engine handles. `bot.update(pool, block_number=...)` is the canonical refresh entry point for the V2/V3/V4 families: it fetches current chain state via the Rust `BotIo` seam and pushes `pool.external_update()` (returns `True` only when state changed). `bot.release_python_state()` drops the Python-side tracker/snapshot caches once the Rust engine owns canonical state. Builders are internal to Bot and not exposed publicly. All pool/token creation goes through `Bot.build_pool()`.

### Pool Types and Builders

`build_pool(address)` is the universal entry point that auto-resolves pool type from DB, registry, and on-chain probing:

| Pool Type | Method | Supports |
|-----------|--------|----------|
| Uniswap V2 | `bot.build_pool(address)` | Standard AMM, Camelot, other forks |
| Uniswap V3 | `bot.build_pool(address)` | Full tick data, range orders |
| Uniswap V4 | `bot.build_managed_pool(address, pool_id=...)` | Singleton architecture with hooks |
| Curve V1 | `bot.build_pool(address)` | StableSwap, metapools, lending pools |

When `build_pool` is called, the pool type is auto-resolved — in order, from the pool registry, the database, and (as a last resort) on-chain probing of the pool contract.

### External Updates

Pools receive state updates via `external_update()` — a pure-logic method that validates the update and transitions pool state. I/O never touches the pool itself: for the V2/V3/V4 families `bot.update(pool)` fetches current reserves/slot0/liquidity through the Rust `BotIo` seam (Curve/Balancer refresh runs through the remaining Python builders), constructs the family's `ExternalUpdate` message, and pushes it to the pool:

<!-- invisible-code-block: python
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.erc20.erc20 import Erc20Token
from tests.helpers.v2_pool_factory import make_v2_pool
from fractions import Fraction

_wbtc = make_erc20(_BOT,
    address='0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599',
    name='Wrapped BTC',
    symbol='WBTC',
    decimals=8,
    chain_id=1,
)
_weth = make_erc20(_BOT,
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether',
    symbol='WETH',
    decimals=18,
    chain_id=1,
)
pool = make_v2_pool(
    '0xBb2b8038a1640196FbE3e38816F3e67Cba72D940',
    token0=_wbtc,
    token1=_weth,
    factory='0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f',
    fee_token0=Fraction(3, 1000),
    fee_token1=Fraction(3, 1000),
    reserves_token0=10732489743,
    reserves_token1=2056834999904002274711,
    chain_id=1,
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
| Balancer | V2 | Ethereum | Weighted & stable pools (MetaStable, Composable) |
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
from degenbot.config import DegenbotConfig

# Initialize Bot (handles config, connections, registries)
bot = degenbot.Bot(
    config=DegenbotConfig(
        default_chain_id=1,
        rpc={1: RPC_URL},
        database={"path": ":memory:"},
    )
)
```

<!-- live-rpc: start "requires live RPC" -->

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

<!-- live-rpc: end -->

---

### Uniswap V2 Liquidity Pools

V2 pools use the constant-product invariant (x·y=k) with directional fees:

<!-- invisible-code-block: python
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.erc20.erc20 import Erc20Token
from tests.helpers.v2_pool_factory import make_v2_pool
from fractions import Fraction

_wbtc = make_erc20(_BOT,
    address='0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599',
    name='Wrapped BTC',
    symbol='WBTC',
    decimals=8,
    chain_id=1,
)
_weth = make_erc20(_BOT,
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether',
    symbol='WETH',
    decimals=18,
    chain_id=1,
)
lp = make_v2_pool(
    '0xBb2b8038a1640196FbE3e38816F3e67Cba72D940',
    token0=_wbtc,
    token1=_weth,
    factory='0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f',
    fee_token0=Fraction(3, 1000),
    fee_token1=Fraction(3, 1000),
    reserves_token0=10732489743,
    reserves_token1=2056834999904002274711,
    chain_id=1,
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
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
import json
from pathlib import Path
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

_wbtc = make_erc20(_BOT,
    address='0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599',
    name='Wrapped BTC',
    symbol='WBTC',
    decimals=8,
    chain_id=1,
)
_weth = make_erc20(_BOT,
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
from tests.helpers.v3_pool_factory import make_v3_pool
lp = make_v3_pool(
    '0xCBCdF9626bC03E24f779434178A73a0B4bad62eD',
    token0=_wbtc,
    token1=_weth,
    factory='0x1F98431c8aD98523631AE4a59f267346ea31F984',
    fee=3000,
    tick_spacing=60,
    sqrt_price_x96=34048891009198980752047510166697902,
    tick=259432,
    liquidity=544425151051415575,
    state_block=24947230,
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
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool, UniswapV4PoolKey
from degenbot.erc20.erc20 import Erc20Token

_eth = make_erc20(_BOT,
    address='0x0000000000000000000000000000000000000000',
    name='Ether',
    symbol='ETH',
    decimals=18,
    chain_id=8453,
)
_usdc = make_erc20(_BOT,
    address='0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913',
    name='USD Coin',
    symbol='USDC',
    decimals=6,
    chain_id=8453,
)
from tests.helpers.v4_pool_factory import make_v4_pool
lp = make_v4_pool(
    pool_id='0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a',
    pool_manager_address='0x498581fF718922c3f8e6A244956aF099B2652b2b',
    token0=_eth,
    token1=_usdc,
    fee=500,
    tick_spacing=10,
    state_view_address='0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71',
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

The `AnvilFork` class is used to launch a fork with `anvil` from the [Foundry](https://github.com/foundry-rs/foundry) toolkit. The fork subprocess itself is spawned and driven by the Rust core crate `degenbot-fork` (`degenbot._ffi.AnvilFork`); the Python class is a thin companion over it. The object provides a `provider` attribute — an `AlloyProvider` — which can be used to communicate with the fork like any typical RPC client.

<!-- skip: start "requires running anvil process" -->

```python
>>> fork = degenbot.AnvilFork(fork_url='http://localhost:8545')
>>> fork.provider.chain_id
1
>>> fork.provider.block_number
22675736

# The `AnvilFork` instance also exposes HTTP and WS endpoints that can be used to make a
# separate connection from a remote machine.
>>> from degenbot.provider import AlloyProvider
>>> _prov = AlloyProvider(fork.http_url)
>>> _prov.is_connected()
True

# The fork can be reset to a specific block (defaults to the latest block).
>>> fork.reset(block_number=22_675_800)
>>> fork.provider.block_number
22675800

# A different endpoint or start block needs a NEW fork — `reset` cannot retarget
# the fork URL. An "imaginary" block after a historical transaction (anvil
# `--fork-transaction-hash`, see the [Anvil reference](https://getfoundry.sh/anvil/reference/))
# is a constructor option:
>>> fork = degenbot.AnvilFork(
    fork_url='http://localhost:8545',
    fork_transaction_hash='0xc16e63e693a2748559c0fd653ade195be426472dddc5bfa3fcc769c4c88c249c',
)

# Blocks can be manually mined
>>> fork.mine()

# Byte code can be set for an arbitrary address.
>>> fork.set_code(
    address='0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045',
    code=bytes.fromhex('45')
)
>>> fork.provider.get_code('0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045')
b'\x45'
```

#### Anvil Options

The Anvil client offers [many options](https://getfoundry.sh/anvil/reference/anvil/); the most common ones are exposed as `AnvilFork` constructor options. For fine-grained control, pass any raw anvil flag through the `anvil_opts` argument (a list of strings, e.g. `anvil_opts=['--optimism']` or `anvil_opts=['--hardfork=london']`) — they are appended after all managed options.

<!-- skip: end -->

### Curve StableSwap Pools (I/O-Free)

Curve pools follow the I/O-free architecture with a single `CurveDataProvider` seam. The Bot handles metapool detection, lending token identification, and data provider injection:

<!-- invisible-code-block: python
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20.erc20 import Erc20Token

_dai = make_erc20(_BOT,
    address='0x6B175474E89094C44Da98b954EedeAC495271d0F',
    name='Dai Stablecoin',
    symbol='DAI',
    decimals=18,
    chain_id=1,
)
_usdc = make_erc20(_BOT,
    address='0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48',
    name='USD Coin',
    symbol='USDC',
    decimals=6,
    chain_id=1,
)
_usdt = make_erc20(_BOT,
    address='0xdAC17F958D2ee523a2206206994597C13D831ec7',
    name='Tether USD',
    symbol='USDT',
    decimals=6,
    chain_id=1,
)
_3crv = make_erc20(_BOT,
    address='0x6c3F90f043a72FA6529E0151d6e9a6e37df9E3e5',
    name='Curve 3Pool Token',
    symbol='3Crv',
    decimals=18,
    chain_id=1,
)
from tests.helpers.curve_pool_factory import make_curve_pool
tripool = make_curve_pool(
    '0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7',
    tokens=[_dai, _usdc, _usdt],
    lp_token=_3crv,
    a_coefficient=2000,
    fee=4000000,
    admin_fee=5000000000,
    balances=[29792690991444656395059310, 27440491064, 27440490397],
    state_block=18900000,
    precision_multipliers=[1000000000000000000, 1000000000000, 1000000000000],
)
-->

```python
# Construct an I/O-free Curve StableSwap pool
assert [t.symbol for t in tripool.tokens] == ['DAI', 'USDC', 'USDT']
assert tripool.a_coefficient == 2000
assert tripool.fee == 4000000

# For lending pools (cTokens), rates are resolved before calculation;
# get_dy() resolves all on-chain inputs upfront, then computes with pure math
```

### Balancer V2 Weighted Pools

Balancer V2 weighted pools use the weighted product invariant with configurable token weights and a singleton Vault architecture. The math libraries are ported from the [Balancer V2 Solidity monorepo](https://github.com/balancer/balancer-v2-monorepo) with exact integer-level matching against on-chain results.

<!-- invisible-code-block: python
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.balancer_pool_factory import make_balancer_weighted_pool
from fractions import Fraction

_bal = make_erc20(_BOT,
    address='0xba100000625a3754423978a60c9317c58a424e3D',
    name='Balancer',
    symbol='BAL',
    decimals=18,
    chain_id=1,
)
_weth = make_erc20(_BOT,
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether',
    symbol='WETH',
    decimals=18,
    chain_id=1,
)
# The real mainnet "80 BAL 20 WETH" WeightedPool2Tokens
# (0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56). State injected offline so
# the block below is pure math — `Bot.build_pool()` runs the same registration
# in production against live RPC + on-chain bytecode for `pow_version`.
weighted_pool = make_balancer_weighted_pool(
    address='0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56',
    pool_id=bytes.fromhex(
        '5c6ee304399dbdb9c8ef030ab642b10820db8f56000200000000000000000014'
    ),
    vault='0xBA12222222228d8Ba445958a75a0704d566BF2C8',
    tokens=[_bal, _weth],
    balances=[1_000_000 * 10**18, 4_000_000 * 10**18],
    fee=Fraction(1, 100),         # 1% swap fee
    weights=[8 * 10**17, 2 * 10**17],  # 80 BAL / 20 WETH
    pow_version=1,               # WeightedPool2Tokens → V1 (general LogExpMath path)
)
-->

```python
from degenbot.balancer.pools import BalancerV2Pool

# `weighted_pool` is the real mainnet "80 BAL 20 WETH" pool, built off-line by
# the fixture above so the math below runs without RPC. In production the same
# object comes from:
#     weighted_pool = bot.build_pool('0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56')
assert isinstance(weighted_pool, BalancerV2Pool)
assert weighted_pool.address == '0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56'
assert weighted_pool.vault == '0xBA12222222228d8Ba445958a75a0704d566BF2C8'
assert [t.symbol for t in weighted_pool.tokens] == ['BAL', 'WETH']
assert weighted_pool.fee == Fraction(1, 100)                 # 1% swap fee
assert weighted_pool.weights == (8 * 10**17, 2 * 10**17)    # 80 BAL / 20 WETH

# Swap math is pure after construction — no I/O
amount_out = weighted_pool.calculate_tokens_out_from_tokens_in(
    token_in=weighted_pool.tokens[1],   # WETH in
    token_out=weighted_pool.tokens[0],  # BAL out
    token_in_quantity=10**18,
)
assert amount_out == 61874980427000000  # ≈ 0.0619 BAL per WETH at 80/20 + 1% fee

amount_in = weighted_pool.calculate_tokens_in_from_tokens_out(
    token_in=weighted_pool.tokens[1],   # WETH in
    token_out=weighted_pool.tokens[0],  # BAL out
    token_out_quantity=100 * 10**18,
)
assert amount_in == 1616565737428323232324
```

Contract addresses and broken pool filters are centralized in `degenbot.balancer.deployments`:

```python
from degenbot.balancer.deployments import (
    BALANCER_V2_VAULT_ADDRESS,
    BALANCERQUERIES_CONTRACT_ADDRESS,
    BROKEN_BALANCER_V2_POOLS,
)

# Canonical Vault + BalancerQueries addresses
assert BALANCER_V2_VAULT_ADDRESS == '0xBA12222222228d8Ba445958a75a0704d566BF2C8'
assert BALANCERQUERIES_CONTRACT_ADDRESS == '0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5'

# BROKEN_BALANCER_V2_POOLS is a frozenset of pools with swaps disabled on-chain.
# Filter before constructing:
broken = '0x753BD6a5bF0b14ae7e5d2877e5cD6a3398aA2AAB'  # YUME/WETH 1/99
assert broken in BROKEN_BALANCER_V2_POOLS
assert weighted_pool.address not in BROKEN_BALANCER_V2_POOLS  # 80 BAL 20 WETH is healthy
```

### Balancer V2 Stable Pools

Balancer V2 stable pools (MetaStablePool and ComposableStablePool) use the StableSwap invariant with rate caching. The math libraries are ported from deployed contracts with exact integer-level matching against on-chain results.

Two pool shapes share the same `BalancerV2StablePool` interface:

- **MetaStablePool** — a 2-token stable pool with no BPT token and near-static rates: exact swap math needs no rate provider and no extra I/O.
- **ComposableStablePool** — a multi-token stable pool that includes its own BPT token; time-varying rates (e.g., bb-a-* yield tokens) require a live `BalancerRateProvider`, and without one a swap call raises `StaleRateResult` (the approximate result is still readable on the exception).

<!-- invisible-code-block: python
from degenbot._ffi import Bot
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.balancer_pool_factory import make_balancer_stable_pool
from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.stable_pools import INVARIANT_V1, INVARIANT_V2

# --- MetaStablePool (2-token, no BPT, V2 invariant, wstETH/WETH 1.1 rate) ---
_BOT = Bot()
_wsteth = make_erc20(_BOT,
    address='0x7f39C581F595B53c5Cb19bD0b3f8dA6c935E2Ca0',
    name='Wrapped liquid staked Ether 2.0',
    symbol='wstETH',
    decimals=18,
    chain_id=1,
)
_weth = make_erc20(_BOT,
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether',
    symbol='WETH',
    decimals=18,
    chain_id=1,
)
meta_pool = make_balancer_stable_pool(
    address='0x32296969Ef14EB0c6d29669C550D4a0449130230',
    pool_id=bytes.fromhex(
        '32296969ef14eb0c6d29669c550d4a0449130230000200000000000000000049'
    ),
    vault='0xBA12222222228d8Ba445958a75a0704d566BF2C8',
    tokens=[_wsteth, _weth],
    balances=[100_000 * 10**18, 110_000 * 10**18],
    fee=Fraction(4, 10000),         # 0.04% swap fee
    amp=50_000,                      # amp = 50 * AMP_PRECISION (1000)
    scaling_factors=[11 * 10**17, ONE],  # wstETH rate 1.1, WETH rate 1.0
    invariant_version=INVARIANT_V2,
)

# --- ComposableStablePool (3-token incl BPT, V1 invariant, static rates) ---
_BOT2 = Bot()
_bpt_addr = '0x53BC3cBa3832ebeCBFa002c12023F8ab1AA3a3a0'
_tusd = make_erc20(_BOT2,
    address='0xdAC17F958D2ee523a2206206994597C13D831ec7',
    name='TrueUSD',
    symbol='TUSD',
    decimals=18,
    chain_id=1,
)
_bpt = make_erc20(_BOT2,
    address=_bpt_addr,
    name='Balancer 50TUSD 50USDC',
    symbol='50TUSD50USDC',
    decimals=18,
    chain_id=1,
)
_usdc = make_erc20(_BOT2,
    address='0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48',
    name='USD Coin',
    symbol='USDC',
    decimals=6,
    chain_id=1,
)
# pool_id = 20-byte address + 2-byte specialization (0x0002) + 10-byte misc
comp_pool_id = bytes.fromhex(_bpt_addr[2:].lower() + '0002' + '0' * 18 + '37')
comp_pool = make_balancer_stable_pool(
    address=_bpt_addr,
    pool_id=comp_pool_id,
    vault='0xBA12222222228d8Ba445958a75a0704d566BF2C8',
    tokens=[_tusd, _bpt, _usdc],
    balances=[100_000 * 10**18, 10_000_000 * 10**18, 200_000 * 10**6],
    fee=Fraction(3, 10000),         # 0.03% swap fee
    amp=600_000,
    scaling_factors=[ONE, ONE, ONE * 10**(18 - 6)],  # base scaling from decimals
    bpt_idx=1,                       # BPT sits at index 1
    invariant_version=INVARIANT_V1,
)
-->

```python
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.exceptions.pool import StaleRateResult

# Both pools were built off-line by the fixture above so the math runs
# without RPC; production obtains the same objects via `bot.build_pool(address)`.

# MetaStablePool: 2-token, no BPT, near-static rates — exact swap math
# needs no rate provider and no live RPC.
assert isinstance(meta_pool, BalancerV2StablePool)
assert [t.symbol for t in meta_pool.tokens] == ['wstETH', 'WETH']
assert meta_pool.fee == Fraction(4, 10000)        # 0.04%
assert meta_pool.amp == 50_000

amount_out = meta_pool.calculate_tokens_out_from_tokens_in(
    token_in=meta_pool.tokens[1],   # WETH in
    token_out=meta_pool.tokens[0],  # wstETH out
    token_in_quantity=10**18,
)
assert amount_out == 908727110808623404  # ≈ 0.9087 wstETH per WETH @ 1.1 rate

# ComposableStablePool: time-varying rates require a live rate provider;
# without one the call raises StaleRateResult (the approximate result is
# still readable on the exception).
assert isinstance(comp_pool, BalancerV2StablePool)
assert [t.symbol for t in comp_pool.tokens] == ['TUSD', '50TUSD50USDC', 'USDC']
assert comp_pool.fee == Fraction(3, 10000)         # 0.03%

try:
    comp_pool.calculate_tokens_out_from_tokens_in(
        token_in=comp_pool.tokens[0],   # TUSD in
        token_out=comp_pool.tokens[2],  # USDC out
        token_in_quantity=10**18,
    )
except StaleRateResult as e:
    # StaleRateResult wraps the approximate result so callers can still read it
    assert e.amount_in == 10**18
    assert e.amount_out == 1001103
```

### Uniswap Arbitrage

Optimal arbitrage amounts for a cyclic pool sequence are computed by the Rust `ArbitrageEngine` (EVM-exact U512 solve), driven through `EngineRegistry` — the production solve surface that replaced both the deprecated `UniswapLpCycle` / `UniswapCurveCycle` and the since-retired Python `ArbitragePath` wrapper (ACDWOC):

<!-- invisible-code-block: python
import asyncio
import degenbot
from degenbot.config import DegenbotConfig
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI as RPC_URL
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.v3_pool_factory import make_v3_pool
from fractions import Fraction

# A single Bot owns the shared BotState both pools register into. Two
# USDC/WETH pools (one V2, one V3) form a valid 2-hop cyclic arb.
bot = degenbot.Bot(
    config=DegenbotConfig(
        default_chain_id=1,
        rpc={1: RPC_URL},
        database={"path": ":memory:"},
    )
)
_py = bot._py_bot
_usdc = make_erc20(_py,
    address='0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48',
    name='USD Coin', symbol='USDC', decimals=6, chain_id=1,
)
_weth = make_erc20(_py,
    address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2',
    name='Wrapped Ether', symbol='WETH', decimals=18, chain_id=1,
)
v2_pool = make_v2_pool(
    '0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc',
    token0=_usdc, token1=_weth,
    factory='0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f',
    fee_token0=Fraction(3, 1000),
    fee_token1=Fraction(3, 1000),
    reserves_token0=100_000 * 10**6,
    reserves_token1=30 * 10**18,
    chain_id=1,
    py_bot=_py,
)
v3_pool = make_v3_pool(
    '0x88e6A0c2ddd26feeb64f039a2c41296fcb3f5640',
    token0=_usdc, token1=_weth,
    factory='0x1F98431c8aD98523631AE4a59f267346ea31F984',
    fee=500, tick_spacing=10,
    sqrt_price_x96=1_500_000_000_000_000_000_000_000_000,
    tick=-210_500,
    liquidity=1_500_000_000_000_000_000,
    py_bot=_py,
)
-->

```python
from degenbot.arbitrage.engine_registry import EngineRegistry

# EngineRegistry is the one canonical entry point: it runs the pre-pump
# startup ritual (subscribe -> backfill from snapshot -> verify config) and
# registers cyclic paths against a Bot's shared BotState. The Rust engine
# owns the EVM-exact U512 solve and re-solves affected paths on each block.
registry = EngineRegistry(bot=bot)
```

<!-- invisible-code-block: python
# pools must be handed to the engine before path-building (in production the
# verify-lifecycle provider is configured by `start()`; here we set it so the
# core-owned registration lifecycle (ADR-022 D1) has a verify access). A V3
# pool not registered in the shared BotState is a no-op (no RPC); registration
# is async because the core lifecycle aspirates the verify choreography.
registry.engine.set_verify_rpc_url('http://127.0.0.1:8545')
registry.register_v2_pool(v2_pool)
asyncio.run(registry.register_v3_pool(v3_pool))
-->

```python
# Production startup ritual (Plan 102): `registry.start(node_http, node_ws)`
# runs subscribe -> snapshot -> verify-config and returns BEFORE resume();
# after attaching the result consumer, `registry.engine.resume()` is the
# single gate after which one ResultBatch per block flows.
path_id = registry.register_path(
    pools_and_zfos=[(v2_pool, True), (v3_pool, False)],
)
# The registered path is inspectable immediately (a solved snapshot of its
# hops); profitable solves surface in the next `latest_results()` batch.
solved_path = registry.engine.inspect_path(path_id)
assert solved_path["path_id"] == path_id

# The Python `BrentSolver` reference oracle and the legacy
# `tests/arbitrage/test_engine_vs_brent_parity.py` cross-validation test
# were retired alongside the f64 hop-state taxonomy (ergo 6C32UV / LMM2NB);
# the Rust `ArbitrageEngine` is now the sole solve surface and its own
# regression corpus is the oracle.
```

> **Note:** The legacy `UniswapLpCycle` / `UniswapCurveCycle`, the Python
> `ArbitragePath` wrapper, the Python `BrentSolver` reference oracle, and
> the Python `SwapAmounts` / `generate_payloads` encoding mirror have all
> been retired. The Rust `ArbitrageEngine` (driven via `EngineRegistry`) is
> the production solve surface, and on-chain calldata is produced Rust-side
> by `degenbot_executor::composers::encode_cmd_stream` /
> `dispatch_profitable`. There is no Python swap-amount encoding layer.

#### Swap Encoding & On-Chain Execution

On-chain calldata for a solved arb path is produced entirely in the Rust
core. The Python `SwapAmounts` / `generate_payloads` / `EncodedCall` mirror
was retired (epic `6Y2PBF`) once the Rust-side `dispatch_profitable_py`
seam became the sole encode/dispatch surface — there is no Python encoding
pipeline to call.

The Rust encoding flow:

1. **Resolve** — `EngineRegistry.register_path(...)` builds a `path_id`
   against the `BotState`-owned pool identities.
2. **Solve** — the Rust `ArbitrageEngine` produces `optimal_input` /
   `hop_outputs` / `consumed_inputs` for the registered path.
3. **Encode** — `degenbot_executor::composers::encode_cmd_stream` emits the
   per-hop calldata (V2 `swap()`, V3 `swap()`, V4 PoolManager `swap()`, Curve
   `exchange()`/`exchange_underlying()`), composed into the cmd-executor
   contract envelope; the Python-visible entry point is the
   `dispatch_profitable_py` PyO3 seam, which calls `encode_cmd_stream`
   directly, with V4 BalanceDelta `int128` overflow guarded Rust-side by
   `composers::fits_int128`.
4. **Submit** — the resulting `execute_calldata` is handed to the Rust
   submission layer (`degenbot-submission`: EIP-1559 signing + fee
   finalization, exposed via `degenbot._ffi.submission`'s `Dispatcher` /
   `TxSigner` and `dispatch_and_submit_py`).

`EngineRegistry` and the example bot driver consume `DispatchCandidate` /
`PyDispatchOutcome` (carrying `path_info` / `hop_outputs`) — never a Python
`SwapAmounts` object.

### Running the Settlement-Arbitrage Bot

The end-to-end settlement-arbitrage bot — the flagship Rust-core-driven workload — is a thin Python driver over the `degenbot.runner` package: the example owns only CLI parsing + SIGINT handling, while `BotRunner` owns the session and the Rust engine handshake.

```bash
# Dry run (default): solves, simulates in-process, renders profit lines — nothing is submitted
uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py \\
  --node-http "https://eth-mainnet.example.com" \\
  --node-ws "wss://eth-mainnet.example.com/ws"

# Restrict to one 3-hop permutation (overrides the driver's default path filter)
uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py --permutation V2-V3-V4

# Live mode: signs and submits real transactions (operator key via env)
uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py --live
```

Endpoints and operator keys come from `examples/mainnet.env` + the OS env: `DEGENBOT_RPC_HTTP_CHAINID_1` / `DEGENBOT_RPC_WS_CHAINID_1` (CLI `--node-http` / `--node-ws` take precedence), `OPERATOR_ADDRESS` / `OPERATOR_PRIVATE_KEY` in live mode, and optional `EXECUTOR_CONTRACT_ADDRESS` overrides. `BotRunner` sequences the startup handshake — subscribe WS → load the V3+V4 DB snapshot into core state → backfill the snapshot→WS gap in Rust → `resume()` the pump → attach the result consumer → `build_paths()` — after which the **Rust core owns the hot loop** (event decode, per-block re-solve, revm simulation, `encode_cmd_stream` encoding, fee finalization, submission) and the Python driver owns config, result rendering, and dispatch policy. With `--operator-socket PATH`, the bot also hosts an `OperatorServer` that the `degenbot path add` / `degenbot path discover` CLI commands target to steer the live path set without a restart (protocol + design in [`docs/architecture/operator-add-path-surface.md`](docs/architecture/operator-add-path-surface.md)). The current state layer is specified by [ADR-003](docs/adr/ADR-003-botcore-state-layer.md) + [ADR-005](docs/adr/ADR-005-polars-inspired-three-layer-architecture.md); [docs/architecture/rust-owned-bot.md](docs/architecture/rust-owned-bot.md) is the original (Plans 079–082) design, kept for history.

## Bot API Reference

The `Bot` class is the primary entry point for degenbot usage. Access factories, registries, and utilities through Bot.

### Initialization

```python
import degenbot
from degenbot.config import DegenbotConfig

# With explicit config
bot = degenbot.Bot(
    config=DegenbotConfig(
        default_chain_id=1,
        rpc={
            1: RPC_URL,
        },
        database={"path": "~/.config/degenbot/degenbot.db"},
    )
)
# The RPC provider is built from the config and its eth_chainId is enforced
# to equal default_chain_id at construction — no manual registration needed.
```

### Universal Pool Builder

<!-- live-rpc: start "requires live RPC" -->

```python
# Universal builder — auto-resolves pool type from DB, registry, or on-chain probing
pool = bot.build_pool(
    "0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8",
    state_block=18900000,  # Optional, defaults to current block
)
```

<!-- live-rpc: end -->

<!-- skip: start "requires Base chain RPC node" -->

```python
# For V4 pools, use build_managed_pool with the PoolManager address + pool_id
pool = bot.build_managed_pool(
    "0x...",  # PoolManager address
    pool_id="0x...",
)
```

<!-- skip: end -->

### Pool Construction by Type

<!-- live-rpc: start "requires live RPC" -->

```python
# V2 pool (auto-detected from factory)
pool = bot.build_pool(
    "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
    state_block=18900000,  # Optional, defaults to current block
)

# V3 pool (auto-detected from factory)
pool = bot.build_pool(
    "0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8",
)

# Curve pool (auto-detected from on-chain probing)
pool = bot.build_pool(
    "0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7",
)
```

<!-- live-rpc: end -->

<!-- skip: start "requires Base chain RPC node" -->

```python
# V4 pool (singleton architecture with pool_id)
pool = bot.build_managed_pool(
    "0x...",  # PoolManager address
    pool_id="0x...",
    state_view_address="0x...",
    tokens=["0x...", "0x..."],
    fee=500,
    tick_spacing=10,
)
```

<!-- skip: end -->

### Token Factory

<!-- live-rpc: start "requires live RPC" -->

```python
# ERC-20 token (fetches name, symbol, decimals from DB/RPC)
token = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

# Token lookup (from registry if cache hit)
token = bot.get_token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
```

<!-- live-rpc: end -->

### Token Utilities (With Caching)

<!-- live-rpc: start "requires live RPC" -->

```python
# Get balance at block (cached per-bot)
balance = bot.get_token_balance(token, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
balance_at_block = bot.get_token_balance(token, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", block_identifier=10000000)

# Get approval amount (cached)
approval = bot.get_token_approval(token, owner="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", spender="0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45")

# Get total supply (cached)
total_supply = bot.get_token_total_supply(token)

# Get native ETH balance
eth_balance = bot.get_ether_balance(address="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
```

<!-- live-rpc: end -->

### Accessing Bot Components

```python
# RPC provider (built from config; chain_id enforced at construction)
provider = bot.provider

# Registries (check if already created)
existing_pool = bot.pools.get(chain_id=1, pool_address="0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
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

# Reset database (creates fresh schema; hidden command, --force skips the prompt)
degenbot database reset --force

# Upgrade database schema to the latest Alembic revision
degenbot database upgrade [--force]

# Compact database to reclaim space
degenbot database compact

# Inspect schema ownership (Alembic vs Rust) + preview the cutover
degenbot database cutover --dry-run

# One-way cutover from Alembic to Rust schema ownership (ADR-010)
degenbot database cutover [--force]

# Out-of-place heal: rebuild a stale Alembic DB at the Rust head schema (ADR-011)
degenbot database heal [--dry-run]
```

#### Pool State Management

```bash
# Update pool metadata and liquidity positions for all active exchanges
# (--verify-chunk/--verify-all add pre-commit on-chain-truth gates: a
# divergence rolls the chunk back and does NOT advance last_update_block)
degenbot pool update [--chunk SIZE] [--to-block BLOCK] [--verify-chunk/--no-verify-chunk] [--verify-all/--no-verify-all]

# Verify one V3/V4 pool's DB state against on-chain truth at a given block
degenbot pool verify --rpc-url URL --chain 1 --block 18900000 --pool 0x... --family v3|v4 [--pool-manager 0x...]

# Activate an exchange for tracking
degenbot exchange activate base_uniswap_v3

# Deactivate an exchange
degenbot exchange deactivate base_uniswap_v3

# Steer a running bot (started with --operator-socket) without restarting it
degenbot path add --socket /path/to/operator.sock --hop V2:0xPoolAddr [--hop V3:0xPoolAddr] [--direction zfo|ozf]
degenbot path discover --socket /path/to/operator.sock [--bound N]
```

**Supported exchanges:**
- Base: `base_aerodrome_v2`, `base_aerodrome_v3`, `base_pancakeswap_v2`, `base_pancakeswap_v3`, `base_sushiswap_v2`, `base_sushiswap_v3`, `base_swapbased_v2`, `base_uniswap_v2`, `base_uniswap_v3`, `base_uniswap_v4`
- Ethereum: `ethereum_pancakeswap_v2`, `ethereum_pancakeswap_v3`, `ethereum_sushiswap_v2`, `ethereum_sushiswap_v3`, `ethereum_uniswap_v2`, `ethereum_uniswap_v3`, `ethereum_uniswap_v4`

#### Aave State Management

```bash
# Update Aave V3 positions for all active markets
degenbot aave update [--chunk SIZE] [--to-block BLOCK] [--verify-chunk/--no-verify-chunk] [--dry-run]

# Activate an Aave market
degenbot aave activate ethereum_aave_v3

# Deactivate an Aave market
degenbot aave deactivate ethereum_aave_v3

# Show a user's position in a market
degenbot aave position show <ADDRESS> [--market MARKET] [--chain-id CHAIN_ID]

# Scan all users in a market for liquidation risk (market-wide; no single address)
degenbot aave position risk [--market MARKET] [--chain-id CHAIN_ID] [--threshold 1.1] [--limit N] [--show-positions]

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
| `DEGENBOT_RPC_HTTP_CHAINID_<ID>` | any HTTP(S) URL | HTTP RPC endpoint for chain `<ID>`; overrides `config.toml` `[rpc]` |
| `DEGENBOT_RPC_WS_CHAINID_<ID>` | any WS(S) URL | WebSocket endpoint for chain `<ID>`; overrides `config.toml` `[ws]` |

```bash
DEGENBOT_DEBUG=1 python my_script.py
```

### Configuration File

Degenbot uses a TOML configuration file located at `~/.config/degenbot/config.toml`:

```toml
# The chain this Bot session targets (required). One Bot per chain — see ADR-006.
# Must match a chain ID key in [rpc]; the connected RPC's eth_chainId is
# enforced to match at construction (fail-fast)
default_chain_id = 1

[rpc]
# Chain ID to HTTP RPC endpoint mapping
1 = "https://eth-mainnet.example.com"
8453 = "https://base-mainnet.example.com"

[ws]
# Chain ID to WebSocket endpoint mapping — the settlement-arbitrage pump
# subscribes to newHeads over WS, so a bot run needs this
1 = "wss://eth-mainnet.example.com/ws"
8453 = "wss://base-mainnet.example.com/ws"

[database]
# SQLite database path (optional, defaults to platform-specific location)
path = "/path/to/degenbot.db"
```

`default_chain_id` (required) selects the single chain this `Bot` targets — a
`Bot` refuses to construct without it, and the connected RPC's `eth_chainId`
is enforced to match it at construction. (Per-chain RPC/WS endpoints can also
be supplied via the `DEGENBOT_RPC_{HTTP,WS}_CHAINID_<ID>` env vars above — the
cascade is CLI flags > OS env > `config.toml`.) A `[deployments]` table may
additionally carry a user overlay on the shipped pool-type/deployment
registry (`src/degenbot/registry/deployments.json` is the single source of
truth).

## The Rust Core (`degenbot_rs` Rust crate, `degenbot._ffi` Python module)

The Rust core is the engine of degenbot — it owns all performance-critical and stateful logic. Python reaches it through the `degenbot._ffi` extension module, a thin PyO3 binding layer (`rust/crates/degenbot-python/`) that translates Python calls into Rust calls with no business logic of its own. The underlying core crates are pyo3-free by default and are consumable directly from pure Rust through the umbrella `degenbot` crate — currently via a git/path dependency (the workspace sets `publish = false`); the in-repo proof is `rust/crates/degenbot/examples/standalone_consumer.rs`, gated by `just test-standalone`.

The extension is built automatically during installation using [maturin](https://www.maturin.rs/) (or `uv sync`, which invokes maturin under the hood).

### Available Functions

#### Tick Math

Uniswap V3 tick-to-price conversions (Q96 fixed point):

```python
from degenbot.uniswap.v3_libraries import get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio

# Convert tick to sqrt price (Q96)
sqrt_price = get_sqrt_ratio_at_tick(253320)
assert sqrt_price == 25082941840919119221697001330704483

# Convert the full sqrt price back into the tick — exact round-trip
assert get_tick_at_sqrt_ratio(sqrt_price) == 253320
```

#### ABI Decoding

High-performance ABI decoding for contract data:

```python
from degenbot._ffi.abi import decode, decode_single, encode

# Encode then decode multiple values
types = ["address", "uint256", "uint256"]
data = encode(types, ["0x0000000000000000000000000000000000000001", 100, 200])
values = decode(types, data)  # Returns list of decoded values
assert values == ["0x0000000000000000000000000000000000000001", 100, 200]

# Decode a single value
address = decode_single("address", data[:32])
assert address == "0x0000000000000000000000000000000000000001"
```

#### Address Utilities

EIP-55 checksummed address conversion:

```python
from degenbot import get_checksum_address

checksummed = get_checksum_address("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
assert checksummed == "0xDeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF"
```

#### ABI Encoding & Selectors

Encode function calls and compute selectors:

```python
from degenbot.contract import encode_function_call, get_function_selector, decode_return_data
```

<!-- invisible-code-block: python
from degenbot.contract import encode_function_call, get_function_selector, decode_return_data
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
assert calldata[:4].hex() == "a9059cbb"

# Decode the same calldata body back out
values = decode_return_data(calldata[4:], ["address", "uint256"])
assert values == ["0x0000000000000000000000000000000000000001", "100"]
```

### Provider Classes

`bot.provider` is the normal way to reach chain data. The extension also exposes the raw synchronous/async RPC provider classes directly for the cases where the `Bot` conveniences don't fit:

<!-- invisible-code-block: python
from degenbot._ffi.contract import Contract
from degenbot._ffi.provider import AlloyProvider
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI as RPC_URL
-->

<!-- live-rpc: start "requires live RPC" -->

```python
# Create provider with connection pooling
provider = AlloyProvider(RPC_URL)

# Contract interaction
contract = Contract(
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
    provider_url=RPC_URL,
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

<!-- live-rpc: end -->

#### Async Provider

The extension also includes async wrappers for use with `asyncio`:

<!-- skip: start "await outside async; uses placeholder addresses" -->

```python
from degenbot._ffi.contract import AsyncContract
from degenbot._ffi.provider import AsyncAlloyProvider

# Create an async provider
async_provider = await AsyncAlloyProvider.create(
    rpc_url="https://eth-mainnet.example.com",
    max_retries=10,
    max_blocks_per_request=5000,
)

# Async contract interaction (built via `create`; `from_provider` wraps an existing provider)
async_contract = await AsyncContract.create("0x...", provider_url="https://...")
result = await async_contract.call("balanceOf(address)", ["0x..."])

# Batch multiple contract calls
results = await async_contract.batch_call(
    [("balanceOf(address)", ["0x..."]), ("totalSupply()", [])],
)
```

<!-- skip: end -->

#### Log Filtering

```python
from degenbot._ffi.provider import LogFilter

# Build a log filter
log_filter = LogFilter(
    from_block=1000000,
    to_block=1000100,
    addresses=["0x0000000000000000000000000000000000000001"],
    topics=[["0x0000000000000000000000000000000000000000000000000000000000000001"]],
)
```

`AlloyProvider` also exposes **pub-sub** — `subscribe_blocks()`, `subscribe_logs(...)`, `subscribe_pending_transactions()`, and friends return an async-iterable `AlloySubscription` (the primitive the settlement-arbitrage pump consumes) — plus **offline modes** (`AlloyProvider.offline_from_json_file(path)` / `offline_from_json_string(s)`) that answer from recorded RPC fixtures for deterministic tests, and opt-in transport-level rate limiting (`requests_per_second` + `burst` constructor args).

### Engine and Dispatch Surface

Advanced drivers can also reach the engine directly through the `degenbot._ffi` module — the settlement-arbitrage engine (`ArbitrageEngine`), the shared `Bot` state handle, plus the I/O, submission, and price seams — instead of going through the `degenbot.*` conveniences. Its type stubs (`src/degenbot/_ffi/*.pyi`) are the reference surface.

### Why Rust for the Hot Path

The MEV workload — per-block re-solve of hundreds of cyclic paths, EVM-exact revm simulation, ABI decode, tick math, and swap encoding — is latency-bound at the CPython boundary, so the pump loop runs in Rust with the GIL released around each PyO3 crossing. Per-operation microbenchmarks are not tracked in-repo.

### Build Requirements

The extension is pre-built in published packages. For source builds:

- A recent stable Rust toolchain (CI tracks `@stable`)
- maturin (installed automatically with `uv sync`)

```bash
# Build the extension (same as `just build-rust-extension`)
cargo build -p degenbot_rs --features extension-module --manifest-path rust/Cargo.toml

# Or use the justfile
just dev  # Build and install Python extension
```

## Documentation

Additional documentation is available in the [`docs/`](docs/) directory:

- **[Architecture](docs/architecture/)**: High-level architectural patterns
  - [I/O-Free Pool Architecture](docs/architecture/io-free-pools.md) — The CurveDataProvider seam for decoupled I/O
  - [Rust-Owned Settlement-Arbitrage Bot](docs/architecture/rust-owned-bot.md) — the original `ArbitrageEngine` design (Plans 079–082); marked historical, kept as a design-history reference (the current state layer follows the ADR log)
  - [Operator Add-Path Surface](docs/architecture/operator-add-path-surface.md) — steering a live bot (mid-run add-path + bounded on-demand discovery) over the Unix-socket JSON-lines operator channel
  - [Semantic Matching](docs/architecture/semantic-matching.md) — Event processing patterns for Aave
- **[Architecture Decision Records](docs/adr/)**: the 34-ADR design log for the Python→Rust migration (three-layer architecture, per-chain Bot, schema retention/cutover, registration-verify lifecycle, executor grammar, …)
- **[Execution Strategy](docs/execution-strategy.md)**: the user-owned `ExecutionStrategy` seam (ADR-025)
- **[Aave V3](docs/aave/)**: Comprehensive control flow diagrams and amount transformations for Aave operations
- **[CLI](docs/cli/)**: Detailed CLI command reference (`aave.md`, `database.md`, `pool.md`)
- **[Logging](docs/logging.md)**: Controlling `RUST_LOG` / `DEGENBOT_DEBUG` tracing, the env-gated hard/loud diagnostics, and debug-named diagnostics

### Contract Reference

Verified Solidity source code for all supported protocols is in [`contract_reference/`](contract_reference/README.md):

| Protocol | Path | Contents |
|----------|------|----------|
| Uniswap V2 | `contract_reference/uniswap/V2/` | Factory, Pair, ERC20, SafeMath, Math, UQ112x112 |
| Uniswap V3 | `contract_reference/uniswap/V3/` | Factory, Pool, Oracle, Tick, TickBitmap, SqrtPriceMath, SwapMath, TickMath, FullMath, Position, etc. |
| Uniswap V4 | `contract_reference/uniswap/V4/` | PoolManager, Pool, Hooks, TickBitmap, SqrtPriceMath, SwapMath, ProtocolFeeLibrary, LPFeeLibrary, ERC6909, etc. |
| Aave V3 | `contract_reference/aave/` | Pool (10 revisions), AToken (5 revisions), VariableDebtToken, GhoVariableDebtToken (6 revisions), GhoDiscountRateStrategy, AaveOracle, stkAAVE, RewardsController |

Useful when auditing, or when you need to understand the exact on-chain behavior of a supported protocol. See [`contract_reference/README.md`](contract_reference/README.md) for the full index.

## Contributing

Contributions are welcome! Please submit issues and pull requests to the [GitHub repository](https://github.com/BowTiedDevil/degenbot).

### Development Setup

```bash
git clone https://github.com/BowTiedDevil/degenbot.git
cd degenbot
uv sync

# Run the full gate: standalone-Rust smoke + cargo workspace + full pytest
just test

# Individual tracks:
just test-rust    # cargo workspace + just test-standalone
just test-python  # uv run pytest
```

## License

This code is published under a permissive MIT license. See [LICENSE](LICENSE) for details.

## Donation

If you find this code valuable, please fund continuing development by donating to [`0xADAf500b965545C8A766CD9Cdeb3BF3FBef073e5`](https://etherscan.io/address/0xadaf500b965545c8a766cd9cdeb3bf3fbef073e5) on any EVM compatible chain.
