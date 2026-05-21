# Plan 065: Collapse AsyncBot Inline I/O Methods into AsyncErc20Builder

## Overview

Route AsyncBot's four inline I/O methods (`get_token_balance`, `get_token_approval`, `get_token_total_supply`, `get_ether_balance`) through `AsyncErc20Builder`, matching Bot's delegation to `Erc20Builder`. Remove ~137 lines of duplicated ABI-encode + cache-check + RPC-call logic from `async_bot.py`.

## Problem

### Deletion test

If you delete the four inline I/O methods from AsyncBot, no unique behavior is lost — `AsyncErc20Builder` can provide the same functionality by accepting `AsyncPoolIO`, matching exactly what `Erc20Builder` does with `PoolIO`. The inline methods are a shadow of the builder seam that already exists for Bot.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|-------------|
| `get_token_balance` duplicated | `async_bot.py:326–362` vs `erc20_builder.py:180–212` | ABI encoding, cache checking, and provider call all re-implemented inline |
| `get_token_approval` duplicated | `async_bot.py:363–401` vs `erc20_builder.py:213–247` | Same pattern — `allowance()` ABI, cache, provider |
| `get_token_total_supply` duplicated | `async_bot.py:402–435` vs `erc20_builder.py:248–277` | Same pattern — `totalSupply()` ABI, cache, provider |
| `get_ether_balance` duplicated | `async_bot.py:436–450` vs `erc20_builder.py:279–297` | Same pattern — `get_balance()`, block resolution |
| `_resolve_block_number` duplicated | `async_bot.py:451–462` vs `erc20_builder.py:298–308` | Structurally similar but operates on different types: `AsyncProviderAdapter` vs `PoolIO` |
| API shape diverges | AsyncBot takes `token_address: str`; Bot takes `token: Erc20Token` | Callers can't write code that works with both; AsyncBot does its own token lookup + cache |
| `get_ether_balance` signature diverges | Bot: `(chain_id, address, block_identifier)` positional; AsyncBot: `(address, *, chain_id=None, block_identifier=None)` keyword-only | Different calling convention — can't swap Bot/AsyncBot in shared code |

## Solution

### Step 1: Add I/O methods to AsyncErc20Builder

Add `get_token_balance`, `get_token_approval`, `get_token_total_supply`, `get_ether_balance` to `AsyncErc20Builder`, mirroring `Erc20Builder`'s signatures but with `async` and `AsyncPoolIO`.

```python
# Before: AsyncErc20Builder has only build()
class AsyncErc20Builder:
    async def build(self, address, *, chain_id, silent, io) -> Erc20Token: ...

# After: AsyncErc20Builder gains the four I/O methods
class AsyncErc20Builder:
    async def build(self, address, *, chain_id, silent, io) -> Erc20Token: ...
    async def get_token_balance(self, token, address, *, block_identifier, io) -> int: ...
    async def get_token_approval(self, token, owner, spender, *, block_identifier, io) -> int: ...
    async def get_token_total_supply(self, token, *, block_identifier, io) -> int: ...
    async def get_ether_balance(self, chain_id, address, *, block_identifier, io) -> int: ...
```

Each async method mirrors `Erc20Builder`'s logic: resolve block number → check token cache → call via `AsyncPoolIO` → decode → update cache → return.

**Use `encode_function_calldata` instead of `Web3.keccak` + `eth_abi.abi.encode`.** The sync `Erc20Builder` uses `Web3.keccak(text="balanceOf(address)")[:4] + eth_abi.abi.encode(...)` inline, but `encode_function_calldata` is the project's standard helper for the same operation. The new async methods should use `encode_function_calldata` for consistency.

### Step 2: Route AsyncBot methods through AsyncErc20Builder

```python
# Before: async_bot.py (inline ~35 lines per method)
async def get_token_balance(self, token_address, holder_address, ...):
    token_address = get_checksum_address(token_address)
    holder_address = get_checksum_address(holder_address)
    provider = self.connections.get_provider(chain_id)
    token = self.tokens.get(...) or await self.build_erc20token(...)
    block_number = await self._resolve_block_number(provider, block_identifier)
    # ... ABI encode, call, decode, cache ...

# After: async_bot.py (delegating, ~8 lines — retains string→token resolution)
async def get_token_balance(
    self,
    token_or_address: str | Erc20Token,
    holder_address: str,
    *,
    chain_id: ChainId | None = None,
    block_identifier: BlockIdentifier | None = None,
) -> int:
    if isinstance(token_or_address, str):
        chain_id = chain_id or self.connections.default_chain_id
        token = self.tokens.get(token_address=token_or_address, chain_id=chain_id)
        if token is None:
            token = await self.build_erc20token(token_or_address, chain_id=chain_id)
    else:
        token = token_or_address
        chain_id = chain_id or token.chain_id or self.connections.default_chain_id
    io = AsyncPoolIO(self.connections.get_provider(chain_id))
    return await self._erc20_builder.get_token_balance(
        token, holder_address, block_identifier=block_identifier, io=io
    )
```

AsyncBot retains the **string→token resolution logic** (registry lookup + auto-build) in its wrapper methods — this is the caller's convenience layer. The builder methods are pure I/O consumers: given an `Erc20Token` and `AsyncPoolIO`, they do the encode/call/decode/cache cycle.

### Step 3: Unify `get_ether_balance` signature with Bot's

The `get_ether_balance` signature divergence between Bot and AsyncBot must be resolved. Bot uses `(chain_id, address, block_identifier)` with `chain_id` positional. AsyncBot uses `(address, *, chain_id=None, block_identifier=None)` with `chain_id` keyword-only.

The builder methods should match `Erc20Builder.get_ether_balance`'s signature: `(chain_id, address, block_identifier, *, io)`. For the AsyncBot public method, adopt the keyword style (it's more Pythonic) but the builder method mirrors the sync builder:

```python
# AsyncErc20Builder (matches Erc20Builder)
async def get_ether_balance(
    self, chain_id: ChainId, address: str,
    block_identifier: BlockIdentifier | None = None, *, io: AsyncPoolIO,
) -> int: ...

# AsyncBot (keyword-style public API)
async def get_ether_balance(
    self, address: str, *,
    chain_id: ChainId | None = None,
    block_identifier: BlockIdentifier | None = None,
) -> int: ...
```

### Design decisions

- **Accept `token: Erc20Token` on builder methods, support `str | Erc20Token` on AsyncBot wrapper**: Matches Bot's builder signature. The Erc20Token object carries the cache. AsyncBot's public methods accept either type for backward compatibility — if a string is passed, AsyncBot does the lookup + auto-build before delegating.
- **`io: AsyncPoolIO` parameter on builder methods**: Matches `Erc20Builder`'s pattern of accepting `io: PoolIO`. Bot creates the adapter, builder consumes it.
- **`_resolve_block_number` changes type from `AsyncProviderAdapter` → `AsyncPoolIO`**: The current `AsyncBot._resolve_block_number` takes `AsyncProviderAdapter` and calls `await provider.get_block_number()`. The new builder-level version takes `AsyncPoolIO` and calls `await io.get_block_number()`. This is consistent with the builder pattern and eliminates the direct provider dependency.
- **`chain_id` inference moves from builder to AsyncBot wrapper**: In the current inline methods, AsyncBot resolves `chain_id` from `self.connections.default_chain_id`. After the refactor, the builder methods take `chain_id` from `token.chain_id` (for token-backed methods) or as an explicit parameter (for `get_ether_balance`). AsyncBot's wrapper methods still resolve `chain_id` before passing it to the builder.
- **Signature change is backward-compatible via `str | Erc20Token` overload**: The `token_address: str` → `token_or_address: str | Erc20Token` change preserves existing string-based callers while enabling `Erc20Token`-based callers. No deprecation cycle needed.

## Files Involved

**Primary:**
- `src/degenbot/builders/async_erc20_builder.py` — add 4 I/O methods + `_resolve_block_number`
- `src/degenbot/async_bot.py` — replace 4 inline methods with delegation; delete `_resolve_block_number`

**Secondary:**
- `src/degenbot/builders/erc20_builder.py` — no structural change, but consider extracting `_resolve_block_number` to a shared location
- `src/degenbot/builders/protocol.py` — update `AsyncPoolBuilder` protocol if it declares I/O methods (currently it doesn't declare these; the builder methods are `Erc20Builder`/`AsyncErc20Builder`-specific, not part of the pool builder protocol)
- `tests/test_async_bot.py` — update I/O method tests to exercise both `str` and `Erc20Token` inputs
- `src/degenbot/builders/context.md` or relevant `CONTEXT.md` — note that AsyncErc20Builder now owns ERC-20 I/O methods

**No change needed:**
- `src/degenbot/bot.py` — already delegates to `Erc20Builder`
- `src/degenbot/erc20/erc20.py` — cache methods already exist (`get_cached_balance`, etc.)

## Implementation Order

### Slice 1: Add `get_token_balance` and `get_ether_balance` to AsyncErc20Builder

1. Add `async get_token_balance(self, token, address, *, block_identifier, io) -> int` to `AsyncErc20Builder`. Use `encode_function_calldata` instead of `Web3.keccak` + `eth_abi.abi.encode`.
2. Add `async get_ether_balance(self, chain_id, address, *, block_identifier, io) -> int` to `AsyncErc20Builder`
3. Add `@staticmethod async _resolve_block_number(io: AsyncPoolIO, block_identifier) -> int` to `AsyncErc20Builder`. Note: takes `AsyncPoolIO`, not `AsyncProviderAdapter` (unlike the current `AsyncBot._resolve_block_number`).
4. Write tests: `tests/builders/test_async_erc20_builder_io.py` with `FakeAsyncPoolIO` that satisfies the `AsyncPoolIOProtocol` (positional `call(to, data, block)` not keyword)
5. Run: `just test-python` — expect all green

### Slice 2: Add `get_token_approval` and `get_token_total_supply` to AsyncErc20Builder

1. Add `async get_token_approval(self, token, owner, spender, *, block_identifier, io) -> int`
2. Add `async get_token_total_supply(self, token, *, block_identifier, io) -> int`
3. Extend test file with approval and total_supply tests
4. Run: `just test-python` — expect all green

### Slice 3: Route AsyncBot methods through AsyncErc20Builder

1. Replace `AsyncBot.get_token_balance` body: resolve `str | Erc20Token` → token, create `AsyncPoolIO`, delegate to builder
2. Replace `AsyncBot.get_token_approval` body similarly
3. Replace `AsyncBot.get_token_total_supply` body similarly
4. Replace `AsyncBot.get_ether_balance` body: create `AsyncPoolIO`, delegate to builder. Adopt keyword-style signature for the public method.
5. Delete `AsyncBot._resolve_block_number`
6. Update `tests/test_async_bot.py` — tests that pass `token_address` strings should still work (backward compat). Add new tests passing `Erc20Token` objects directly.
7. Run: `just test-python` — expect all green

### Slice 4: Validate and clean up

1. Run `just lint` + `just test-all`
2. Verify `async_bot.py` line count dropped by ~137 lines (the four inline methods + `_resolve_block_number`)
3. Update `AsyncBot` docstrings to note delegation to `AsyncErc20Builder`
4. Update relevant `CONTEXT.md` files (builders, bot) to note ERC-20 I/O method ownership

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Existing AsyncBot tests must pass after each slice.

### New unit tests

```python
# tests/builders/test_async_erc20_builder_io.py

import eth_abi.abi
import pytest
from degenbot.erc20.erc20 import Erc20Token
from degenbot.builders.async_erc20_builder import AsyncErc20Builder


class FakeAsyncPoolIO:
    """Minimal AsyncPoolIO stub satisfying AsyncPoolIOProtocol.

    Note: call() takes positional args (to, data, block), not keyword —
    matching the AsyncPoolIO / AsyncPoolIOProtocol signatures.
    """
    def __init__(self, responses=None, block_number=100, balance=10**18):
        self._responses = responses or {}
        self._block_number = block_number
        self._balance = balance

    async def call(self, to, data, block=None):
        return self._responses.get(data[:4], b'\x00' * 32)

    async def get_block_number(self):
        return self._block_number

    async def get_balance(self, address, block=None):
        return self._balance


def _make_weth() -> Erc20Token:
    return Erc20Token(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )


@pytest.mark.asyncio
async def test_async_get_token_balance():
    """AsyncErc20Builder.get_token_balance mirrors Erc20Builder."""
    ...


@pytest.mark.asyncio
async def test_async_get_token_approval():
    """AsyncErc20Builder.get_token_approval returns cached or fetched value."""
    ...


@pytest.mark.asyncio
async def test_async_get_ether_balance():
    """AsyncErc20Builder.get_ether_balance delegates to io.get_balance."""
    ...
```

### Integration tests

`tests/test_async_bot.py` already tests AsyncBot's I/O methods with `token_address: str`. After routing through the builder, these tests continue to pass (same external behavior). Add new tests that pass `Erc20Token` objects directly to verify the alternate code path.

Tests in `tests/erc20/test_erc20_io_free.py` exercise Bot's delegation to `Erc20Builder` — these are unaffected.

### Backward-compatibility tests

After Slice 3, explicitly verify that:
- `await bot.get_token_balance(token_address="0x...", holder_address="0x...", chain_id=1)` still works (string path)
- `await bot.get_token_balance(token=weth, holder_address="0x...")` works (Erc20Token path)
- `await bot.get_ether_balance("0x...", chain_id=1)` still works (keyword `chain_id`)

## Benefits

- **Locality**: ERC-20 I/O logic concentrates in builders — fix ABI encoding once in `Erc20Builder` / `AsyncErc20Builder`
- **Leverage**: Bot and AsyncBot share the same interface per builder; one seam, two adapters (sync + async)
- **Depth**: AsyncBot's I/O methods are shallow (call → encode → decode → cache). The builder absorbs them, exposing a single `get_token_balance()` method instead.
- **Deletion test**: The inline methods are a pass-through over the builder. Deleting them re-concentrates complexity in the builder (where it belongs).

## Risks

- **`_resolve_block_number` type change**: The current `AsyncBot._resolve_block_number` takes `AsyncProviderAdapter`; the new builder version takes `AsyncPoolIO`. Any code that called the old method directly (unlikely — it's private) would need updating.
- **`get_ether_balance` signature change**: The AsyncBot public method's signature changes from `(address, *, chain_id=None, ...)` to `(address, *, chain_id=None, ...)` — no change from the caller's perspective, but the builder method has `chain_id` as a positional parameter (matching `Erc20Builder`). This is an internal detail only.
- **One extra async hop**: Routing through the builder adds one method call per I/O operation. Negligible compared to the RPC round-trip (~50ms).
- **Auto-build behavior preserved**: The current inline methods build tokens if not found in the registry. The `str | Erc20Token` overload on AsyncBot's wrapper methods preserves this behavior. Risk: if a caller passes an `Erc20Token` that isn't in the registry, the cache won't be populated for other code that looks it up by address. This matches Bot's existing behavior — callers are responsible for registration.

## Relationship to Other Plans

- **Plan 067** (BuildPoolRequest): Complementary — removing inline I/O from AsyncBot reduces the class size before the kwargs tunnel refactor.
- **Plan 048** (Async Builder Shared): This is a continuation — Plan 048 introduced `AsyncPoolIO` and async builders; this plan completes the migration by routing the remaining I/O methods through the builder seam.
- **Plan 066** (Unify type resolution): Orthogonal — different module, but both reduce duplication between sync/async paths.
- **Plan 070** (Balancer Builder): Orthogonal — different pool family.

## Status

[x] Slice 1: Add `get_token_balance` and `get_ether_balance` to AsyncErc20Builder
[x] Slice 2: Add `get_token_approval` and `get_token_total_supply` to AsyncErc20Builder
[x] Slice 3: Route AsyncBot methods through AsyncErc20Builder
[x] Slice 4: Validate and clean up
