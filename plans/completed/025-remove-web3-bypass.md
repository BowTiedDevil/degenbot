# Plan 025: Remove Web3 Bypass — Route All RPC Through ProviderAdapter

> **Note**: `ConnectionManager.get_web3()` and `Bot.get_web3()` were deleted by Plan 059. Use `get_provider()` + `provider.as_web3()` instead.

## Problem

Multiple call sites reach past `ProviderAdapter` to use raw `Web3`/`w3` objects directly (`w3.eth.call()`, `w3.eth.get_block()`, `w3.eth.contract()`, `w3.batch_requests()`). This means:

1. **Alloy providers break** — any code path that gets a non-Web3 `ProviderAdapter` (e.g. from Alloy) will fail at runtime when callers invoke `get_web3()` or `.underlying`.
2. **Offline providers break** — same issue for `OfflineProvider`.
3. **Testing requires real Web3** — detection modules can't be tested with a fake provider; they need a real or mocked `w3`.
4. **The escape hatch persists** — `ConnectionManager.get_web3()` and `ProviderAdapter.underlying` exist solely to serve these bypasses, undermining the adapter pattern.

## Goal

All application RPC calls route through `ProviderAdapter` (sync) or `AsyncProviderAdapter` (async). No production code calls `w3.eth.*` directly. `get_web3()` and `.underlying` are deprecated and eventually removed.

## Scope

### In Scope
- Curve detection sub-modules (6 files, ~23 call sites)
- Curve fetcher factory (~15 call sites)
- Curve pool builder (~8 call sites)
- Chainlink contract calls (2 call sites)
- Aerodrome batch/pool identity calls (3 call sites)
- Async log-fetching pipeline (3 files)
- Arbitrage cycle block-number call (1 call site)
- `ConnectionManager.get_web3()` / `ProviderAdapter.underlying` deprecation

### Out of Scope
- `ProviderAdapter` internals (e.g. `_Web3Adapter._w3.eth.*`) — these ARE the adapter layer
- `AnvilFork.w3` — fork-management utility, not application RPC
- CLI endpoint construction (`cli/utils.py`) — boundary code that creates `Web3` instances
- `Web3.keccak()` — static hashing, not an RPC call
- Type imports from `web3.types` — no runtime effect

---

## Steps

### Step 1: Add `call_raw` and `batch_call` to `ProviderAdapter`

Some bypass sites need capabilities that the current `ProviderAdapter` interface doesn't expose:
- **Raw `eth_call` with arbitrary `Transaction` dict** — detection modules pass `{"to": ..., "data": ...}` dicts to `w3.eth.call()`, but `ProviderAdapter.call()` uses keyword args `call(to=, data=, block=)`. Add a lower-level `call_raw(tx: dict, block: int | None = None)` for these callers.
- **Batch `eth_call`** — Aerodrome's `get_pool_identity_values()` uses `w3.batch_requests()`. Add `batch_call(calls: list[dict], block: int | None = None) -> list[HexBytes]` that sends `eth_call` for each and returns results in order. (A future optimization could use `multicall` or Alloy batch under the hood.)

```python
# In ProviderAdapter:
def call_raw(self, tx: dict[str, Any], block: int | None = None) -> HexBytes:
    """Execute an eth_call with a raw transaction dict."""
    return self._backend.call_raw(tx, block)


def batch_call(self, calls: list[dict[str, Any]], block: int | None = None) -> list[HexBytes]:
    """Execute multiple eth_calls. Returns results in input order."""
    return [self._backend.call_raw(tx, block) for tx in calls]
```

Add `call_raw` to `_SyncProviderBackend` protocol and implement in `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`.

### Step 2: Add `get_block_timestamp` convenience method

Several sites call `get_block()` only to extract `["timestamp"]`. Add:

```python
def get_block_timestamp(self, block: int | None = None) -> int:
    """Get the timestamp for a block."""
    block = self.get_block(block or "latest")
    if block is None:
        msg = f"Block {block} not found"
        raise DegenbotValueError(message=msg)
    return block["timestamp"]
```

### Step 3: Migrate Curve detection sub-modules

Change each detector's `w3: Any` parameter to `provider: ProviderAdapter` and replace all `w3.eth.call({...}, block_identifier=N)` with `provider.call_raw({...}, block=N)`.

Files:
- `src/degenbot/curve/detection/coin_discovery.py` — 4 `w3.eth.call` → `provider.call_raw`
- `src/degenbot/curve/detection/a_ramping.py` — 4 → `provider.call_raw`
- `src/degenbot/curve/detection/crypto_detector.py` — 5 → `provider.call_raw`
- `src/degenbot/curve/detection/lending_detector.py` — 5 → `provider.call_raw`
- `src/degenbot/curve/detection/metapool_detector.py` — 4 → `provider.call_raw`
- `src/degenbot/curve/detection/lp_token.py` — 1 → `provider.call_raw`

All detectors are called from `CurvePoolBuilder.build()` (Step 5), so the builder will pass `provider` instead of `w3`.

**Testing**: Each detector is independently testable — replace `w3: Any` with a `ProviderAdapter` wrapping a fake/offline provider and verify.

### Step 4: Migrate CurveFetcherFactory

Change all fetcher closures from `w3 = self._connections.get_web3(chain_id)` + `w3.eth.call()` to `provider = self._connections.get_provider(chain_id)` + `provider.call()` or `provider.call_raw()`.

The `CurveFetcherFactory` already uses `get_provider()` + `provider.call()` for `admin_balances_fetcher`, `block_number_fetcher`, `total_supply_fetcher`, and `token_balance_fetcher` — so 4 of 13 fetchers are already correct. Migrate the remaining 9:

| Fetcher | Current | Target |
|---------|---------|--------|
| `virtual_price_fetcher` | `get_web3` + `w3.eth.call` | `get_provider` + `provider.call_raw` |
| `base_virtual_price_fetcher` | `get_web3` + `w3.eth.call` | `get_provider` + `provider.call_raw` |
| `base_cache_updated_fetcher` | `get_web3` + `w3.eth.call` | `get_provider` + `provider.call_raw` |
| `timestamp_fetcher` | `get_web3` + `w3.eth.get_block` | `get_provider` + `provider.get_block_timestamp` |
| `redemption_price_fetcher` | `get_web3` + `w3.eth.call` × 2 | `get_provider` + `provider.call_raw` × 2 |
| `provider_call` | `get_web3` + `w3.eth.call` | `get_provider` + `provider.call_raw` |
| `D_fetcher` | `get_web3` + `w3.eth.call` | `get_provider` + `provider.call_raw` |
| `gamma_fetcher` | `get_web3` + `w3.eth.call` | `get_provider` + `provider.call_raw` |
| `price_scale_fetcher` | `get_web3` + `w3.eth.call` | `get_provider` + `provider.call_raw` |

After this step, `CurveFetcherFactory` no longer imports or references `Web3` except for `Web3.keccak()` (static hashing — keep as-is).

### Step 5: Migrate CurvePoolBuilder

In `CurvePoolBuilder.build()`:
- Replace `w3 = self._connections.get_web3(chain_id)` with `provider = self._connections.get_provider(chain_id)`
- Pass `provider` to all detection functions instead of `w3`
- Replace `_fetch_pool_params(w3, ...)` with `_fetch_pool_params(provider, ...)` using `provider.call_raw()`

In `CurvePoolBuilder.update()`:
- Replace `w3 = self._connections.get_web3(pool.chain_id)` + `w3.eth.call()` with `provider = self._connections.get_provider(pool.chain_id)` + `provider.call_raw()`

The standalone `_fetch_pool_params()` function changes from `w3: Any` to `provider: ProviderAdapter` with `provider.call_raw()`.

### Step 6: Migrate Aerodrome `get_pool_identity_values()`

`AerodromeV2Pool.get_pool_identity_values()` uses two Web3-specific features:
1. `w3.batch_requests()` — batch `eth_call`
2. `w3.eth.call` passed as a batch callable

Replace with `provider.batch_call()` (from Step 1):

```python
def get_pool_identity_values(
    self,
    provider: ProviderAdapter,
    state_block: BlockNumber,
) -> tuple[...]:
    immutable_calls = [
        {"to": self.address, "data": encode_function_calldata("factory()", None)},
        {"to": self.address, "data": encode_function_calldata("token0()", None)},
        {"to": self.address, "data": encode_function_calldata("token1()", None)},
        {"to": self.address, "data": encode_function_calldata("stable()", None)},
    ]
    results = provider.batch_call(immutable_calls)
    factory, token0, token1, stable = results

    reserves = provider.call_raw(
        {"to": self.address, "data": encode_function_calldata("getReserves()", None)},
        block=state_block,
    )
    fee = provider.call_raw(
        {"to": get_checksum_address(factory_decoded), "data": encode_function_calldata(...)},
    )
    ...
```

This removes `Web3` import from `aerodrome/pools.py` (except `Web3.keccak` if used elsewhere).

### Step 7: Migrate Chainlink contract calls

`ChainlinkPriceContract` uses `w3.eth.contract()` to create a contract instance, then calls `.functions.*.call()`. This is a Web3-specific convenience with no ProviderAdapter equivalent.

**Approach**: Replace the contract-object pattern with raw `eth_call` using `provider.call_raw()`, matching the pattern used everywhere else in the codebase:

```python
# Before:
w3 = self._bot.connections.get_provider(chain_id).underlying
contract = w3.eth.contract(address=self.address, abi=CHAINLINK_PRICE_FEED_ABI)
self._decimals = contract.functions.decimals().call()
price_data = contract.functions.latestRoundData().call()

# After:
provider = self._bot.connections.get_provider(chain_id)
decimals_data = provider.call_raw({"to": self.address, "data": Web3.keccak(text="decimals()")[:4]})
(self._decimals,) = eth_abi.abi.decode(["uint8"], decimals_data)

round_data = provider.call_raw({
    "to": self.address,
    "data": Web3.keccak(text="latestRoundData()")[:4],
})
roundId, answer, startedAt, updatedAt, answeredInRound = eth_abi.abi.decode(
    ["uint80", "int256", "uint256", "uint256", "uint80"], round_data
)
```

This eliminates the `w3.eth.contract()` dependency. Only `Web3.keccak()` remains (static hashing).

### Step 8: Migrate async log-fetching pipeline

`fetch_logs_retrying_async()` and `get_number_for_block_identifier_async()` accept `w3: AsyncWeb3[AsyncBaseProvider]`. Change them to accept `provider: AsyncProviderAdapter` and route through `provider.get_logs()`, `provider.get_block_number()`, `provider.get_block()`.

Update callers:
- `UniswapV3Snapshot.fetch_new_events_async(w3=...)` → `provider=...`
- `UniswapV4Snapshot.fetch_new_events_async(w3=...)` → `provider=...`

### Step 9: Migrate arbitrage cycle block-number call

```python
# Before (uniswap_curve_cycle.py):
block_number = self._bot.connections.get_web3(chain_id=...).eth.get_block_number()

# After:
block_number = self._bot.connections.get_provider(chain_id=...).get_block_number()
```

### Step 10: Deprecate `get_web3()` and `ProviderAdapter.underlying`

Once all callers are migrated:

1. Add deprecation warnings to:
   - `ConnectionManager.get_web3()`
   - `AsyncConnectionManager.get_web3()`
   - `Bot.get_web3()`
   - `AsyncBot.get_web3()`
   - `ProviderAdapter.underlying`
   - `AsyncProviderAdapter.underlying`

2. Remove `get_web3()` from `Bot` and `AsyncBot` public API (keep on `ConnectionManager` temporarily for any external callers).

3. Mark for removal in a future release (Plan 026 or later).

---

## Dependency Graph

```
Step 1 (add call_raw/batch_call) ← all others depend on this
  ├── Step 2 (get_block_timestamp) ← Step 4 needs this
  ├── Step 3 (Curve detection)    ← Step 5 needs this
  │     └── Step 5 (Curve builder) ← orchestrates Step 3
  ├── Step 4 (CurveFetcherFactory) ← independent of Step 3
  │     └── Step 5 also depends on Step 4 (fetchers used in build)
  ├── Step 6 (Aerodrome)          ← independent
  ├── Step 7 (Chainlink)          ← independent
  ├── Step 8 (async pipeline)     ← independent
  └── Step 9 (arb cycle)          ← independent
Step 10 (deprecation)             ← after Steps 3-9 complete
```

## Risk Analysis

| Risk | Mitigation |
|------|-----------|
| `call_raw` dict format diverges between Web3 and Alloy | Both adapters translate the dict to their internal format. The dict schema is `{"to": ..., "data": ...}` — simple, stable, same as web3.py's `Transaction` type. |
| Batch call performance regression (sequential vs parallel) | Web3 `batch_requests()` already fires requests sequentially in a single HTTP batch for HTTP providers. Our `batch_call()` can do the same. A future optimization adds `multicall3` batching. |
| Chainlink ABI decoding changes with raw calls | The contract ABI is already known; we just swap `contract.functions.X().call()` → `provider.call_raw()` + `eth_abi.abi.decode()`. Same result, more explicit. |
| Breaking change for external callers using `get_web3()` | Step 10 adds deprecation warnings first. Removal is a separate plan. |
| `curve_stableswap_liquidity_pool.py` uses `provider_call` fetcher (injected from factory) | Already handled — Step 4 migrates `provider_call` from `w3.eth.call` to `provider.call_raw`. The pool still calls its injected fetcher; the fetcher's implementation changes but its signature doesn't. |

## Testing Strategy

- Each detection module is independently testable with `ProviderAdapter.from_offline(FakeOfflineProvider(...))` — no mocking needed.
- `CurveFetcherFactory` fetchers can be tested with a fake provider that returns canned responses.
- Aerodrome's `get_pool_identity_values()` can be tested with `OfflineProvider` returning pre-set call results.
- Existing integration tests (ForkedAvalancheTest, ForkedEthereumTest, etc.) verify end-to-end correctness.
- Run `just test-all` after each step. Each step is independently committable and revertible.

## Call Site Count Summary

| Step | File(s) | Sites Changed |
|------|---------|---------------|
| 1 | `provider/interface.py` | +2 new methods |
| 2 | `provider/interface.py` | +1 new method |
| 3 | 6 detection files | 23 |
| 4 | `curve/fetcher_factory.py` | 9 |
| 5 | `builders/curve_pool_builder.py` | 8 |
| 6 | `aerodrome/pools.py` | 3 |
| 7 | `chainlink.py` | 2 |
| 8 | `functions.py`, `v3_snapshot.py`, `v4_snapshot.py` | 3 |
| 9 | `arbitrage/uniswap_curve_cycle.py` | 1 |
| 10 | `connection_manager.py`, `bot.py`, `async_bot.py` | deprecation warnings |
| **Total** | | **49 call sites** |
