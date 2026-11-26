# Degenbot

A Rust MEV-bot core for EVM chains, with a thin Python driver shell. Supported integrations: Uniswap V2/V3/V4, Curve V1, Balancer V2, Aerodrome, PancakeSwap, SushiSwap, Camelot, SwapBased, and Aave V3.

Degenbot has two equally first-class consumers sharing one Rust core:

- **Pure-Rust MEV bot** — `cargo add degenbot` (the umbrella crate re-exporting the cores; a git/path dependency until the workspace is published to crates.io) and build a fully functional MEV bot in Rust only.
- **Python-driven MEV bot** — drive the same Rust core from Python through a thin [PyO3](https://pyo3.rs) layer that translates Python calls into Rust calls.

The Rust core is the engine; Python is a driver shell, not a co-implementation — [Architecture: The Python-Rust Split](#architecture-the-python-rust-split) states the ownership rule, and [`ADR-005`](docs/adr/ADR-005-polars-inspired-three-layer-architecture.md) records the vision behind it.

## Contents

- [Overview](#overview)
- [Architecture: The Python-Rust Split](#architecture-the-python-rust-split)
- [Installation](#installation)
  - [Requirements](#requirements)
  - [From PyPI](#from-pypi)
  - [From Source](#from-source)
- [Quick Start](#quick-start)
  - [Create a Bot Session](#create-a-bot-session)
  - [Direct Pool Construction (Advanced)](#direct-pool-construction-advanced)
- [Core Concepts](#core-concepts)
  - [I/O-Free Architecture](#io-free-architecture)
  - [The Bot Class](#the-bot-class)
  - [Pool Types and Builders](#pool-types-and-builders)
  - [External Updates](#external-updates)
- [Supported Protocols](#supported-protocols)
  - [DEXs (Automated Market Makers)](#dexs-automated-market-makers)
  - [Lending Protocols](#lending-protocols)
  - [Infrastructure](#infrastructure)
- [Examples](#examples)
  - [Uniswap V2 Liquidity Pools](#uniswap-v2-liquidity-pools)
  - [Uniswap V3 Liquidity Pools](#uniswap-v3-liquidity-pools)
  - [Uniswap V4 Liquidity Pools](#uniswap-v4-liquidity-pools)
  - [Forking With Anvil](#forking-with-anvil)
    - [Anvil Options](#anvil-options)
  - [Curve StableSwap Pools](#curve-stableswap-pools)
  - [Balancer V2 Weighted Pools](#balancer-v2-weighted-pools)
  - [Balancer V2 Stable Pools](#balancer-v2-stable-pools)
  - [Uniswap Arbitrage](#uniswap-arbitrage)
    - [Swap Encoding & On-Chain Execution](#swap-encoding--on-chain-execution)
  - [Running the Settlement-Arbitrage Bot](#running-the-settlement-arbitrage-bot)
- [Bot API Reference](#bot-api-reference)
  - [Initialization](#initialization)
  - [Universal Pool Builder](#universal-pool-builder)
  - [Pool Construction by Type](#pool-construction-by-type)
  - [Token Factory](#token-factory)
  - [Token Utilities (With Caching)](#token-utilities-with-caching)
  - [Accessing Bot Components](#accessing-bot-components)
  - [Chainlink Price Feeds](#chainlink-price-feeds)
- [CLI Reference](#cli-reference)
  - [Installing the Console](#installing-the-console)
  - [Commands](#commands)
    - [Database Management](#database-management)
    - [Pool State Management](#pool-state-management)
    - [Fleet Posture Management](#fleet-posture-management)
    - [Aave State Management](#aave-state-management)
    - [Config and Strategy Management](#config-and-strategy-management)
  - [Block Identifiers](#block-identifiers)
- [Configuration](#configuration)
  - [Configuration File](#configuration-file)
  - [Layer Precedence](#layer-precedence)
  - [Environment Variables](#environment-variables)
- [The Rust Core](#the-rust-core-degenbot_rs-rust-crate-degenbot_ffi-python-module)
  - [Available Functions](#available-functions)
    - [Tick Math](#tick-math)
    - [ABI Decoding](#abi-decoding)
    - [Address Utilities](#address-utilities)
    - [ABI Encoding & Selectors](#abi-encoding--selectors)
  - [Provider Classes](#provider-classes)
    - [Async Provider](#async-provider)
    - [Log Filtering](#log-filtering)
  - [Engine and Dispatch Surface](#engine-and-dispatch-surface)
  - [Why Rust for the Hot Path](#why-rust-for-the-hot-path)
  - [Build Requirements](#build-requirements)
  - [Rust Feature Matrix](#rust-feature-matrix)
- [Documentation](#documentation)
  - [Upstream Sources](#upstream-sources)
- [Contributing](#contributing)
  - [Development Setup](#development-setup)
- [License](#license)
- [Donation](#donation)

## Overview

> **Capability map:** every capability's Rust home, Python name, door items, and consumer probe is cataloged in [`docs/capabilities.md`](docs/capabilities.md) (ADR-084).

Degenbot is a MEV-bot toolkit for EVM chains: state, math, solvers, and the whole execution path live in Rust core crates, and Python reaches them through a thin PyO3 binding that holds no business logic of its own.

The core owns the operator-facing infrastructure too: the settlement-arbitrage engine, the in-process evm2 simulation engine, on-chain price readers, the DB-aware pool/Aave updaters, EIP-1559 transaction signing and submission, and WS/HTTP pub-sub. Python remains the user-facing API, configuration, and registry driver, while the Rust `degenbot-db` crate owns the SQLite schema, file lifecycle, and read/write operations. Python consumers use the stable typed mirror in `degenbot.db`; there is no Python SQLAlchemy session or ORM layer.

Degenbot is the toolkit behind the lessons published by [BowTiedDevil](https://twitter.com/BowTiedDevil) on [Degen Code](https://www.degencode.com/).

## Architecture: The Python-Rust Split

Ownership is strict, which is what keeps both consumption paths first-class: the Rust core owns everything stateful and performance-critical — pool/token state, swap math, event decoding, solvers, the pump loop, and swap encoding — while the Python side owns the user-facing API, orchestration, and configuration. The core crates contain **no PyO3 code at all**, so a pure-Rust bot (`cargo add degenbot`) runs without any Python machinery; an in-repo proof is `rust/crates/facade/degenbot/examples/standalone_consumer.rs`. Architectural decisions — state ownership (ADR-003), FFI topology (ADR-005), schema cutover (ADR-010) — are recorded in the [ADR design log](docs/adr/), with the crate sources as the last word.

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
just bootstrap  # locked deps, then the dev-features extension; `pip install -e .` builds the binding's default feature set (maturin's dev profile) instead
```

## Quick Start

### Create a Bot Session

The `Bot` class is the central session object for all degenbot operations. It manages connections, registries, and provides factory methods for creating pools and tokens:

<!-- invisible-code-block: python
# Executed but never shown: the live-RPC examples read historical state, so
# they need an archive-capable node, which is what the harness name resolves.
import degenbot
from tests.conftest import TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI as RPC_URL
-->

```python
# RPC_URL is any HTTP RPC endpoint for chain 1, for example
#   "https://eth-mainnet.example.com"
# Initialize Bot from explicit settings (in-memory session; a persistent
# database path is the production default)
bot = degenbot.Bot(
    chain_id=1,
    node=RPC_URL,
    database=":memory:",
)

# Bot constructs the RPC provider from the node endpoint and enforces its
# eth_chainId matches chain_id (fail-fast). No manual provider
# registration is needed.
```

```python
# Create pools and tokens through Bot
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")  # USDC/WETH 0.3%
token = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")  # WETH

print(f"Pool: {pool.name}")
print(f"Token: {token}")

# Swap math reads the registered state, so a quote costs no network call. Scale
# the quantity by the input token's decimals: token0 here is USDC (6 decimals),
# so a raw 10**18 would be a trillion-dollar sell and walk far past the
# liquidity this pool actually holds.
amount_out = pool.calculate_tokens_out_from_tokens_in(
    token_in=pool.token1,      # WETH
    token_in_quantity=10**18,
)
print(f"Output: {amount_out}")
```

### Direct Pool Construction (Advanced)

Pool classes are Python companions over **Rust-owned pool state** — direct construction is impossible (any constructor call raises `TypeError`); a pool comes into being only by registering in a `Bot`'s Rust state. Use `Bot.build_pool()` in production (or the `make_*_pool` test helpers in tests):

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

<!-- clear-namespace -->

## Core Concepts

### I/O-Free Architecture

Degenbot pools follow an **I/O-free architecture** (ADR-001): on-chain data is resolved once, up front, and registered into the pool's state, so a pool companion carries no RPC client of its own. The provider a pool keeps is a Rust trait object stored on the core state and read through a thin adapter when a calculation needs it; it is never a Python-side fetch path. `Bot.build_pool()` runs the fetch-and-register choreography core-side for every family, and the Python builders that remain are companion shells over that one entry (registry short-circuit, token companions, handle wrapper).

Construction is I/O-free for every pool type. Calculation is I/O-free for most, but not all:

| Pool family | Construction | Calculation |
|-------------|--------------|-------------|
| V2 / V3 / V4 / Aerodrome / Camelot | I/O-free | I/O-free |
| Curve STANDARD / RAW_BALANCE | I/O-free | I/O-free |
| Curve lending / crypto / live-admin / metapool | I/O-free | reads per-block `D`, `gamma`, `price_scale`, lending rates, `virtual_price` |
| Curve with A ramping | I/O-free | reads the block timestamp |
| Balancer V2 weighted / stable with a static rate provider | I/O-free | I/O-free |
| Balancer V2 ComposableStable with a live rate provider | I/O-free | reads fresh rates |

Only the Curve and Balancer companions expose `requires_io_at_calculation_time`: `True` exactly for the shapes above, `False` for their static ones. `docs/adr/ADR-001-io-free-pools.md` owns this table.

**Benefits:**
- **Testability**: Easy to create test fixtures with mocked data
- **Performance**: Swap calculations are pure math, no network calls
- **Reliability**: No async complexity in pool logic
- **State Management**: state can be snapshotted atomically (`pool.state`) for the simulation path, and rewound through the Rust reorg journal

**State changes** land in the pool's Rust-owned state, never in a Python-side copy: `bot.dispatch_log()` and `bot.dispatch_reorg_log()` decode and apply logs, and `bot.update(pool)` delegates to the core refresh entry (`pool.refresh_via`). Only the Curve and Balancer companions take an explicit `external_update()`; the V2/V3/V4 companions have no state-write method at all.

### The Bot Class

`Bot` is the central session object that owns all runtime state:

<!-- invisible-code-block: python
# Executed but never shown: the live-RPC examples read historical state, so
# they need an archive-capable node, which is what the harness name resolves.
from tests.conftest import TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI as RPC_URL
-->

```python
import degenbot

# RPC_URL is any HTTP RPC endpoint for chain 1, for example
#   "https://eth-mainnet.example.com"
# Bot manages connections, registries, and provides factory methods
bot = degenbot.Bot(
    chain_id=1,
    node=RPC_URL,
    database=":memory:",
)
# The RPC provider is built from the node endpoint; eth_chainId is enforced to
# equal chain_id at construction.
bot.provider  # the chain's AlloyProvider (chain_id enforced at construction)
bot.chain_id  # 1
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
- `bot.chain_id` - the configured chain ID for this single-chain session
- `bot.provider` / `bot.get_provider()` - the chain's `AlloyProvider` (chain_id enforced at construction)
- `bot.pools` - PoolRegistry for created pools
- `bot.tokens` - TokenRegistry for created tokens
- `bot.managed_pools` - ManagedPoolRegistry for V4 pools
- `bot.database_path` - the SQLite path used by the Rust-owned database core
- `degenbot.db` - the stable Python mirror for Rust-backed database operations and row types

**Lifecycle & refresh:** `Bot` is a context manager — `with degenbot.Bot(config=...) as bot:` (or an explicit, idempotent `bot.close()`) tears down the provider, the Rust database snapshot, and the Rust engine handles. `bot.update(pool, block_number=...)` is the canonical refresh entry point: it runs the core refresh entry (`pool.refresh_via`), which fetches current chain state and applies it atomically (returns `True` only when state changed). `bot.release_python_state()` drops the Python-side tracker/snapshot caches once the Rust engine owns canonical state. The builder objects stay internal to `Bot`; the request types they consume (`degenbot.builders.request.BuildPoolRequest` / `BuildManagedPoolRequest`) are part of the public construction surface, since a V4 build cannot name its pool without one.

### Pool Types and Builders

`build_pool(address)` is the universal entry point: it resolves the pool's type before building it.

| Pool Type | Method | Supports |
|-----------|--------|----------|
| Uniswap V2 | `bot.build_pool(address)` | Constant-product AMM, Aerodrome/Camelot (Solidly) variants, other forks |
| Uniswap V3 | `bot.build_pool(address)` | Full tick data, range orders |
| Uniswap V4 | `bot.build_managed_pool(pool_manager, BuildManagedPoolRequest(...))` | Singleton architecture with hooks |
| Curve V1 | `bot.build_pool(address)` | StableSwap, metapools, lending pools |
| Balancer V2 | `bot.build_pool(address)` | Weighted and stable (MetaStable, ComposableStable) |

`build_pool` answers an already-built pool from the session's pool registry with no I/O. Otherwise the core identity ladder resolves the pool: the persisted `pools.kind` from the database first, then the pool's `factory()` against the registered species and shipped deployment records, then positive on-chain probes. The discipline is positive-only — an identity it cannot positively establish is a typed refusal, never a guess.

### External Updates

State reaches a pool through the pump's own door, never through a Python-side write method: `bot.dispatch_log(...)` decodes a WS log and applies it to the shared Rust state, and `bot.dispatch_reorg_log(...)` drives the same log flagged `removed: true` through the reorg restore. I/O never touches the pool companion: `bot.update(pool)` hands the session's I/O façade to the core's per-family refresh entry (`pool.refresh_via`), which fetches the current reserves/slot0/liquidity/balances for every family — Curve and Balancer included — compares, and applies atomically:

<!-- invisible-code-block: python
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
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
    py_bot=_BOT,
)
block_number = 100
reserves0 = 10732455184
reserves1 = 2056841643098872755548
-->

```python
# The pump's door: a WS Sync log is decoded and applied to the shared Rust
# state — the pool class carries no write method of its own.
_BOT.dispatch_log(
    address=pool.address,
    block_number=block_number,
    topics=["0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"],
    data="0x" + (reserves0.to_bytes(32, "big") + reserves1.to_bytes(32, "big")).hex(),
)
assert pool.state.reserves_token0 == reserves0

# Pool.simulate_swap() previews swaps without state change
# Pool.calculate_tokens_out_from_tokens_in() is pure math after construction
```

## Supported Protocols

### DEXs (Automated Market Makers)

| Protocol | Versions | Chains | Notes |
|----------|----------|--------|-------|
| Uniswap | V2, V3, V4 | Ethereum, Base, Arbitrum | Arbitrum carries V3 only |
| Aerodrome | V2, V3 | Base | |
| PancakeSwap | V2, V3 | Ethereum, Base | |
| SushiSwap | V2, V3 | Ethereum, Base, Arbitrum | |
| Curve | V1 | Ethereum | StableSwap, metapools, lending pools |
| Solidly | V2 | Ethereum, Base | Math leaves only, no pool class |
| Balancer | V2 | Ethereum | Weighted and stable pools (MetaStable, ComposableStable) |
| Camelot | V2 | Arbitrum | Solidly stable math |
| SwapBased | V2 | Base | |

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

Worked examples, one section per pool family. Each is self-contained: a real mainnet pool's state is injected from the database or a recorded fixture, so the swap math runs with no RPC. Production code obtains the same objects through `Bot.build_pool()` (see [Bot API Reference](#bot-api-reference)).

### Uniswap V2 Liquidity Pools

V2 pools use the constant-product invariant (x·y=k) with directional fees:

<!-- invisible-code-block: python
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
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
    py_bot=_BOT,
)
-->

```python
# `lp` is the WBTC/WETH V2 pool; in production it comes from
# `lp = bot.build_pool('0xBb2b8038a1640196FbE3e38816F3e67Cba72D940')`.
# An off-line test fixture built the same pool here so the math below
# runs without RPC.
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

# Pools are I/O-free: state arrives through the pump's door. The log below is
# the WBTC/WETH pair's `Sync` event at block 100.
_BOT.dispatch_log(
    address=lp.address,
    block_number=100,
    topics=["0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"],
    data="0x" + (10732455184).to_bytes(32, "big").hex()
    + (2056841643098872755548).to_bytes(32, "big").hex(),
)

# Reserves are updated in-place
assert lp.reserves_token0 == 10732455184
assert lp.reserves_token1 == 2056841643098872755548
```

### Uniswap V3 Liquidity Pools

V3 pools use concentrated liquidity with tick-based positions. Tick data is seeded into the Rust tick map at construction: a tracked pool carries the complete map, while a sparse pool whose walk enters an unfetched bitmap word backfills it through the core's tick-word source.

<!-- invisible-code-block: python
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20
import json
from pathlib import Path
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.concentrated.types import LiquidityAtTick

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
_td_raw = _data.get(f'{_pk}_tick_data', {})
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
    tick_data=_tick_data,
)
-->

```python
# `lp` is the WBTC/WETH 0.3% V3 pool; in production it comes from
# `lp = bot.build_pool('0xCBCdF9626bC03E24f779434178A73a0B4bad62eD')`.
# An off-line test fixture built the same pool here so the math below
# runs without RPC.
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

V4 uses a singleton pool manager with hooks. A pool is identified by the pair (PoolManager, `pool_id`) rather than by an address of its own:

<!-- invisible-code-block: python
from degenbot._ffi import Bot
_BOT = Bot()
from tests.helpers.erc20_factory import make_erc20, make_native_token
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool, UniswapV4PoolKey
from degenbot.erc20.erc20 import Erc20Token

_eth = make_native_token(_BOT, chain_id=8453)
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
# `lp` is the ETH/USDC 0.05% V4 pool; in production it comes from
# `bot.build_managed_pool('<poolManager>', BuildManagedPoolRequest(pool_id='0x<...>'))`
# — V4 pools are identified by (pool manager, pool id), not by an address.
# An off-line test fixture built the same pool here so the math below
# runs without RPC.
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

The `AnvilFork` class is used to launch a fork with `anvil` from the [Foundry](https://github.com/foundry-rs/foundry) toolkit. The fork subprocess is spawned and driven by the Rust core; the Python `AnvilFork` is a thin companion over it. The object provides a `provider` attribute — an `AlloyProvider` — which can be used to communicate with the fork like any typical RPC client.

<!-- skip: start "requires running anvil process" -->

```python
>>> fork = degenbot.AnvilFork(fork_url='http://localhost:8545')
>>> fork.provider.get_chain_id()
1
>>> fork.provider.get_block_number()
22675736

# The `AnvilFork` instance also exposes HTTP and WS endpoints, so a separate
# connection (from a remote machine, say) can be made against them.
>>> from degenbot.provider import AlloyProvider
>>> _prov = AlloyProvider(fork.http_url)
>>> _prov.get_chain_id()
1

# The fork can be reset to a specific block (defaults to the latest block).
>>> fork.reset(block_number=22_675_800)
>>> fork.provider.get_block_number()
22675800

# `reset(fork_url=...)` / `reset(transaction_hash=...)` retarget by tearing the
# rust subprocess down and respawning it (slower than the in-place block reset
# above). An "imaginary" block after a historical transaction (anvil
# `--fork-transaction-hash`, see the [Anvil reference](https://getfoundry.sh/anvil/reference/))
# is a launch setting:
>>> from degenbot.fork import ForkLaunchConfig
>>> fork = degenbot.AnvilFork(
    fork_url='http://localhost:8545',
    launch=ForkLaunchConfig(
        fork_transaction_hash='0xc16e63e693a2748559c0fd653ade195be426472dddc5bfa3fcc769c4c88c249c',
    ),
)

# Blocks can be manually mined
>>> fork.mine()

# Byte code can be set for an arbitrary address.
>>> fork.set_code(
    address='0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045',
    bytecode=bytes.fromhex('45')
)
>>> fork.provider.get_code('0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045')
b'\x45'
```

#### Anvil Options

The Anvil client offers [many options](https://getfoundry.sh/anvil/reference/anvil/). The launch knobs live on `degenbot.fork.ForkLaunchConfig`, passed as `launch=`: mining mode and interval, storage caching, chain id, base fee, the post-spawn balance/bytecode/nonce/storage overrides, and `anvil_opts` — a raw flag list (e.g. `ForkLaunchConfig(anvil_opts=['--optimism'])` or `anvil_opts=['--hardfork=london']`) appended after all managed options.

<!-- skip: end -->

### Curve StableSwap Pools

Curve pools follow the same I/O-free architecture: the Bot resolves metapool detection and lending-token identification, and the pool reads any per-block inputs its shape needs through its data provider (see [I/O-Free Architecture](#io-free-architecture)):

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
# `tripool` is Curve's 3Crv pool; in production it comes from
# `tripool = bot.build_pool('0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7')`.
# An off-line test fixture built the same pool here so the math below
# runs without RPC.
assert [t.symbol for t in tripool.tokens] == ['DAI', 'USDC', 'USDT']
assert tripool.a_coefficient == 2000
assert tripool.fee == 4000000

# For lending pools (cTokens), the per-block rates are read through the pool's
# data provider at calculation time (see I/O-Free Architecture); the invariant
# math itself is pure
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

Contract addresses are centralized in `degenbot.balancer.deployments`; the set of
deployments with broken swaps is not, because it is Rust-core state rather than a
caller-side filter:

```python
from degenbot.balancer.deployments import (
    BALANCER_V2_VAULT_ADDRESS,
    BALANCERQUERIES_CONTRACT_ADDRESS,
)
from degenbot.exceptions import PoolRegistrationError, SpecViolationError

# Canonical Vault + BalancerQueries addresses
assert BALANCER_V2_VAULT_ADDRESS == '0xBA12222222228d8Ba445958a75a0704d566BF2C8'
assert BALANCERQUERIES_CONTRACT_ADDRESS == '0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5'

# Deployments whose swaps are disabled at the contract level (BAL#327
# SWAPS_DISABLED) need no caller-side filter list: the Rust core owns that set
# and refuses the build on both Balancer arms, before the arm's first state
# read, so a broken deployment can never register as a pool that reverts on
# every swap simulation. The refusal is the typed admission error below — a
# `PoolRegistrationError`, so one `except` covers the whole admission taxonomy:
broken = '0x753BD6a5bF0b14ae7e5d2877e5cD6a3398aA2AAB'  # YUME/WETH 1/99, swaps disabled
# bot.build_pool(broken)  →  SpecViolationError: pool build refused: out of spec
assert issubclass(SpecViolationError, PoolRegistrationError)
```

### Balancer V2 Stable Pools

Balancer V2 stable pools (MetaStablePool and ComposableStablePool) use the StableSwap invariant with rate caching. The math libraries are ported from deployed contracts with exact integer-level matching against on-chain results.

Two pool shapes share the same `BalancerV2StablePool` interface:

- **MetaStablePool** — a 2-token stable pool with no BPT token and near-static rates: exact swap math needs no rate provider and no extra I/O.
- **ComposableStablePool** — a multi-token stable pool that includes its own BPT token; time-varying rates (e.g., bb-a-* yield tokens) require a live `BalancerRateProvider`, and without one a swap call raises `StaleRateResult` (the approximate result is still readable on the exception). The pool's swap verbs are the boundary that refuses an approximate answer; the swap math still computes it, and the quoter reports it as an inexact verdict.

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
# without one the call raises StaleRateResult — the swap verbs refuse an
# approximate answer and leave the computed amounts readable on the exception.
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
    # StaleRateResult carries the approximate result so callers can still read it
    assert e.amount_in == 10**18
    assert e.amount_out == 1001103
```

### Uniswap Arbitrage

Optimal arbitrage amounts for a cyclic pool sequence are computed by the Rust `ArbitrageEngine` (EVM-exact U512 solve), driven through `EngineRegistry`. The older Python cycle/solver classes are retired — the Rust engine is the sole solve surface:

<!-- invisible-code-block: python
import asyncio
import degenbot
# Executed but never shown: the live-RPC examples read historical state, so
# they need an archive-capable node, which is what the harness name resolves.
from tests.conftest import TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI as RPC_URL
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.v3_pool_factory import make_v3_pool
from fractions import Fraction

# A single Bot owns the shared BotState both pools register into. Two
# USDC/WETH pools (one V2, one V3) form a valid 2-hop cyclic arb.
bot = degenbot.Bot(
    chain_id=1,
    node=RPC_URL,
    database=":memory:",
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
registry.engine.set_verify_rpc_url(RPC_URL)
registry.register_v2_pool(v2_pool)
asyncio.run(registry.register_v3_pool(v3_pool))
-->

```python
# In production, `registry.start(node_http, node_ws)` runs the startup
# ritual (subscribe, snapshot, verify config) and returns BEFORE resume();
# after attaching the result consumer, `registry.engine.resume()` is the
# single gate after which one result batch per block flows.
path_id, created = registry.register_path(
    pools_and_zfos=[(v2_pool, True), (v3_pool, False)],
)
# The registered path is inspectable immediately (a solved snapshot of its
# hops); profitable solves surface in the next `latest_results()` batch.
solved_path = registry.engine.inspect_path(path_id)
assert solved_path["path_id"] == path_id
```

#### Swap Encoding & On-Chain Execution

A profitable solve goes straight from the Rust engine to a submitted
transaction — there is no Python encoding layer. After a path solves:

1. **Encode** — the Rust core emits the per-hop calldata (V2 `swap()`, V3 `swap()`, V4 PoolManager `swap()`, Curve `exchange()` / `exchange_underlying()`) and composes it into the cmd-executor contract envelope.
2. **Submit** — the Rust submission layer signs (EIP-1559) and sends the transaction; dry-run mode (the default) stops after solving and simulates in-process without submitting.

### Running the Settlement-Arbitrage Bot

The end-to-end settlement-arbitrage bot is a thin Python driver over the `degenbot.runner` package: the example owns only CLI parsing + SIGINT handling, while `BotRunner` owns the session and the Rust engine handshake.

```bash
# Dry run (default): solves, simulates in-process, renders profit lines — nothing is submitted
uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py \
  --node "https://eth-mainnet.example.com"

# Restrict to one 3-hop permutation (overrides the driver's default path filter)
uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py --permutation V2-V3-V4

# Live mode: signs and submits real transactions (operator key via env)
uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py --live
```

Endpoints resolve through the shared four-layer cascade: the operator file's `[nodes]` tables are the base layer, the `DEGENBOT_RPC_HTTP_CHAINID_1` / `DEGENBOT_RPC_WS_CHAINID_1` env names override that chain's file entry alone, and the example's own `--node` flag (one self-classifying URI) outranks the environment. Operator/executor identity is declared config (`driver.*`, docs in [`docs/rust-config-keys.md`](docs/rust-config-keys.md)): `OPERATOR_ADDRESS` / `OPERATOR_PRIVATE_KEY` are required in live mode, `EXECUTOR_CONTRACT_ADDRESS` / `EXECUTOR_OWNER_ADDRESS` / `INJECTED_EXECUTOR_ADDRESS` / `EXECUTOR_RUNTIME` are optional overrides, and each name resolves through the same cascade as every other key — operator file, environment, declared default. `BotRunner` performs the driver-side startup handshake, after which the **Rust core owns the hot loop** — event decode, per-block re-solve, in-process simulation, encoding, submission — and the Python driver owns config, result rendering, and dispatch policy. With `--operator-socket PATH`, the bot also hosts an `OperatorServer` that the `degenbot path add` / `degenbot path discover` CLI commands target to steer the live path set without a restart (protocol + design in [`docs/architecture/operator-add-path-surface.md`](docs/architecture/operator-add-path-surface.md)).

Both drivers launch through the repo's `./run_bot.sh` wrapper, which owns the
shared env exports, the RPC-cascade print, build-on-demand for the Rust
driver, and the `start`/`stop`/`status` lifecycle:

```bash
./run_bot.sh                      # Python driver, foreground (legacy default)
./run_bot.sh --python start       # Python driver, detached (same command/env as above)
./run_bot.sh --rust start         # pure-Rust driver (rust/examples/settlement_bot)
./run_bot.sh --rust start -- --live --permutation V2-V3-V4   # args after `--` pass through verbatim
./run_bot.sh --rust print-cmd     # resolved driver + command + exports; no build, no launch
./run_bot.sh status               # covers both drivers
./run_bot.sh stop                 # stops whichever driver is running
```

`--python` is the default and is byte-identical to the direct `uv run`
invocation above. `--rust` builds
`rust/target/$RUST_PROFILE/degenbot-settlement-bot-example` when the binary or
workspace is stale (`RUST_PROFILE=release` by default; `RUST_PROFILE=dev` opts
into the workspace `opt-level = 1` development profile for iteration) and arms
the Rust example's live handshake by exporting
`SMOKE_RPC_URL` from the same RPC cascade the Python driver resolves. `--live`
is never implied — pass it explicitly after `--`. `stop`/`status` cover both
driver process names plus the pidfile. The launcher-consolidation record, with
the two deliberate divergences between the drivers, lives in
[`docs/architecture/rust-settlement-bot-parity.md`](docs/architecture/rust-settlement-bot-parity.md).

## Bot API Reference

The `Bot` class is the primary entry point for degenbot usage. Access factories, registries, and utilities through Bot.

### Initialization

```python
import degenbot

# RPC_URL is any HTTP RPC endpoint for chain 1, for example
#   "https://eth-mainnet.example.com"
# With explicit settings
bot = degenbot.Bot(
    chain_id=1,
    node=RPC_URL,
    database=":memory:",
)
# The RPC provider is built from the node endpoint and its eth_chainId is
# enforced to equal chain_id at construction — no manual registration needed.
```

### Universal Pool Builder

```python
# Universal builder — the type resolves through the core identity ladder
pool = bot.build_pool(
    "0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8",
    state_block=18900000,  # Optional, defaults to current block
)
```

<!-- skip: start "requires Base chain RPC node" -->

```python
# V4 pools are identified by (PoolManager, pool id), and the id rides a typed request
from degenbot.builders.request import BuildManagedPoolRequest

pool = bot.build_managed_pool(
    "0x498581fF718922c3f8e6A244956aF099B2652b2b",  # PoolManager singleton
    BuildManagedPoolRequest(
        pool_id="0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a",
    ),
)
```

<!-- skip: end -->

### Pool Construction by Type

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

<!-- skip: start "requires Base chain RPC node" -->

```python
# V4 pool (singleton architecture): a pool the database has never seen carries
# its whole immutable set on the request — nothing about it is discoverable
# on-chain without them
pool = bot.build_managed_pool(
    "0x498581fF718922c3f8e6A244956aF099B2652b2b",  # PoolManager singleton
    BuildManagedPoolRequest(
        pool_id="0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a",
        state_view_address="0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71",
        tokens=[
            "0x0000000000000000000000000000000000000000",
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        ],
        fee=500,
        tick_spacing=10,
    ),
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
eth_balance = bot.get_ether_balance(address="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
```

### Accessing Bot Components

```python
# RPC provider (built from config; chain_id enforced at construction)
provider = bot.provider

# Registries (check if already created)
existing_pool = bot.pools.get(chain_id=1, pool_address="0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
existing_token = bot.tokens.get(token_address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", chain_id=1)

# Rust-backed database state; use the stable degenbot.db mirror
from degenbot.db import db_inspect_schema_state

schema_state = db_inspect_schema_state(str(bot.database_path))
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

# `price` reads through the session's provider, so it needs a Bot — pass
# `bot=` at construction; without one, `decimals=` above still answers
```

## CLI Reference

Degenbot provides a command-line interface for managing blockchain data and pool state. The console is the Rust `degenbot-cli` binary; `docs/rust-cli.md` is its authoritative command/flag/exit-code reference.

### Installing the Console

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

# RETIRED (ADR-052): the database upgrades itself at open. `database
# upgrade` now renders a pointed error and exits 1; use `database heal` for an
# explicit repair.
degenbot database upgrade

# Inspect the schema state (read-only; never writes)
degenbot database inspect

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
degenbot pool update [--chunk SIZE] [--max-block-range BLOCKS] [--to-block BLOCK]
                     [--verify-chunk/--no-verify-chunk] [--verify-all/--no-verify-all]
                     [--verify-all-interval BLOCKS]

# Verify one V3/V4 pool's DB state against on-chain truth at a given block
degenbot pool verify --rpc-url URL --chain 1 --block 18900000 --pool 0x... --family v3|v4 [--pool-manager 0x...]

# Activate an exchange for tracking (ADR-051 D5: one data-driven command;
# --chain is a chain slug or numeric id, --name is the DEX name slug)
degenbot exchange activate --chain base --name uniswap_v3

# Deactivate an exchange
degenbot exchange deactivate --chain base --name uniswap_v3

# List every supported (chain, DEX) pair with its activation state
degenbot exchange list [--chain base]

# Steer a running bot (started with --operator-socket) without restarting it
degenbot path add --socket /path/to/operator.sock --hop V2:0xPoolAddr [--hop V3:0xPoolAddr] [--direction zfo|ozf]
degenbot path discover --socket /path/to/operator.sock [--bound N]
```

**Supported exchanges** (`--chain <chain> --name <dex>`):
- Base: `aerodrome_v2`, `aerodrome_v3`, `pancakeswap_v2`, `pancakeswap_v3`, `sushiswap_v2`, `sushiswap_v3`, `swapbased_v2`, `uniswap_v2`, `uniswap_v3`, `uniswap_v4`
- Ethereum: `pancakeswap_v2`, `pancakeswap_v3`, `sushiswap_v2`, `sushiswap_v3`, `uniswap_v2`, `uniswap_v3`, `uniswap_v4`

#### Fleet Posture Management

The bot's operator channel also hosts the live **cordon posture** of a worker fleet (Nominal/Cordoned state and its thresholds). Socket resolution matches `path`: `--socket` flag, else `DEGENBOT_OPERATOR_SOCKET`, else `~/.config/degenbot/operator.sock`.

```bash
# Show the LIVE cordon posture: thresholds + Nominal|Cordoned
degenbot fleet posture show [--socket /path/to/operator.sock]

# Re-tune the LIVE cordon thresholds (a partial patch)
degenbot fleet posture set [--socket /path/to/operator.sock] \
  [--cordon-enter-events COUNT] [--cordon-duty-percent PERCENT] \
  [--cordon-enter-window-ms MS] [--cordon-duty-window-ms MS] \
  [--cordon-exit-clean-ms MS] [--cordon-sim-intake-floor COUNT|null]
```

#### Aave State Management

```bash
# Update Aave V3 positions for all active markets
degenbot aave update [--chunk SIZE] [--to-block BLOCK] [--verify-chunk/--no-verify-chunk] [--dry-run]

# Activate the Aave market for the session chain (global --chain-id; default 1)
degenbot aave activate [--chain-id CHAIN_ID]

# Deactivate an Aave market (default: "Aave Ethereum Market")
degenbot aave deactivate [--chain-id CHAIN_ID] [--name MARKET]

# Show a user's position in a market
degenbot aave position show <ADDRESS> [--market MARKET] [--chain-id CHAIN_ID]

# Purge one market's populated state, then re-run its cold-boot update
degenbot aave reset [--name MARKET] [--dry-run]

# Print one market's completed-market record (row counts + table digests)
degenbot aave digest [--market-name MARKET]
```

#### Config and Strategy Management

```bash
# Inspect the operator file and the values the process resolves (read-only)
degenbot config show [--resolved]
degenbot config get nodes.http.1
degenbot config path

# Write or drop one declared key / table entry (validated before write)
degenbot config set nodes.http.1 "http://localhost:8545" [--force]
degenbot config unset nodes.http.1 [--force]

# Inspect and steer the typed strategy facets (ADR-055)
degenbot strategy list
degenbot strategy show settlement
degenbot strategy activate settlement (--endpoints URLS | --endpoints-default)
degenbot strategy deactivate settlement
degenbot strategy set settlement <KEY> <VALUE>
degenbot strategy default settlement <KEY>
```

> **Rust-owned console (ADR-051).** The `degenbot` console is the `degenbot-cli`
> Rust binary; the Python console script (`degenbot._cli:main`) and
> `python -m degenbot` are thin passthroughs over the same command model, so
> argv, prompts, and exit codes exist once. Two read-only commands from the
> former Python click tree have no Rust arm yet and exit with a usage error:
> `degenbot aave position risk` and `degenbot aave market show`.

### Block Identifiers

Commands accepting `--to-block` support the following formats:

| Format | Example | Description |
|--------|---------|-------------|
| Tag | `latest` | One of the closed tag set: `earliest`, `finalized`, `safe`, `latest`, `pending` |
| `<tag>:<signed offset>` | `latest:-64`, `safe:128` | The tag's block plus a negative or positive offset; the updaters default to `latest:-64` |
| Number | `18900000` | Specific block number |

## Configuration

### Configuration File

The operator file `$XDG_CONFIG_HOME/degenbot/config.toml` (else `~/.config/degenbot/config.toml`, or the `DEGENBOT_CONFIG`
override) is the typed Rust `BotConfig` file layer: its tables must name
declared schema sections (see
[docs/rust-config-keys.md](docs/rust-config-keys.md) for the authoritative,
generated key reference), plus the two free-form tables the loader skips rather
than types: `[failure_policy]` (per-bucket reaction overrides, ADR-040) and
`[deployments]` (the Python registry overlay, ADR-062). It is the BASE layer of
the cascade — the per-chain endpoints, the session chain id, and
the database path are read from it:

```toml
# Per-chain node endpoints, keyed by chain id: the base layer.
[nodes]
http = { 1 = "http://localhost:8545" }
ws = { 1 = "ws://localhost:8546" }

[session]
chain_id = 1

[database]
path = "./degenbot.db"

[telemetry]
otel = true
jaeger_endpoint = "http://localhost:4318"
metrics_addr = "0.0.0.0:9464"

# Free-form per-bucket failure-reaction overrides (ADR-040). An empty or
# missing table runs the default matrix.
[failure_policy]
```

`[nodes.ipc]` takes a socket path or an `ipc://` URL for a node running beside
the bot. Treat the file as a secret carrier: `chmod 600` it, and prefer a
per-machine environment variable for anything credential-shaped.

### Layer Precedence

| Precedence | Layer | Supplies |
|---|---|---|
| 1 | explicit CLI | `--node <uri>`, `--chain-id`, `--database` |
| 2 | environment | `DEGENBOT_RPC_{HTTP,WS,IPC}_CHAINID_<chain>`, `DEGENBOT_DEFAULT_CHAIN_ID`, `DEGENBOT_DB_PATH` |
| 3 (base) | `--config` file | `[nodes.*]`, `session.chain_id`, `database.path`, and every other typed section |
| 4 | declared default | the schema default (`database.path` → the XDG state home) |

An env entry for one chain overrides that chain's file entry ALONE, and
`--node <uri>` outranks the environment; it is repeatable, and the value
classifies its own transport — `http(s)://` → `nodes.http`, `ws(s)://` →
`nodes.ws`, `ipc://` or a socket path → `nodes.ipc`.

`degenbot config show --resolved` prints the whole inventory as the process
resolves it, each key annotated with the layer that won it, and
`degenbot config path` prints the file the cascade reads; both are read-only
(see [docs/rust-cli.md](docs/rust-cli.md)). That is the place to look first when
an edited file entry appears to be ignored.

A surviving pre-0.6 spelling (`[rpc]`, `[ws]`, `[database] filepath`, top-level
`default_chain_id`) is refused at boot, as is the `[otel]` table — see
[docs/config-migration.md](docs/config-migration.md) for the replacement table.

The SQLite database defaults to the XDG state home — `$XDG_STATE_HOME/degenbot/db/degenbot.db`
when `$XDG_STATE_HOME` is an absolute path, else `~/.local/state/degenbot/db/degenbot.db`.

### Environment Variables

| Variable | Values | Description |
|----------|--------|-------------|
| `DEGENBOT_DEBUG` | `1`, `true`, `yes` | Enable debug-level logging output |
| `DEGENBOT_DEFAULT_CHAIN_ID` | integer chain id | The env layer of the declared `session.chain_id`; overrides the file's `[session] chain_id` (ADR-006, one Bot per chain). Only the top-level FILE key `default_chain_id` is retired — this env name is live. A `Bot` refuses to construct without a chain id from some layer, and the connected RPC's `eth_chainId` is enforced to match at construction |
| `DEGENBOT_RPC_HTTP_CHAINID_<ID>` | any HTTP(S) URL | HTTP RPC endpoint for chain `<ID>`; overrides that chain's `[nodes.http]` entry alone |
| `DEGENBOT_RPC_WS_CHAINID_<ID>` | any WS(S) URL | WebSocket endpoint for chain `<ID>`; overrides that chain's `[nodes.ws]` entry alone |
| `DEGENBOT_RPC_IPC_CHAINID_<ID>` | any `ipc://` URL or socket path | Local IPC endpoint for chain `<ID>`; overrides that chain's `[nodes.ipc]` entry alone |
| `DEGENBOT_DB_PATH` | filesystem path | SQLite database file; overrides the file's `database.path` |
| `DEGENBOT_CONFIG` | filesystem path | Operator config file; overrides the XDG config home |
| `DEGENBOT_OPERATOR_SOCKET` | filesystem path | Operator-channel socket the `path` / `fleet` commands dial when `--socket` is omitted |
| `DEGENBOT_LOG_LEVEL` | `off`/`error`/`warn`/`info`/`debug`/`trace` | Console wiring default, used only when `RUST_LOG` is absent |
| `DEGENBOT_OTEL` | `0`, `1` | OTLP span layer plus the Prometheus endpoint (default on) |
| `DEGENBOT_METRICS_ADDR` | `host:port` | Prometheus scrape bind address (default `127.0.0.1:9464`) |
| `DEGENBOT_JAEGER_ENDPOINT` | URL | OTLP endpoint for the opt-in Jaeger E2E test (default `http://127.0.0.1:4318`) |

[`docs/rust-config-keys.md`](docs/rust-config-keys.md) is the full env-name-to-key inventory; this table is the operator-facing subset.

```bash
DEGENBOT_DEBUG=1 python my_script.py
```

## The Rust Core (`degenbot_rs` Rust crate, `degenbot._ffi` Python module)

The Rust core is the engine of degenbot — it owns all performance-critical and stateful logic. Python reaches it through the `degenbot._ffi` extension module, a thin PyO3 binding layer (`rust/crates/shells/degenbot-python/`) that translates Python calls into Rust calls with no business logic of its own. The underlying core crates are pyo3-free by default and are consumable directly from pure Rust through the umbrella `degenbot` crate — currently via a git/path dependency (the crates are not yet published to crates.io); the in-repo proof is `rust/crates/facade/degenbot/examples/standalone_consumer.rs`, gated by `just test-standalone`.

The extension is built automatically during installation using [maturin](https://www.maturin.rs/) (or `uv sync`, which invokes maturin under the hood).

### Available Functions

These are the leaf helpers a driver reaches directly. Each is a thin Python companion over a Rust implementation: the math and ABI codecs are pyo3 exports, the address helpers wrap `degenbot._ffi.to_checksum_address` with an LRU cache, and `degenbot.contract` composes the encoding leaves.

#### Tick Math

Concentrated-liquidity tick-to-price conversions (Q96 fixed point), shared by the V3 and V4 families:

```python
from degenbot.uniswap.math import get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio

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

<!-- invisible-code-block: python
from degenbot.contract import encode_function_call, get_function_selector, decode_return_data
-->

```python
# Get a 4-byte function selector (raw bytes, like the other crypto leaves)
selector = get_function_selector("transfer(address,uint256)")
assert selector == bytes.fromhex("a9059cbb")

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

<!-- live-rpc: start "requires live RPC" -->

<!-- invisible-code-block: python
from degenbot._ffi.contract import Contract
from degenbot._ffi.provider import AlloyProvider
# Executed but never shown: the live-RPC examples read historical state, so
# they need an archive-capable node, which is what the harness name resolves.
from tests.conftest import TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI as RPC_URL
-->

```python
# RPC_URL is any HTTP RPC endpoint for chain 1, for example
#   "https://eth-mainnet.example.com"
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

The MEV workload — per-block re-solve of hundreds of cyclic paths, EVM-exact evm2 simulation, ABI decode, tick math, and swap encoding — is latency-bound at the CPython boundary, so the pump loop runs in Rust with the GIL released around each PyO3 crossing. Criterion benches for the math, pathfinding, pool, and db crates — plus the updater replay bench (`just bench-updaters`) — live in-repo; the pump's own counters are opt-in behind the `hotpath` feature.

### Build Requirements

Cargo commands without a package selector build only the two pure-Rust
workspace defaults: the `degenbot` umbrella and the `degenbot-cli` console.
The PyO3 extension and non-publishable sample crates remain workspace members
for explicit recipes and the full workspace gate, but a plain `cargo build`
does not compile them.

The extension is pre-built in published packages. For source builds:

- Rust 1.98.1 (the workspace MSRV is Rust 1.97)
- maturin (installed automatically with `uv sync`)

Local Rust and maturin development builds intentionally use the workspace
`[profile.dev]`: `opt-level = 1`, no LTO or stripping, and line-tables-only
debug information. This favors representative optimization over an
unoptimized debug build while keeping iterative rebuilds cheaper than
release. Release builds use `[profile.release]` (thin LTO, stripped), with the
shipped `degenbot_rs` cdylib pinned to `codegen-units = 1` so the final link
gets maximal codegen granularity; every other member library and third-party
dependency compiles at the profile default of 16 units and is re-converged by
that same link. Release-profile test runs use `[profile.release-tests]`, which
inherits release's semantics and drops only the two packaging keys (`lto`,
`strip`), because a thin-LTO link of a test binary is pure link cost.

```bash
# Build the two default pure-Rust entry points
cargo build --manifest-path rust/Cargo.toml

# Build the extension (same as `just build-rust-extension`)
cargo build --locked -p degenbot_rs --features extension-module --manifest-path rust/Cargo.toml

# Standalone smoke + the dev-profile workspace suite, then the release-profile suite
just test-rust

# Bootstrap locked Python dependencies, then build/install the explicit
# development feature set through the canonical extension path.
just bootstrap

# Rebuild after Rust edits (incremental `maturin develop`). `just test-python`
# also builds the extension first, so a test run never uses a stale `.so`.
just dev

# Direct Cargo hotpath test/build (same requirement):
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--cfg tokio_unstable" cargo test --locked \
  -p degenbot-bot --features hotpath --lib profiling:: -- --ignored --nocapture
```

### Rust Feature Matrix

The Rust workspace is checked by named feature lanes. The default lane never
uses `--all-features`, so a default-feature regression cannot be hidden by an
exhaustive build.

| Lane | Feature set | Recipe |
| --- | --- | --- |
| Workspace defaults | Every workspace member with its declared default features; no `--all-features` | `just lint-rust-check` or `just check-rust-default` |
| Pure-Rust consumer | `degenbot` and its examples, with the umbrella's defaults and no PyO3 binding | `just check-rust-consumer` |
| Binding defaults | `degenbot_rs` with its broad default domain features, without `extension-module` | `just check-rust-binding-default` |
| Binding no-default | `degenbot_rs` with no domain feature on (`--no-default-features`): the no-bot module tree, and the shape that proves every export carries the gate its module needs | `just check-rust-binding-no-default` |
| Development wheel | the binding's `dev-features` alias: `pyo3/extension-module`, `degenbot-bot/hotpath`, `degenbot-bot/hotpath-prometheus`, `degenbot-pools/hotpath`, and `otel`; `allocator-ctrl` and `mimalloc` stay explicit opt-ins for A/B probes | `just check-rust-dev-features` |
| Release wheel | `extension-module` (forwards to `pyo3/extension-module`) plus `degenbot_rs` defaults; no dev-only profiling or telemetry features | `just check-rust-extension-release` or `just build-rust-extension` |
| Diagnostic | Workspace `--all-features`, including test-only and mutually exclusive variants | `just check-rust-all-features` |

The development-wheel list is the binding manifest's canonical `dev-features`
alias selected by `just dev`; `just bootstrap` installs locked Python
dependencies without building the project through a second path. Release wheels
are built with `maturin --release --features pyo3/extension-module`; that
release command
replaces the development feature list and keeps the binding crate's defaults,
while excluding the development-only features above. The release profile keeps
thin LTO, stripping, and the intentional per-package codegen-unit policy; the
extension-release recipe checks this same feature set with Cargo's release
profile.

## Documentation

Additional documentation is available in the [`docs/`](docs/) directory:

- **[Architecture](docs/architecture/)**: High-level architectural patterns
  - [Rust-Owned Settlement-Arbitrage Bot](docs/architecture/rust-owned-bot.md) — the original `ArbitrageEngine` design (Plans 079–082); marked historical, kept as a design-history reference (the current state layer follows the ADR log)
  - [Operator Add-Path Surface](docs/architecture/operator-add-path-surface.md) — steering a live bot (mid-run add-path + bounded on-demand discovery) over the Unix-socket JSON-lines operator channel
  - [Semantic Matching](docs/architecture/semantic-matching.md) — Event processing patterns for Aave
- **[Investigation harness](docs/architecture/investigation-harness.md)**: the simulation oracle driver, the per-contract scaffolder, and the path-fixture toolkit — for debugging a failing settlement-arbitrage path or building a custom simulation harness
- **[Architecture Decision Records](docs/adr/)**: the ADR design log for the Python→Rust migration (three-layer architecture, per-chain Bot, schema retention/cutover, registration-verify lifecycle, executor grammar, …)
- **[Execution Strategy](docs/execution-strategy.md)**: the user-owned `ExecutionStrategy` seam (ADR-025)
- **[Aave V3](docs/cli/aave.md)**: The Aave market/position command reference
- **[CLI](docs/cli/)**: Detailed CLI command reference (`aave.md`, `database.md`, `pool.md`)
- **[Logging](docs/logging.md)**: Controlling `RUST_LOG` / `DEGENBOT_DEBUG` tracing, the env-gated hard/loud diagnostics, and debug-named diagnostics

### Upstream Sources

The Solidity ground truth for each supported core lives upstream, pinned to the release each port was written against, next to the deployed contract that fixes its on-chain behavior:

| Protocol | Source | Deployed |
|----------|--------|----------|
| Uniswap V2 | [`Uniswap/v2-core@v1.0.1`](https://github.com/Uniswap/v2-core/tree/v1.0.1) | factory [`0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f`](https://etherscan.io/address/0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f) |
| Uniswap V3 | [`Uniswap/v3-core@v1.0.0`](https://github.com/Uniswap/v3-core/tree/v1.0.0) | factory [`0x1F98431c8aD98523631AE4a59f267346ea31F984`](https://etherscan.io/address/0x1F98431c8aD98523631AE4a59f267346ea31F984) |
| Uniswap V4 | [`Uniswap/v4-core@v4.0.0`](https://github.com/Uniswap/v4-core/tree/v4.0.0) | `PoolManager` [`0x000000000004444c5dc75cB358380D2e3dE08A90`](https://etherscan.io/address/0x000000000004444c5dc75cB358380D2e3dE08A90) |
| Aave V3 | [`bgd-labs/aave-v3-origin`](https://github.com/bgd-labs/aave-v3-origin) | `Pool` [`0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2`](https://etherscan.io/address/0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2) |

Useful when auditing, or when you need to understand the exact on-chain behavior of a supported protocol.

## Contributing

Contributions are welcome! Please submit issues and pull requests to the [GitHub repository](https://github.com/BowTiedDevil/degenbot).

### Development Setup

```bash
git clone https://github.com/BowTiedDevil/degenbot.git
cd degenbot
just bootstrap

# The default gate: Rust lint + the tripwire ratchet, the whole workspace suite
# under nextest, the full pytest suite, the offline executor lane, and the
# consumer probes
just test

# Individual tracks:
just test-rust    # standalone smoke + the dev-profile workspace suite, then the release-profile suite
just test-python  # builds the extension (incremental), then pytest + the executor release ratchet
```

## License

This code is published under a permissive MIT license. See [LICENSE](LICENSE) for details.

## Donation

If you find this code valuable, please fund continuing development by donating to [`0xADAf500b965545C8A766CD9Cdeb3BF3FBef073e5`](https://etherscan.io/address/0xadaf500b965545c8a766cd9cdeb3bf3fbef073e5) on any EVM compatible chain.
