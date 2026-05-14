# Plan 015: Extract ChainDataSource Abstraction from Bot

**Status: SUPERSEDED by Plan 001**

Plan 001 extracted all pool-building I/O into typed builder classes that receive `connections` and `db` via dependency injection. The builders themselves are now the I/O seam — there is no remaining god class in Bot to extract from. A `ChainDataSource` or `ChainIO` protocol would add abstraction without a current consumer. If test isolation (fake I/O) or record/replay becomes a concrete need later, a narrow protocol can be introduced at the builder constructor boundary.

## Overview

Refactor Bot to delegate all blockchain I/O to a `ChainDataSource` abstraction. This enables Bot to serve as a robust test harness while maintaining its orchestration role (registry, factory, fetcher injection).

**Key insight:** Bot currently couples two responsibilities:
1. **I/O orchestration** (fetching on-chain data via RPC)
2. **Pool construction** (factory pattern, registry management, fetcher injection)

Tests need pool construction without I/O. The solution: inject the I/O layer.

## Problem Statement

### Current State

Bot's `build_*` methods do 100+ lines of RPC calls before constructing pools:

```python
def build_curve_pool(self, address, ...):
    # 100+ lines of RPC calls
    factory = provider.call(address, "factory()", ...)
    token0 = provider.call(address, "token0()", ...)
    token1 = provider.call(address, "token1()", ...)
    # ... 20+ more RPC calls for lending detection, crypto params, metapool, etc.
    
    # Finally construct pool
    return CurveStableswapPool(
        address=address,
        tokens=tokens,
        # ... data fetched above
    )
```

**Testing challenges:**
- Tests must mock providers (complex, fragile, requires knowledge of internal RPC sequence)
- OR bypass Bot entirely (loses orchestration benefits: registry, manager creation, fetcher injection)
- No way to record production scenarios and replay in tests

### Desired State

Bot delegates I/O to an injected `ChainDataSource`:

```python
def build_curve_pool(self, address, ...):
    # Fetch data via abstraction (could be chain, fake, or replayed)
    metadata = self.chain_data.get_curve_pool_metadata(address, chain_id)
    state = self.chain_data.get_curve_pool_state(address, block)
    fetchers = self.chain_data.get_curve_fetcher_factories(address, chain_id, metadata)
    
    # Construct pool (pure factory, no I/O)
    return CurveStableswapPool(
        address=metadata.address,
        tokens=self._build_tokens(metadata.tokens),
        a_coefficient=metadata.a_coefficient,
        balances=state.balances,
        **fetchers.to_kwargs(),
    )
```

**Benefits:**
- Same Bot API works in production and tests
- Explicit data dependencies (metadata vs state vs fetchers)
- Record/replay enabled (wrap OnChainDataSource with RecordingDataSource)
- Clean separation: I/O in ChainDataSource, orchestration in Bot

## Design: ChainDataSource Protocol

### Three Categories of Data

Pool construction requires data with different lifecycles:

1. **Metadata** (immutable, fetched once, cached forever)
   - Contract addresses, static parameters, deployment constants
   - Examples: factory, tokens, fee, tick_spacing, A coefficient

2. **State** (mutable, fetched per block)
   - Reserves, sqrt_price, liquidity, balances
   - Changes every block, must be re-fetched

3. **Fetchers** (on-demand callbacks, injected into pool)
   - Functions the pool calls when it needs data
   - Examples: timestamp_fetcher, D_fetcher, virtual_price_fetcher

### Protocol Definition

```python
# src/degenbot/chain_data/protocol.py

from typing import Protocol, Callable
from dataclasses import dataclass
from fractions import Fraction
from eth_typing import ChecksumAddress

# === Data Classes ===

@dataclass(frozen=True)
class TokenMetadata:
    """Immutable token metadata."""
    address: ChecksumAddress
    name: str
    symbol: str
    decimals: int
    chain_id: int


@dataclass(frozen=True)
class V2PoolMetadata:
    """Immutable data for V2-style pools."""
    address: ChecksumAddress
    chain_id: int
    factory: ChecksumAddress
    token0: ChecksumAddress
    token1: ChecksumAddress
    fee_token0: Fraction
    fee_token1: Fraction
    deployer: ChecksumAddress
    init_hash: str


@dataclass(frozen=True)
class V2PoolState:
    """State for V2-style pools (mutable, per-block)."""
    block: int
    reserves_token0: int
    reserves_token1: int


@dataclass(frozen=True)
class V3PoolMetadata:
    """Immutable data for V3-style pools."""
    address: ChecksumAddress
    chain_id: int
    factory: ChecksumAddress
    token0: ChecksumAddress
    token1: ChecksumAddress
    fee: int
    tick_spacing: int
    deployer: ChecksumAddress
    init_hash: str


@dataclass(frozen=True)
class V3PoolState:
    """State for V3-style pools (mutable, per-block)."""
    block: int
    sqrt_price_x96: int
    tick: int
    liquidity: int
    tick_bitmap: dict[int, "UniswapV3BitmapAtWord"]
    tick_data: dict[int, "UniswapV3LiquidityAtTick"]


@dataclass(frozen=True)
class CurvePoolMetadata:
    """Immutable data for Curve pools."""
    address: ChecksumAddress
    chain_id: int
    tokens: tuple[ChecksumAddress, ...]
    a_coefficient: int
    fee: int
    admin_fee: int
    lp_token: ChecksumAddress | None
    # A ramping
    initial_a: int | None
    future_a: int | None
    initial_a_time: int | None
    future_a_time: int | None
    # Metapool
    base_pool: ChecksumAddress | None
    tokens_underlying: tuple[ChecksumAddress, ...] | None
    # Lending
    use_lending: tuple[bool, ...]
    precision_multipliers: tuple[int, ...] | None
    # Crypto pool
    fee_gamma: int | None
    mid_fee: int | None
    out_fee: int | None
    gamma: int | None
    offpeg_fee_multiplier: int | None


@dataclass(frozen=True)
class CurvePoolState:
    """State for Curve pools (mutable, per-block)."""
    block: int
    balances: tuple[int, ...]


@dataclass
class FetcherFactories:
    """Factory functions that create fetcher callbacks."""
    # V3
    tick_data_fetcher: Callable[[int, int], "UniswapV3LiquidityAtTick"] | None = None
    
    # Curve
    timestamp_fetcher: Callable[[int], int] | None = None
    virtual_price_fetcher: Callable[[int], int] | None = None
    base_virtual_price_fetcher: Callable[[int], int] | None = None
    D_fetcher: Callable[[int], int] | None = None
    gamma_fetcher: Callable[[int], int] | None = None
    price_scale_fetcher: Callable[[int], tuple[int, ...]] | None = None
    redemption_price_fetcher: Callable[[int], int] | None = None
    admin_balances_fetcher: Callable[[int], tuple[int, ...]] | None = None
    
    # Generic
    block_number_fetcher: Callable[[], int] | None = None
    provider_call: Callable[..., bytes] | None = None


# === Protocol ===

class ChainDataSource(Protocol):
    """
    Protocol for abstracting blockchain data access.
    
    Implementations:
    - OnChainDataSource: Fetches live data from blockchain
    - FakeDataSource: Returns pre-configured test data
    - RecordingDataSource: Records calls for replay
    - ReplayingDataSource: Replays recorded calls
    
    This abstraction allows Bot to work with any data source:
    production (on-chain), testing (fake), or recorded scenarios.
    """
    
    # === Token Metadata ===
    def get_token_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> TokenMetadata:
        """Fetch token name/symbol/decimals."""
        ...
    
    # === V2 Pool ===
    def get_v2_pool_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> V2PoolMetadata:
        """Fetch immutable V2 pool data."""
        ...
    
    def get_v2_pool_state(
        self,
        address: ChecksumAddress,
        block: int,
    ) -> V2PoolState:
        """Fetch V2 pool reserves at a block."""
        ...
    
    # === V3 Pool ===
    def get_v3_pool_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> V3PoolMetadata:
        """Fetch immutable V3 pool data."""
        ...
    
    def get_v3_pool_state(
        self,
        address: ChecksumAddress,
        block: int,
        tick_spacing: int,
        current_tick: int,
    ) -> V3PoolState:
        """Fetch V3 pool state at a block."""
        ...
    
    def get_v3_fetcher_factories(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> FetcherFactories:
        """Create fetcher callbacks for V3 pool."""
        ...
    
    # === V4 Pool ===
    def get_v4_pool_metadata(
        self,
        pool_id: bytes,
        pool_manager: ChecksumAddress,
        chain_id: int,
    ) -> "V4PoolMetadata":
        """Fetch immutable V4 pool data."""
        ...
    
    def get_v4_pool_state(
        self,
        pool_id: bytes,
        state_view: ChecksumAddress,
        block: int,
        tick_spacing: int,
        current_tick: int,
    ) -> "V4PoolState":
        """Fetch V4 pool state at a block."""
        ...
    
    # === Curve Pool ===
    def get_curve_pool_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> CurvePoolMetadata:
        """Fetch immutable Curve pool data."""
        ...
    
    def get_curve_pool_state(
        self,
        address: ChecksumAddress,
        block: int,
    ) -> CurvePoolState:
        """Fetch Curve pool balances at a block."""
        ...
    
    def get_curve_fetcher_factories(
        self,
        address: ChecksumAddress,
        chain_id: int,
        metadata: CurvePoolMetadata,
    ) -> FetcherFactories:
        """Create fetcher callbacks for Curve pool."""
        ...
```

### Implementation: OnChainDataSource

```python
# src/degenbot/chain_data/on_chain.py

class OnChainDataSource:
    """Production: Fetches live data from blockchain via RPC."""
    
    def __init__(self, web3, provider):
        self.w3 = web3
        self.provider = provider
    
    def get_token_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> TokenMetadata:
        """Fetch token metadata via RPC."""
        # Move existing code from Bot.build_erc20token
        name = self._call(address, "name()")
        symbol = self._call(address, "symbol()")
        decimals = self._call(address, "decimals()")
        
        return TokenMetadata(
            address=address,
            name=name,
            symbol=symbol,
            decimals=decimals,
            chain_id=chain_id,
        )
    
    def get_v2_pool_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> V2PoolMetadata:
        """Fetch V2 pool metadata - move code from Bot.build_v2_pool."""
        # All the RPC calls from lines 340-420 of current bot.py
        factory = self._call(address, "factory()")
        token0 = self._call(address, "token0()")
        token1 = self._call(address, "token1()")
        
        # Detect pool variant (Camelot, Aerodrome, etc.)
        # ... existing detection logic ...
        
        return V2PoolMetadata(
            address=address,
            chain_id=chain_id,
            factory=factory,
            token0=token0,
            token1=token1,
            # ... other fields
        )
    
    def get_v2_pool_state(
        self,
        address: ChecksumAddress,
        block: int,
    ) -> V2PoolState:
        """Fetch V2 reserves at a block."""
        reserves0, reserves1 = self._call(
            address,
            "getReserves()",
            block=block,
        )
        
        return V2PoolState(
            block=block,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
    
    def get_curve_pool_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> CurvePoolMetadata:
        """Fetch Curve pool metadata - move code from Bot.build_curve_pool."""
        # All the RPC calls from lines 1240-1520 of current bot.py
        # - coins, balances, A, fee, admin_fee
        # - lending token detection
        # - crypto pool parameter detection
        # - metapool detection
        # ... existing code ...
        
        return CurvePoolMetadata(
            address=address,
            chain_id=chain_id,
            tokens=tuple(tokens),
            a_coefficient=a,
            # ... all other fields
        )
    
    def get_curve_fetcher_factories(
        self,
        address: ChecksumAddress,
        chain_id: int,
        metadata: CurvePoolMetadata,
    ) -> FetcherFactories:
        """Create fetcher closures - move code from Bot._make_curve_* methods."""
        
        def timestamp_fetcher(block: int) -> int:
            """Fetch block timestamp."""
            block_info = self.provider.get_block(block)
            return block_info["timestamp"]
        
        def virtual_price_fetcher(block: int) -> int:
            """Fetch virtual price for metapools."""
            if metadata.base_pool is None:
                return 10**18
            return self._call(metadata.base_pool, "get_virtual_price()", block=block)
        
        def D_fetcher(block: int) -> int:
            """Fetch invariant D for crypto pools."""
            return self._call(address, "D()", block=block)
        
        return FetcherFactories(
            timestamp_fetcher=timestamp_fetcher,
            virtual_price_fetcher=virtual_price_fetcher,
            D_fetcher=D_fetcher if metadata.fee_gamma else None,
            # ... other fetchers
        )
    
    def _call(self, address: str, method: str, block: int = None, return_types=None):
        """Helper for eth_call."""
        data = encode_function_calldata(method, None)
        result = self.provider.call(to=address, data=data, block=block or "latest")
        if return_types:
            return eth_abi.abi.decode(return_types, result)
        return result
```

### Implementation: FakeDataSource

```python
# src/degenbot/chain_data/fake.py

class FakeDataSource:
    """Testing: Returns pre-configured test data."""
    
    def __init__(self):
        self._tokens: dict[tuple[ChecksumAddress, int], TokenMetadata] = {}
        self._v2_metadata: dict[ChecksumAddress, V2PoolMetadata] = {}
        self._v2_states: dict[tuple[ChecksumAddress, int], V2PoolState] = {}
        self._v3_metadata: dict[ChecksumAddress, V3PoolMetadata] = {}
        self._v3_states: dict[tuple[ChecksumAddress, int], V3PoolState] = {}
        self._curve_metadata: dict[ChecksumAddress, CurvePoolMetadata] = {}
        self._curve_states: dict[tuple[ChecksumAddress, int], CurvePoolState] = {}
        self._fetcher_factories: dict[ChecksumAddress, FetcherFactories] = {}
    
    # === Registration API ===
    
    def add_token(
        self,
        address: str,
        name: str,
        symbol: str,
        decimals: int,
        chain_id: int = 1,
    ) -> "FakeDataSource":
        """Register a token. Returns self for chaining."""
        addr = get_checksum_address(address)
        self._tokens[(addr, chain_id)] = TokenMetadata(
            address=addr,
            name=name,
            symbol=symbol,
            decimals=decimals,
            chain_id=chain_id,
        )
        return self
    
    def add_v2_pool(
        self,
        address: str,
        token0: str,
        token1: str,
        reserves0: int,
        reserves1: int,
        factory: str = "0x5C69bEe701ef814E44274f655e7632cB715C14B6",
        fee: Fraction = Fraction(3, 1000),
        block: int = 18_000_000,
        chain_id: int = 1,
    ) -> "FakeDataSource":
        """Register a V2 pool with metadata and initial state. Returns self."""
        addr = get_checksum_address(address)
        
        # Auto-register tokens if not present
        if (get_checksum_address(token0), chain_id) not in self._tokens:
            self.add_token(token0, token0[:8], token0[:6], 18, chain_id)
        if (get_checksum_address(token1), chain_id) not in self._tokens:
            self.add_token(token1, token1[:8], token1[:6], 18, chain_id)
        
        # Register metadata
        self._v2_metadata[addr] = V2PoolMetadata(
            address=addr,
            chain_id=chain_id,
            factory=get_checksum_address(factory),
            token0=get_checksum_address(token0),
            token1=get_checksum_address(token1),
            fee_token0=fee,
            fee_token1=fee,
            deployer=get_checksum_address(factory),
            init_hash=UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
        )
        
        # Register state
        self._v2_states[(addr, block)] = V2PoolState(
            block=block,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        
        return self
    
    def add_curve_pool(
        self,
        address: str,
        tokens: list[str],
        balances: list[int],
        a_coefficient: int,
        fee: int,
        admin_fee: int,
        block: int = 18_000_000,
        chain_id: int = 1,
        fetchers: FetcherFactories | None = None,
    ) -> "FakeDataSource":
        """Register a Curve pool. Returns self for chaining."""
        addr = get_checksum_address(address)
        
        # Auto-register tokens
        for token_addr in tokens:
            if (get_checksum_address(token_addr), chain_id) not in self._tokens:
                self.add_token(token_addr, token_addr[:8], token_addr[:6], 18, chain_id)
        
        # Register metadata
        self._curve_metadata[addr] = CurvePoolMetadata(
            address=addr,
            chain_id=chain_id,
            tokens=tuple(get_checksum_address(t) for t in tokens),
            a_coefficient=a_coefficient,
            fee=fee,
            admin_fee=admin_fee,
            lp_token=None,
            initial_a=None,
            future_a=None,
            initial_a_time=None,
            future_a_time=None,
            base_pool=None,
            tokens_underlying=None,
            use_lending=tuple(False for _ in tokens),
            precision_multipliers=None,
            fee_gamma=None,
            mid_fee=None,
            out_fee=None,
            gamma=None,
            offpeg_fee_multiplier=None,
        )
        
        # Register state
        self._curve_states[(addr, block)] = CurvePoolState(
            block=block,
            balances=tuple(balances),
        )
        
        # Register fetchers (or use defaults)
        self._fetcher_factories[addr] = fetchers or FetcherFactories(
            timestamp_fetcher=lambda block: 1700000000,
        )
        
        return self
    
    # === ChainDataSource Protocol Implementation ===
    
    def get_token_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> TokenMetadata:
        key = (address, chain_id)
        if key not in self._tokens:
            raise ValueError(f"Token not registered: {address} on chain {chain_id}")
        return self._tokens[key]
    
    def get_v2_pool_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> V2PoolMetadata:
        if address not in self._v2_metadata:
            raise ValueError(f"V2 pool not registered: {address}")
        return self._v2_metadata[address]
    
    def get_v2_pool_state(
        self,
        address: ChecksumAddress,
        block: int,
    ) -> V2PoolState:
        key = (address, block)
        if key not in self._v2_states:
            # Fall back to any available state for this pool
            for (addr, blk), state in self._v2_states.items():
                if addr == address:
                    return state
            raise ValueError(f"No state registered for V2 pool {address}")
        return self._v2_states[key]
    
    def get_curve_pool_metadata(
        self,
        address: ChecksumAddress,
        chain_id: int,
    ) -> CurvePoolMetadata:
        if address not in self._curve_metadata:
            raise ValueError(f"Curve pool not registered: {address}")
        return self._curve_metadata[address]
    
    def get_curve_pool_state(
        self,
        address: ChecksumAddress,
        block: int,
    ) -> CurvePoolState:
        key = (address, block)
        if key not in self._curve_states:
            for (addr, blk), state in self._curve_states.items():
                if addr == address:
                    return state
            raise ValueError(f"No state registered for Curve pool {address}")
        return self._curve_states[key]
    
    def get_curve_fetcher_factories(
        self,
        address: ChecksumAddress,
        chain_id: int,
        metadata: CurvePoolMetadata,
    ) -> FetcherFactories:
        return self._fetcher_factories.get(address, FetcherFactories())
```

### Refactored Bot

```python
# src/degenbot/bot.py (modified)

class Bot:
    """Orchestrator that delegates I/O to a ChainDataSource."""
    
    def __init__(
        self,
        config: DegenbotConfig,
        chain_data: ChainDataSource | None = None,
    ):
        self.config = config
        self.connections = ConnectionManager()
        self.db = DatabaseSessionManager(...)
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._managers: dict[tuple[ChainId, str], AbstractPoolManager] = {}
        
        # Use provided chain data source or create default
        if chain_data is None:
            provider = self.connections.get_provider(config.default_chain_id)
            web3 = self.connections.get_web3(config.default_chain_id)
            chain_data = OnChainDataSource(web3, provider)
        
        self.chain_data = chain_data
        
        # Check database migration version
        self._check_database_version()
    
    def build_erc20token(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        silent: bool = False,
    ) -> Erc20Token:
        """Build token using chain data source."""
        address = get_checksum_address(address)
        chain_id = chain_id or self.connections.default_chain_id
        
        # Check registry
        if existing := self.tokens.get(address, chain_id):
            return existing
        
        # Fetch metadata via chain data source
        metadata = self.chain_data.get_token_metadata(address, chain_id)
        
        # Construct I/O-free token
        token = Erc20Token(
            address=metadata.address,
            chain_id=metadata.chain_id,
            name=metadata.name,
            symbol=metadata.symbol,
            decimals=metadata.decimals,
        )
        
        self.tokens.add(address, chain_id, token)
        
        if not silent:
            logger.info(f"• {token.symbol} ({token.name})")
        
        return token
    
    def build_v2_pool(
        self,
        pool_address: str,
        *,
        chain_id: int | None = None,
        state_block: int | None = None,
        silent: bool = False,
    ) -> UniswapV2Pool:
        """Build V2 pool using chain data source."""
        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self.connections.default_chain_id
        
        # Check registry
        if existing := self.pools.get(pool_address, chain_id):
            return existing
        
        state_block = state_block or self.chain_data.get_current_block(chain_id)
        
        # Fetch metadata (immutable)
        metadata = self.chain_data.get_v2_pool_metadata(pool_address, chain_id)
        
        # Build tokens
        token0 = self.build_erc20token(metadata.token0, chain_id=chain_id, silent=silent)
        token1 = self.build_erc20token(metadata.token1, chain_id=chain_id, silent=silent)
        
        # Fetch state (mutable)
        state = self.chain_data.get_v2_pool_state(pool_address, state_block)
        
        # Construct I/O-free pool
        pool = UniswapV2Pool(
            address=metadata.address,
            chain_id=metadata.chain_id,
            token0=token0,
            token1=token1,
            factory=metadata.factory,
            fee_token0=metadata.fee_token0,
            fee_token1=metadata.fee_token1,
            reserves_token0=state.reserves_token0,
            reserves_token1=state.reserves_token1,
            state_block=state.block,
            deployer_address=metadata.deployer,
            init_hash=metadata.init_hash,
        )
        
        self.pools.add(pool_address, chain_id, pool)
        
        if not silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {state.reserves_token0}")
            logger.info(f"• Token 1: {token1} - Reserves: {state.reserves_token1}")
        
        return pool
    
    def build_curve_pool(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        state_block: int | None = None,
        silent: bool = False,
    ) -> CurveStableswapPool:
        """Build Curve pool using chain data source."""
        address = get_checksum_address(address)
        chain_id = chain_id or self.connections.default_chain_id
        
        # Check registry
        if existing := self.pools.get(address, chain_id):
            return existing
        
        state_block = state_block or self.chain_data.get_current_block(chain_id)
        
        # Fetch metadata (immutable)
        metadata = self.chain_data.get_curve_pool_metadata(address, chain_id)
        
        # Build tokens
        tokens = tuple(
            self.build_erc20token(addr, chain_id=chain_id, silent=silent)
            for addr in metadata.tokens
        )
        
        # Build base pool (recursive)
        base_pool = None
        if metadata.base_pool:
            base_pool = self.build_curve_pool(metadata.base_pool, chain_id=chain_id, state_block=state_block)
        
        # Fetch state (mutable)
        state = self.chain_data.get_curve_pool_state(address, state_block)
        
        # Get fetcher factories
        fetchers = self.chain_data.get_curve_fetcher_factories(address, chain_id, metadata)
        
        # Construct I/O-free pool
        pool = CurveStableswapPool(
            address=metadata.address,
            tokens=tokens,
            a_coefficient=metadata.a_coefficient,
            fee=metadata.fee,
            admin_fee=metadata.admin_fee,
            balances=state.balances,
            chain_id=chain_id,
            state_block=state.block,
            # A ramping
            initial_a_coefficient=metadata.initial_a,
            future_a_coefficient=metadata.future_a,
            initial_a_coefficient_time=metadata.initial_a_time,
            future_a_coefficient_time=metadata.future_a_time,
            # Metapool
            base_pool=base_pool,
            tokens_underlying=tuple(self.build_erc20token(a, chain_id) for a in metadata.tokens_underlying) if metadata.tokens_underlying else None,
            # Lending
            use_lending=metadata.use_lending,
            precision_multipliers=metadata.precision_multipliers,
            # Crypto
            fee_gamma=metadata.fee_gamma,
            mid_fee=metadata.mid_fee,
            out_fee=metadata.out_fee,
            gamma=metadata.gamma,
            offpeg_fee_multiplier=metadata.offpeg_fee_multiplier,
            # Fetchers
            **fetchers.__dict__,
        )
        
        self.pools.add(address, chain_id, pool)
        
        if not silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Tokens: {[t.symbol for t in pool.tokens]}")
            logger.info(f"• A: {pool.a_coefficient}")
        
        return pool
```

## Implementation Phases

### Phase 1: Extract Data Classes (1 day)

**Goal:** Create all dataclass definitions for metadata, state, and fetchers.

**Files to create:**
```
src/degenbot/chain_data/
    __init__.py
    protocol.py      # Protocol + dataclasses
    types.py         # Type aliases
```

**Tasks:**
1. Create `src/degenbot/chain_data/` directory
2. Create `protocol.py` with all dataclasses:
   - TokenMetadata
   - V2PoolMetadata, V2PoolState
   - V3PoolMetadata, V3PoolState
   - V4PoolMetadata, V4PoolState
   - CurvePoolMetadata, CurvePoolState
   - FetcherFactories
3. Create ChainDataSource protocol (empty methods with `...` for now)
4. Write unit tests for dataclasses (test immutability, field validation)

**Verification:**
```bash
just test-python tests/chain_data/test_protocol.py
```

**Definition of Done:**
- All dataclasses defined and tested
- Protocol compiles without errors
- No changes to existing code yet

---

### Phase 2: Implement OnChainDataSource (2-3 days)

**Goal:** Move RPC logic from Bot to OnChainDataSource.

**Files to create:**
```
src/degenbot/chain_data/
    on_chain.py      # OnChainDataSource implementation
```

**Approach:** Extract one pool type at a time.

#### Step 2a: V2 Pools (half day)

1. Create `OnChainDataSource` class with `__init__(self, web3, provider)`
2. Implement `get_v2_pool_metadata()`:
   - Move lines 340-420 from `bot.py` (factory, token0/1, fee detection)
   - Add helper methods `_call()`, `_try_call()`
3. Implement `get_v2_pool_state()`:
   - Move reserves fetch from Bot
4. Write integration test against fork:
   ```python
   def test_on_chain_data_source_v2_pool(fork_mainnet_full):
       provider = ProviderAdapter.from_web3(fork_mainnet_full.w3)
       source = OnChainDataSource(fork_mainnet_full.w3, provider)
       
       metadata = source.get_v2_pool_metadata("0xAE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5", 1)
       assert metadata.factory == "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
       
       state = source.get_v2_pool_state("0xAE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5", 18_000_000)
       assert state.reserves_token0 > 0
   ```

#### Step 2b: V3 Pools (half day)

1. Implement `get_v3_pool_metadata()`:
   - Move lines 720-850 from `bot.py` (factory, tokens, fee, tick_spacing)
2. Implement `get_v3_pool_state()`:
   - Move slot0, liquidity, tick bitmap logic from Bot
   - Handle DB snapshot loading vs on-chain fetch
3. Implement `get_v3_fetcher_factories()`:
   - Move tick data fetcher creation from Bot
4. Write integration test against fork

#### Step 2c: V4 Pools (half day)

1. Implement `get_v4_pool_metadata()`:
   - Move lines 990-1100 from `bot.py`
2. Implement `get_v4_pool_state()`:
   - Move state view fetches from Bot
3. Write integration test against fork

#### Step 2d: Curve Pools (1 day)

1. Implement `get_curve_pool_metadata()`:
   - Move lines 1240-1520 from `bot.py`
   - This is the largest: coin iteration, lending detection, crypto params, metapool
2. Implement `get_curve_pool_state()`:
   - Move balances fetch
3. Implement `get_curve_fetcher_factories()`:
   - Move all `_make_curve_*_fetcher()` methods from Bot
   - timestamp, virtual_price, D, gamma, price_scale, etc.
4. Write integration test against fork

**Verification:**
```bash
just test-python tests/chain_data/test_on_chain_source.py
```

**Definition of Done:**
- All pool types supported
- All RPC calls moved from Bot to OnChainDataSource
- Integration tests pass against fork
- No behavior changes (same data returned)

---

### Phase 3: Implement FakeDataSource (1 day)

**Goal:** Create test-friendly implementation with registration API.

**Files to create:**
```
src/degenbot/chain_data/
    fake.py          # FakeDataSource implementation
```

**Tasks:**
1. Create `FakeDataSource` class
2. Implement registration API:
   - `add_token()` - auto-register tokens
   - `add_v2_pool()` - auto-register tokens, metadata + state
   - `add_v3_pool()` - include tick data
   - `add_curve_pool()` - include fetchers
3. Implement ChainDataSource protocol methods:
   - Look up registered data
   - Raise clear errors for missing data
4. Write unit tests:
   ```python
   def test_fake_data_source_v2_pool():
       source = FakeDataSource()
       source.add_token("0x...", "DAI", "DAI", 18)
       source.add_token("0x...", "USDC", "USDC", 6)
       source.add_v2_pool(
           address="0x...",
           token0="0x...",
           token1="0x...",
           reserves0=10**20,
           reserves1=10**20,
       )
       
       metadata = source.get_v2_pool_metadata("0x...", 1)
       assert metadata.fee_token0 == Fraction(3, 1000)
       
       state = source.get_v2_pool_state("0x...", 18_000_000)
       assert state.reserves_token0 == 10**20
   ```
5. Write test showing Bot using FakeDataSource:
   ```python
   def test_bot_with_fake_data_source():
       fake_data = FakeDataSource()
       fake_data.add_token("0x...", "DAI", "DAI", 18)
       fake_data.add_v2_pool("0x...", "0x...", "0x...", 10**20, 10**20)
       
       bot = Bot(config={}, chain_data=fake_data)
       pool = bot.build_v2_pool("0x...")
       
       assert pool.reserves_token0 == 10**20
   ```

**Verification:**
```bash
just test-python tests/chain_data/test_fake_source.py
```

**Definition of Done:**
- All pool types supported
- Fluent API (returns self for chaining)
- Clear error messages for missing data
- Tests demonstrate Bot using fake source

---

### Phase 4: Refactor Bot (1-2 days)

**Goal:** Inject ChainDataSource into Bot, delegate all I/O.

**Files to modify:**
```
src/degenbot/bot.py
```

**Approach:** Incremental migration, one pool type at a time.

#### Step 4a: Add chain_data parameter (10 min)

1. Add `chain_data: ChainDataSource | None = None` parameter to `Bot.__init__()`
2. Create default `OnChainDataSource` if None provided
3. Store as `self.chain_data`

#### Step 4b: Refactor build_erc20token (30 min)

1. Replace provider calls with `self.chain_data.get_token_metadata()`
2. Keep registry logic unchanged
3. Run tests

#### Step 4c: Refactor build_v2_pool (1 hour)

1. Replace metadata RPC calls with `self.chain_data.get_v2_pool_metadata()`
2. Replace state RPC calls with `self.chain_data.get_v2_pool_state()`
3. Keep registry, token building, pool construction unchanged
4. Run all V2 tests

#### Step 4d: Refactor build_v3_pool (1 hour)

1. Replace metadata calls with `self.chain_data.get_v3_pool_metadata()`
2. Replace state calls with `self.chain_data.get_v3_pool_state()`
3. Replace fetcher creation with `self.chain_data.get_v3_fetcher_factories()`
4. Run all V3 tests

#### Step 4e: Refactor build_v4_pool (1 hour)

1. Similar pattern to V3
2. Run all V4 tests

#### Step 4f: Refactor build_curve_pool (2 hours)

1. Replace metadata calls with `self.chain_data.get_curve_pool_metadata()`
2. Replace state calls with `self.chain_data.get_curve_pool_state()`
3. Replace fetcher creation with `self.chain_data.get_curve_fetcher_factories()`
4. Delete all `_make_curve_*_fetcher()` methods from Bot
5. Run all Curve tests

#### Step 4g: Update update() methods (1 hour)

1. Refactor `_update_v2_pool()` to use `self.chain_data.get_v2_pool_state()`
2. Same for V3, V4, Curve
3. Run all update tests

**Verification:**
```bash
just test-python  # All existing tests still pass
```

**Definition of Done:**
- Bot accepts chain_data parameter
- All build_* methods delegate to chain_data
- All _update_* methods delegate to chain_data
- No provider.call() or w3.eth.call() in Bot
- All existing tests pass unchanged

---

### Phase 5: Update Tests (incremental)

**Goal:** Replace mock-based tests with FakeDataSource.

**Files to modify:**
```
tests/bot/test_bot.py
tests/bot/test_curve_pool_manager.py
tests/bot/test_v2_pool_manager.py
# ... other test files
```

**Approach:** Update tests as you encounter them, not wholesale rewrite.

**Pattern to replace:**
```python
# OLD: Mocking provider
def test_bot_builds_pool():
    provider = MagicMock()
    provider.call.side_effect = [...]
    bot.connections.register_provider(provider)
    pool = bot.build_v2_pool("0x...")
```

**New pattern:**
```python
# NEW: FakeDataSource
def test_bot_builds_pool():
    fake_data = FakeDataSource()
    fake_data.add_token("0x...", "DAI", "DAI", 18)
    fake_data.add_v2_pool("0x...", "0x...", "0x...", 10**20, 10**20)
    
    bot = Bot(config={}, chain_data=fake_data)
    pool = bot.build_v2_pool("0x...")
```

**Definition of Done:**
- Key tests updated to use FakeDataSource
- Keep Fake pools for complex state generation (different purpose)
- All tests pass

---

### Phase 6: Add Recording/Replay (optional, future)

**Goal:** Enable recording production scenarios and replaying in tests.

**Files to create:**
```
src/degenbot/chain_data/
    recording.py     # RecordingDataSource
    replaying.py     # ReplayingDataSource
```

**Implementation:**
```python
class RecordingDataSource:
    """Wraps another source and records all fetches."""
    
    def __init__(self, source: ChainDataSource, output_path: str):
        self.source = source
        self.output_path = output_path
        self.recording: list[dict] = []
    
    def get_v2_pool_metadata(self, address, chain_id):
        metadata = self.source.get_v2_pool_metadata(address, chain_id)
        self.recording.append({
            "method": "get_v2_pool_metadata",
            "args": {"address": address, "chain_id": chain_id},
            "result": metadata,
        })
        return metadata
    
    def save(self):
        with open(self.output_path, 'w') as f:
            json.dump(self.recording, f, indent=2, cls=DataclassEncoder)


class ReplayingDataSource:
    """Replays recorded fetches."""
    
    def __init__(self, recording_path: str):
        with open(recording_path) as f:
            self.recording = json.load(f, cls=DataclassDecoder)
        self._index = 0
    
    def get_v2_pool_metadata(self, address, chain_id):
        entry = self.recording[self._index]
        self._index += 1
        return entry["result"]
```

**Usage:**
```python
# Record once on fork
bot = Bot(config, chain_data=RecordingDataSource(
    OnChainDataSource(provider),
    "tests/fixtures/tripool_recording.json"
))
pool = bot.build_curve_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")

# Replay in tests
bot = Bot(config, chain_data=ReplayingDataSource("tests/fixtures/tripool_recording.json"))
pool = bot.build_curve_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
# Fast, deterministic, uses real production data
```

---

## Files Created/Modified

### Created
```
src/degenbot/chain_data/
    __init__.py
    protocol.py          # ChainDataSource protocol + dataclasses
    types.py             # Type aliases
    on_chain.py          # OnChainDataSource implementation
    fake.py              # FakeDataSource implementation

tests/chain_data/
    __init__.py
    test_protocol.py     # Dataclass tests
    test_on_chain.py     # Integration tests against fork
    test_fake.py         # FakeDataSource tests
```

### Modified
```
src/degenbot/bot.py     # Inject chain_data, delegate I/O

tests/bot/test_bot.py   # Update to use FakeDataSource
tests/curve/test_curve_pool_manager.py
# ... other test files incrementally
```

---

## Testing Strategy

### Unit Tests
- Dataclass immutability, field validation
- FakeDataSource registration API
- Error messages for missing data

### Integration Tests (against fork)
- OnChainDataSource fetches correct data
- Bot + OnChainDataSource = same behavior as before

### Test Migration
- Replace mock-based tests with FakeDataSource
- Keep existing tests passing during migration

### Example Test File

```python
# tests/chain_data/test_fake.py

import pytest
from degenbot.chain_data.fake import FakeDataSource
from degenbot.chain_data.protocol import FetcherFactories
from fractions import Fraction


class TestFakeDataSource:
    """Test FakeDataSource registration and retrieval."""
    
    def test_add_token(self):
        """Tokens can be registered and retrieved."""
        source = FakeDataSource()
        source.add_token("0x6B175474E89094C44Da98b954EedeAC495271d0F", "DAI", "DAI", 18)
        
        metadata = source.get_token_metadata("0x6B175474E89094C44Da98b954EedeAC495271d0F", 1)
        assert metadata.name == "DAI"
        assert metadata.symbol == "DAI"
        assert metadata.decimals == 18
    
    def test_add_v2_pool_auto_registers_tokens(self):
        """add_v2_pool auto-registers tokens if not present."""
        source = FakeDataSource()
        source.add_v2_pool(
            address="0xAE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5",
            token0="0x6B175474E89094C44Da98b954EedeAC495271d0F",
            token1="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            reserves0=10**20,
            reserves1=10**20,
        )
        
        # Tokens auto-registered
        token0 = source.get_token_metadata("0x6B175474E89094C44Da98b954EedeAC495271d0F", 1)
        assert token0.symbol == "0x6B175..."
        
        # Pool metadata retrievable
        metadata = source.get_v2_pool_metadata("0xAE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5", 1)
        assert metadata.fee_token0 == Fraction(3, 1000)
        
        # Pool state retrievable
        state = source.get_v2_pool_state("0xAE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5", 18_000_000)
        assert state.reserves_token0 == 10**20
    
    def test_missing_pool_raises_clear_error(self):
        """Fetching unregistered pool raises helpful error."""
        source = FakeDataSource()
        
        with pytest.raises(ValueError, match="V2 pool not registered"):
            source.get_v2_pool_metadata("0x1234...", 1)


class TestBotWithFakeDataSource:
    """Test Bot using FakeDataSource instead of mocks."""
    
    def test_build_v2_pool_with_fake_data(self):
        """Bot can build V2 pool from fake data."""
        fake_data = FakeDataSource()
        fake_data.add_token("0x...", "DAI", "DAI", 18)
        fake_data.add_token("0x...", "USDC", "USDC", 6)
        fake_data.add_v2_pool("0xpool...", "0x...", "0x...", 10**20, 10**20)
        
        bot = Bot(config={}, chain_data=fake_data)
        pool = bot.build_v2_pool("0xpool...")
        
        assert pool.reserves_token0 == 10**20
        assert pool.token0.symbol == "DAI"
```

---

## Risks and Mitigations

### Risk 1: Breaking existing tests
**Mitigation:** Incremental migration, one pool type at a time. All existing tests must pass before moving to next phase.

### Risk 2: Subtle behavior differences
**Mitigation:** OnChainDataSource should be a pure extraction - no logic changes. Use comparison tests:
```python
def test_on_chain_source_matches_old_bot_behavior(fork_mainnet_full):
    # Build pool using old Bot (pre-refactor)
    old_pool = OldBot(...).build_v2_pool("0x...")
    
    # Build pool using new Bot + OnChainDataSource
    source = OnChainDataSource(...)
    new_pool = Bot(..., chain_data=source).build_v2_pool("0x...")
    
    # Verify identical
    assert new_pool.reserves_token0 == old_pool.reserves_token0
    assert new_pool.factory == old_pool.factory
```

### Risk 3: Complex pool variants (Camelot, Aerodrome, etc.)
**Mitigation:** Handle variants in metadata dataclasses with optional fields:
```python
@dataclass(frozen=True)
class V2PoolMetadata:
    # ... common fields
    stable: bool = False  # Aerodrome
    stable_swap: bool = False  # Camelot
    fee_denominator: int | None = None  # Camelot
```

### Risk 4: Database caching logic
**Mitigation:** Move DB logic into OnChainDataSource methods:
```python
def get_v2_pool_metadata(self, address, chain_id):
    # Try DB first
    if cached := self._try_db_fetch(address, chain_id):
        return cached
    
    # Fall back to chain
    return self._fetch_from_chain(address, chain_id)
```

---

## Success Criteria

### Must Have
- ✅ ChainDataSource protocol defined
- ✅ OnChainDataSource implements all methods
- ✅ FakeDataSource provides test-friendly API
- ✅ Bot accepts chain_data parameter
- ✅ All existing tests pass
- ✅ No provider.call() or w3.eth.call() in Bot

### Should Have
- ✅ Clear error messages from FakeDataSource
- ✅ Fluent API for test setup (chaining)
- ✅ Auto-registration of tokens when adding pools
- ✅ Integration tests against fork

### Nice to Have
- ⚪ RecordingDataSource for capturing scenarios
- ⚪ ReplayingDataSource for deterministic tests
- ⚪ All mock-based tests replaced with FakeDataSource

---

## Definition of Done

- [ ] All dataclasses defined in `chain_data/protocol.py`
- [ ] ChainDataSource protocol defined with all methods
- [ ] OnChainDataSource implements protocol (all pool types)
- [ ] FakeDataSource implements protocol with registration API
- [ ] Bot accepts `chain_data` parameter
- [ ] Bot.build_erc20token() delegates to chain_data
- [ ] Bot.build_v2_pool() delegates to chain_data
- [ ] Bot.build_v3_pool() delegates to chain_data
- [ ] Bot.build_v4_pool() delegates to chain_data
- [ ] Bot.build_curve_pool() delegates to chain_data
- [ ] Bot.update() methods delegate to chain_data
- [ ] All existing tests pass
- [ ] New tests demonstrate Bot + FakeDataSource
- [ ] No RPC calls remaining in Bot
- [ ] Documentation updated (CLAUDE.md or similar)

---

## References

- **I/O-free pool architecture:** `docs/architecture/io-free-pools.md`
- **Current Bot implementation:** `src/degenbot/bot.py`
- **Pool types:**
  - V2: `src/degenbot/uniswap/v2_liquidity_pool.py`
  - V3: `src/degenbot/uniswap/v3_liquidity_pool.py`
  - V4: `src/degenbot/uniswap/v4_liquidity_pool.py`
  - Curve: `src/degenbot/curve/curve_stableswap_liquidity_pool.py`
- **Design discussion:** `/home/ralph/.agents/skills/diagnose/DATASOURCE_DESIGN_POOL_TYPES.md`
- **Summary:** `/home/ralph/.agents/skills/diagnose/CHAIN_DATA_SOURCE_FINAL.md`

---

## Estimated Effort

| Phase | Effort | Risk |
|-------|--------|------|
| Phase 1: Data classes | 1 day | Low |
| Phase 2: OnChainDataSource | 2-3 days | Medium (code extraction) |
| Phase 3: FakeDataSource | 1 day | Low |
| Phase 4: Refactor Bot | 1-2 days | Medium (must not break) |
| Phase 5: Update tests | Incremental | Low |
| Phase 6: Record/Replay | Optional | Low |

**Total: ~5-7 days core work**

---

## Implementation Notes

### Naming Rationale

**ChainDataSource** was chosen over alternatives because:
- "Chain" prefix makes it specific (blockchain data, not generic)
- "Source" is idiomatic (cf. `javax.sql.DataSource`)
- Portable (extends to TokenDataSource, MarketDataSource, etc.)
- Reads naturally: "OnChainDataSource" = source that fetches from chain

### Why Three Categories of Data?

**Metadata (immutable):**
- Fetched once, cached forever
- Can be stored in DB
- Examples: factory address, fee, tick_spacing

**State (mutable):**
- Fetched per block
- Changes frequently
- Examples: reserves, sqrt_price, liquidity

**Fetchers (callbacks):**
- Injected into pools for on-demand fetching
- Pool calls them when needed
- Examples: timestamp_fetcher, D_fetcher

This separation enables:
- Different caching strategies per category
- Clear data dependencies
- Test-friendly API (provide only what's needed)

### Why Keep Fake Pools?

`FakeCurveStableswapPool` and `FakeV3PoolWithTicks` serve different purposes than `FakeDataSource`:

| Purpose | FakeDataSource | Fake Pools |
|---------|----------------|------------|
| Simple pool tests | ✅ Register pool data | ⚠️ Overkill |
| Complex state generation | ⚠️ Verbose | ✅ Ergonomic API |
| Solver integration | ❌ Different concern | ✅ Protocol methods |
| Multi-range V3 | ⚠️ Manual tick setup | ✅ `TickRangeDefinition` |

**Keep both.** They're complementary, not redundant.

---

*Plan created: 2026-05-09*
*Based on design discussion about I/O-free architecture testing improvements*
