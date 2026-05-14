# Plan 018: Decompose CurvePoolBuilder.build() into Detection Sub-Modules

**Status: READY**

## Overview

Break `CurvePoolBuilder.build()` (~400 lines) into focused detection sub-modules, each testable with a fake provider. The `build()` method becomes a ~50-line orchestrator that calls detectors in sequence and feeds results to the `CurveStableswapPool` constructor.

## Files Involved

**Existing:**
- `src/degenbot/builders/curve_pool_builder.py` (~415 lines) — the monolith
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — pool constructor (consumer)

**New:**
- `src/degenbot/curve/detection/coin_discovery.py` — coin address + balance enumeration
- `src/degenbot/curve/detection/lending_detector.py` — lending token detection (cToken, yToken)
- `src/degenbot/curve/detection/metapool_detector.py` — metapool + base pool resolution
- `src/degenbot/curve/detection/crypto_detector.py` — crypto pool parameter detection
- `src/degenbot/curve/detection/__init__.py`
- `src/degenbot/curve/detection/types.py` — shared dataclasses for detection results

**Modified:**
- `src/degenbot/builders/curve_pool_builder.py` — body of `build()` replaced with orchestrator calls

**Tests:**
- `tests/curve/detection/test_coin_discovery.py`
- `tests/curve/detection/test_lending_detector.py`
- `tests/curve/detection/test_metapool_detector.py`
- `tests/curve/detection/test_crypto_detector.py`
- `tests/curve/test_curve_pool_builder.py` — updated to test orchestrator

## Problem

`CurvePoolBuilder.build()` is ~400 lines of sequential I/O with deeply nested try/except blocks for heuristic detection. Understanding what data flows into the `CurveStableswapPool` constructor requires reading all 400 lines because:

1. **Coin discovery** (~80 lines) — iterates up to 8 coins, tries `coins(uint256)` then `coins(int128)`, fetches balances — all intermixed with prototype tracking.
2. **A/fetching** (~20 lines) — straightforward, but tangled in the sequence.
3. **A ramping** (~25 lines) — optional, wrapped in try/except.
4. **Lending token detection** (~60 lines) — checks `isCToken()`, `underlying()`, `token()`, fetches underlying decimals — nested try/except with `precision_multiplier_overrides` handling.
5. **Crypto pool detection** (~50 lines) — checks `fee_gamma()`, if positive fetches `mid_fee`, `out_fee`, `gamma` — nested try/excepts.
6. **Off-peg fee multiplier** (~10 lines) — another optional fetch.
7. **LP token** (~20 lines) — iterates registry addresses to find `get_lp_token()`.
8. **Metapool detection** (~80 lines) — checks `is_meta()`, `base_pool()`, `get_underlying_coins()`, recursive `self.build()` for base pool — deeply nested try/except.

There's no way to test a single detection path in isolation. The `build()` method is the interface and the implementation is nearly as complex as understanding the raw RPC calls yourself. Applying the deletion test: if you deleted `build()`, the complexity of probing every Curve pool variant wouldn't vanish — it would reappear as copy-pasted I/O across callers. The module is **shallow**.

## Solution

Extract each detection concern into a focused module with a narrow interface. Each detector receives what it needs (a provider/web3 and pool address) and returns a frozen dataclass.

### Detection result types

```python
# src/degenbot/curve/detection/types.py

from dataclasses import dataclass
from eth_typing import ChecksumAddress


@dataclass(frozen=True)
class CoinDiscoveryResult:
    """Result of coin enumeration for a Curve pool."""
    token_addresses: tuple[ChecksumAddress, ...]
    balances: tuple[int, ...]
    coin_prototype: str  # "coins(uint256)" or "coins(int128)"
    balance_prototype: str  # "balances(uint256)" or "balances(int128)"


@dataclass(frozen=True)
class LendingDetectionResult:
    """Result of lending token detection for a Curve pool."""
    use_lending: tuple[bool, ...]
    precision_multipliers: tuple[int, ...] | None  # None if no overrides needed


@dataclass(frozen=True)
class MetapoolDetectionResult:
    """Result of metapool detection for a Curve pool."""
    is_meta: bool
    base_pool_address: ChecksumAddress | None  # None if not a metapool
    tokens_underlying: tuple[ChecksumAddress, ...] | None  # None if not a metapool


@dataclass(frozen=True)
class CryptoDetectionResult:
    """Result of crypto pool parameter detection."""
    is_crypto: bool  # True if fee_gamma > 0
    fee_gamma: int | None
    mid_fee: int | None
    out_fee: int | None
    gamma: int | None
    offpeg_fee_multiplier: int | None


@dataclass(frozen=True)
class ARampingResult:
    """Result of A ramping parameter detection."""
    initial_a: int | None
    initial_a_time: int | None
    future_a: int | None
    future_a_time: int | None
    has_ramping: bool  # True if all four values were fetched


@dataclass(frozen=True)
class LpTokenResult:
    """Result of LP token address lookup."""
    lp_token_address: ChecksumAddress | None
```

### Sub-module interfaces

```python
# src/degenbot/curve/detection/coin_discovery.py

def discover_coins(
    w3: Any,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int,
    max_coins: int = 8,
) -> CoinDiscoveryResult:
    """
    Enumerate coins and balances for a Curve pool.

    Tries coins(uint256) first, falls back to coins(int128).
    Stops at first zero address or revert.
    """
    ...


# src/degenbot/curve/detection/lending_detector.py

def detect_lending_tokens(
    w3: Any,
    pool_address: ChecksumAddress,
    token_addresses: tuple[ChecksumAddress, ...],
    tokens: tuple[Erc20Token, ...],
    *,
    block_identifier: int,
) -> LendingDetectionResult:
    """
    Detect lending tokens (cTokens, yTokens) and compute precision multipliers overrides.

    Uses isCToken() and token() methods as primary detection,
    avoiding exchangeRateStored/getPricePerFullShare (false positives from WETH).
    """
    ...


# src/degenbot/curve/detection/metapool_detector.py

def detect_metapool(
    w3: Any,
    pool_address: ChecksumAddress,
    token_addresses: tuple[ChecksumAddress, ...],
    *,
    block_identifier: int,
) -> MetapoolDetectionResult:
    """
    Detect whether a Curve pool is a metapool and resolve base pool info.

    Checks Curve registry and factory via is_meta(), then resolves
    base pool address and underlying coins.
    """
    ...


# src/degenbot/curve/detection/crypto_detector.py

def detect_crypto_params(
    w3: Any,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int,
) -> CryptoDetectionResult:
    """
    Detect crypto pool parameters (fee_gamma, mid_fee, out_fee, gamma, offpeg_fee_multiplier).

    A pool is identified as crypto if fee_gamma() > 0.
    All parameters default to None for non-crypto pools.
    """
    ...


# Also in crypto_detector.py:

def detect_a_ramping(
    w3: Any,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int,
) -> ARampingResult:
    """
    Detect A coefficient ramping parameters.

    Not all pools support initial_A()/future_A() — they're optional.
    """
    ...
```

### Orchestrated build()

```python
# src/degenbot/builders/curve_pool_builder.py (after refactoring)

class CurvePoolBuilder:
    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> CurveStableswapPool:
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._connections.default_chain_id
        w3 = self._connections.get_web3(chain_id)
        provider = self._connections.get_provider(chain_id)
        state_block = state_block or provider.get_block_number()

        # 1. Discover coins and balances
        coins = discover_coins(w3, pool_address, block_identifier=state_block)

        # 2. Build tokens
        tokens = tuple(
            self._erc20_builder.build(addr, chain_id=chain_id, silent=silent)
            for addr in coins.token_addresses
        )

        # 3. Fetch A, fee, admin_fee
        a_coefficient, fee, admin_fee = _fetch_pool_params(w3, pool_address, block_identifier=state_block)

        # 4. Detect A ramping
        a_ramping = detect_a_ramping(w3, pool_address, block_identifier=state_block)

        # 5. Get block timestamp
        block = provider.get_block(state_block)
        create_timestamp = block["timestamp"]

        # 6. Detect lending tokens
        lending = detect_lending_tokens(
            w3, pool_address, coins.token_addresses, tokens, block_identifier=state_block,
        )

        # 7. Detect crypto pool parameters
        crypto = detect_crypto_params(w3, pool_address, block_identifier=state_block)

        # 8. Find LP token
        lp_token_address = _find_lp_token(w3, pool_address, block_identifier=state_block)

        # 9. Detect metapool
        metapool = detect_metapool(w3, pool_address, coins.token_addresses, block_identifier=state_block)

        # 10. Build base pool and underlying tokens (if metapool)
        base_pool, tokens_underlying = self._resolve_metapool(
            metapool, chain_id, state_block, silent,
        )

        # 11. Build LP token
        lp_token = (
            self._erc20_builder.build(lp_token_address, chain_id=chain_id, silent=silent)
            if lp_token_address else None
        )

        # 12. Skip broken pools
        if len(tokens) < 2:
            raise BrokenPool()

        # 13. Create fetchers and construct pool
        fetchers = CurveFetcherFactory(connections=self._connections, chain_id=chain_id)
        pool = CurveStableswapPool(
            address=pool_address,
            tokens=tokens,
            a_coefficient=a_coefficient,
            fee=fee,
            admin_fee=admin_fee,
            balances=coins.balances,
            chain_id=chain_id,
            state_block=state_block,
            state_cache_depth=state_cache_depth,
            initial_a_coefficient=a_ramping.initial_a,
            future_a_coefficient=a_ramping.future_a,
            initial_a_coefficient_time=a_ramping.initial_a_time,
            future_a_coefficient_time=a_ramping.future_a_time,
            create_timestamp=create_timestamp,
            lp_token=lp_token,
            base_pool=base_pool,
            tokens_underlying=tokens_underlying,
            use_lending=lending.use_lending,
            precision_multipliers=lending.precision_multipliers,
            fee_gamma=crypto.fee_gamma,
            mid_fee=crypto.mid_fee,
            out_fee=crypto.out_fee,
            gamma=crypto.gamma,
            offpeg_fee_multiplier=crypto.offpeg_fee_multiplier,
            virtual_price_fetcher=fetchers.virtual_price_fetcher(
                pool_address,
                base_pool_address=metapool.base_pool_address if metapool.is_meta else None,
            ),
            base_virtual_price_fetcher=fetchers.base_virtual_price_fetcher(pool_address),
            timestamp_fetcher=fetchers.timestamp_fetcher(),
            redemption_price_fetcher=fetchers.redemption_price_fetcher(pool_address),
            admin_balances_fetcher=fetchers.admin_balances_fetcher(pool_address),
            block_number_fetcher=fetchers.block_number_fetcher(),
            total_supply_fetcher=fetchers.total_supply_fetcher(),
            token_balance_fetcher=fetchers.token_balance_fetcher(),
            provider_call=fetchers.provider_call(),
            D_fetcher=fetchers.D_fetcher(pool_address) if crypto.is_crypto else None,
            gamma_fetcher=fetchers.gamma_fetcher(pool_address) if crypto.is_crypto else None,
            price_scale_fetcher=(
                fetchers.price_scale_fetcher(pool_address, len(tokens)) if crypto.is_crypto else None
            ),
        )

        self._pools.add(pool, chain_id=chain_id, pool_address=pool.address)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Tokens: {[t.symbol for t in pool.tokens]}")
            logger.info(f"• A: {pool.a_coefficient}")
            logger.info(f"• Fee: {100 * pool.fee / pool.FEE_DENOMINATOR:.4f}%")

        return pool
```

## Implementation Steps

### Phase 1: Create detection types (TDD)

1. **Red:** Write tests for `CoinDiscoveryResult`, `LendingDetectionResult`, etc. — test immutability, field defaults.
2. **Green:** Create `src/degenbot/curve/detection/types.py` with all frozen dataclasses.
3. Create `src/degenbot/curve/detection/__init__.py`.

### Phase 2: Extract coin discovery (TDD)

1. **Red:** Write test for `discover_coins()` with a fake `w3` that returns predefined coin addresses and reverts at index 3.
   ```python
   def test_discover_coins_uint256():
       w3 = FakeWeb3(coin_responses=[
           ("0xA0b8...", True),   # coins(0) → valid address
           ("0x6B17...", True),   # coins(1) → valid address
           ("0x0000...", False),  # coins(2) → zero address (stop)
       ])
       result = discover_coins(w3, "0xPool...", block_identifier=18_000_000)
       assert len(result.token_addresses) == 2
       assert result.coin_prototype == "coins(uint256)"
   ```
2. **Green:** Extract the coin iteration loop from `CurvePoolBuilder.build()` into `discover_coins()`.
3. Run existing pool builder tests — should still pass.

### Phase 3: Extract lending detector (TDD)

1. **Red:** Write test for `detect_lending_tokens()`.
   ```python
   def test_detect_ctoken():
       w3 = FakeWeb3(
           is_ctoken={0: True, 1: False},
           underlying={0: "0x6B17..."},  # DAI underlying for cDAI
           underlying_decimals={0: 18},
       )
       result = detect_lending_tokens(w3, "0xPool...", token_addrs, tokens, block_identifier=18_000_000)
       assert result.use_lending == (True, False)
       assert result.precision_multipliers is not None
   ```
2. **Green:** Extract the cToken/yToken detection from `CurvePoolBuilder.build()` into `detect_lending_tokens()`.
3. Run existing pool builder tests.

### Phase 4: Extract metapool detector (TDD)

1. **Red:** Write test for `detect_metapool()`.
   ```python
   def test_detect_metapool_from_registry():
       w3 = FakeWeb3(is_meta=True, base_pool="0xbEbc4...", underlying_coins=[...])
       result = detect_metapool(w3, "0xPool...", token_addrs, block_identifier=18_000_000)
       assert result.is_meta
       assert result.base_pool_address == "0xbEbc4..."
   ```
2. **Green:** Extract the metapool detection from `CurvePoolBuilder.build()` into `detect_metapool()`.
3. Run existing pool builder tests.

### Phase 5: Extract crypto detector (TDD)

1. **Red:** Write tests for `detect_crypto_params()` and `detect_a_ramping()`.
2. **Green:** Extract from `CurvePoolBuilder.build()`.
3. Run existing pool builder tests.

### Phase 6: Extract LP token finder (TDD)

1. **Red:** Write test for `_find_lp_token()`.
2. **Green:** Extract from `CurvePoolBuilder.build()`.
3. Run existing pool builder tests.

### Phase 7: Rewrite build() as orchestrator

1. Replace the 400-line `build()` body with the orchestrator shown above.
2. Each step calls one of the extracted functions.
3. The `_resolve_metapool()` helper handles the recursive base pool build.
4. Run ALL curve tests.

### Phase 8: Verify and clean up

1. `just test-all` — all tests pass.
2. `just lint` — no new warnings.
3. Verify `build()` is under 100 lines.
4. Verify each detection module is independently testable.

## What Stays the Same

- `CurveStableswapPool` constructor — same parameters, same types.
- `CurveFetcherFactory` — unchanged.
- `Bot.build_curve_pool()` — delegates to builder, same API.
- Integration tests — same pool construction results.

## What Changes

| Before | After |
|---|---|
| `CurvePoolBuilder.build()` is ~400 lines | `build()` is ~80 lines orchestrator |
| One method contains all detection logic | 5 focused detection modules |
| Can't test coin discovery in isolation | `discover_coins()` testable with fake `w3` |
| Can't test lending detection in isolation | `detect_lending_tokens()` testable independently |
| Fix cToken false-positive: edit 400-line method | Fix: edit `lending_detector.py` only |
| New pool variant: understand 400 lines to find where to add detection | New variant: add a case in the relevant detector |

## Metrics

| Metric | Before | After |
|---|---|---|
| `build()` lines | ~400 | ~80 |
| Detection modules | 0 (all inline) | 5 (coin, lending, metapool, crypto, A ramping) |
| Max function depth in `build()` | 8 levels of try/except | 1-2 levels |
| Time to understand coin discovery | Read 400 lines | Read `coin_discovery.py` (~60 lines) |
| Testability of lending detection | Only via integration test | Unit test with fake `w3` |

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Behavioral change in detection heuristics | Each extraction is a pure move — check existing tests still pass after each phase. |
| Over-abstraction: detectors need too many shared parameters | The `w3` + `pool_address` + `block_identifier` pattern is narrow. Detection results are frozen dataclasses — no shared mutable state. |
| Metapool detector needs recursive builder call | The detector only *detects* — it returns `MetapoolDetectionResult`. The builder handles the recursive `self.build()` call in `_resolve_metapool()`. Clean separation. |
| Detection order might matter (e.g., crypto detection uses fee_gamma which also appears in the main params fetch) | Each detector fetches what it needs. The orchestrator calls them in the right order. No cross-detector dependencies unless documented. |

## Definition of Done

- [x] `src/degenbot/curve/detection/types.py` created with all frozen dataclasses
- [x] `discover_coins()` extracted and tested
- [x] `detect_lending_tokens()` extracted and tested
- [x] `detect_metapool()` extracted and tested
- [x] `detect_crypto_params()` extracted and tested
- [x] `detect_a_ramping()` extracted and tested
- [x] `find_lp_token()` extracted and tested
- [x] `CurvePoolBuilder.build()` rewritten as orchestrator (~140 lines, constructor call accounts for ~40)
- [x] All existing Curve tests pass unchanged
- [x] New unit tests for each detector pass
- [ ] `just test-all` passes (Rust tests not run yet)
