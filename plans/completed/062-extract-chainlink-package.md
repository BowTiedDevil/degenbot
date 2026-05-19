# Plan 062: Extract Chainlink into a Package with CONTEXT.md

## Overview

Move `chainlink.py` from the package root into a `chainlink/` package with a `CONTEXT.md` defining oracle/price-feed vocabulary, and delete the unused `CHAINLINK_PRICE_FEED_ABI` constant. This organizational refactoring gives oracle concerns a proper home and establishes the vocabulary for future oracle integrations.

## Problem

### Deletion test

If you moved `chainlink.py` into its own package, nothing would break — the import path `degenbot.chainlink` can be preserved via `__init__.py` re-export. The complexity moves rather than vanishes, but the signal is that the current placement is wrong: a price oracle module at the package root suggests it's as foundational as `Bot` or `Config`, which it isn't.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Price oracle at package root | `src/degenbot/chainlink.py` (81 lines) | Sits alongside `bot.py`, `config.py`, `anvil_fork.py` — infrastructure modules. But `ChainlinkPriceContract` is a domain object (price oracle), not infrastructure. A reader scanning the package root can't tell the conceptual weight of each module. |
| Dead-code ABI constant | `chainlink.py` lines 15–28 | `CHAINLINK_PRICE_FEED_ABI` is defined but never imported anywhere — not by `ChainlinkPriceContract`, not by any test, not by any other module. The class uses raw `eth_abi.abi.decode` with manually-computed selectors (`Web3.keccak(text="decimals()")[:4]`). The ABI is dead weight that should be deleted, not moved. |
| No CONTEXT.md for oracle vocabulary | Missing | If someone adds a Pyth oracle or a Uniswap TWAP oracle, there's no vocabulary file defining terms like "Price Feed," "Oracle," "Aggregator," or "Round Data." The root CONTEXT-MAP.md has no oracle module listed. |
| Only 4 import sites (2 production, 2 test) | `src/degenbot/erc20/erc20.py`, `src/degenbot/__init__.py`, `tests/test_chainlink_price_feed.py`, `tests/erc20/test_erc20_token.py` | The module is used only by `Erc20Token` and re-exported from the package root. It doesn't warrant package-root placement. |
| `Bot` dependency for RPC access | `ChainlinkPriceContract.__init__(bot=...)` | The class takes `bot: Bot | None` and calls `self._bot.connections.get_provider(chain_id)` directly, bypassing the I/O-free architecture. This is inconsistent with the rest of the codebase where pools receive data through builders and `external_update()`. This plan does not fix this — it documents it as a known smell in the CONTEXT.md. |

## Solution

### Step 1: Create `src/degenbot/chainlink/` package

```
src/degenbot/chainlink/
├── __init__.py       # Re-exports ChainlinkPriceContract for backward compat
├── price_feed.py     # ChainlinkPriceContract class (logic only)
└── CONTEXT.md        # Oracle vocabulary
```

No `abi.py` — `CHAINLINK_PRICE_FEED_ABI` is dead code and will be deleted rather than moved.

### Step 2: Move `ChainlinkPriceContract` to `price_feed.py`

Move the class (without `CHAINLINK_PRICE_FEED_ABI`). Also remove the now-unused `import pydantic_core` from the module.

### Step 3: Create `__init__.py` with backward-compat re-exports

```python
# chainlink/__init__.py
from degenbot.chainlink.price_feed import ChainlinkPriceContract

__all__ = ["ChainlinkPriceContract"]
```

This preserves the import path `from degenbot.chainlink import ChainlinkPriceContract`.

### Step 4: Create `CONTEXT.md` with oracle vocabulary

```markdown
# Context — Chainlink Price Oracles

## Terms

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Price Feed** | An on-chain contract that provides the nominal price of an asset in a reference currency (e.g., ETH/USD) | Oracle, price contract |
| **Aggregator** | The underlying contract that stores and updates the price answer | Price source |
| **Round Data** | A single price observation containing round ID, answer, started-at, updated-at, and answered-in-round | Price update, round |
| **Latest Answer** | The most recent price value from the aggregator | Current price, spot price |

## Relationships

- A **Price Feed** wraps an **Aggregator** contract and exposes a simplified `price` property
- An **Erc20Token** may reference a **Price Feed** for USD-denominated price discovery

## Resolved ambiguities

### Price Feed vs Oracle

**Ruling: **Price Feed** for the Chainlink proxy contract. **Oracle** as the abstract concept. Use "Chainlink Price Feed" specifically, "oracle" generically.**

- ✅ "The ETH/USD **Price Feed** returns 1,500.00"
- ✅ "We need an **oracle** for this token — use a Chainlink Price Feed"
- ❌ "The Chainlink oracle returned 1,500.00" (use **Price Feed**)

## Known issues

### Bot dependency for RPC access

`ChainlinkPriceContract` takes `bot: Bot | None` and calls `self._bot.connections.get_provider(chain_id)` directly in its `decimals` and `price` properties. This bypasses the I/O-free architecture used by pool classes (which receive all data through builders and `external_update()`). A future refactoring should replace the `bot` parameter with a `ProviderAdapter` or `PoolIO` parameter, making the class testable without a live `Bot` instance.
```

### Step 5: Delete `src/degenbot/chainlink.py`

After the package is created and re-exports are verified, delete the original `chainlink.py` file. Note: this must happen in the same slice as the package creation — Python cannot have both `chainlink.py` and `chainlink/` in the same directory, as `import degenbot.chainlink` is ambiguous when both exist.

### Step 6: Update root `CONTEXT-MAP.md`

Add `Chainlink` to the module contexts section:

```
- [Chainlink](src/degenbot/chainlink/CONTEXT.md) — price feeds, aggregators, round data
```

### Design decisions

- **Package, not renamed file**: A `chainlink/` package allows future oracle types (Pyth, Band, Uniswap TWAP) to coexist without further file moves. A renamed file (`oracles/chainlink.py`) would require another move when the second oracle is added.
- **Preserve import path via `__init__.py`**: `from degenbot.chainlink import ChainlinkPriceContract` must continue to work. This is a non-breaking reorganization. The root `src/degenbot/__init__.py` also re-exports it via `from .chainlink import ChainlinkPriceContract` — this continues to work unchanged since the import resolves through the package's `__init__.py`.
- **Delete `CHAINLINK_PRICE_FEED_ABI`, don't extract it**: The constant is dead code — zero callers. The class uses raw `eth_abi.abi.decode` with manual selector computation (`Web3.keccak(text="...")[:4]`), bypassing the ABI entirely. Extracting dead code into `abi.py` would carry it forward for no reason. A future consumer that needs the ABI can add it then.
- **Document the `Bot` dependency smell**: `ChainlinkPriceContract` depends on `Bot` for RPC access, which is inconsistent with the I/O-free architecture. This plan documents the issue in `CONTEXT.md` rather than fixing it — the fix (accepting a `ProviderAdapter` instead of `Bot`) is a separate concern.

## Files Involved

**Primary:**
- `src/degenbot/chainlink/` — new package directory with `__init__.py`, `price_feed.py`, `CONTEXT.md`
- `src/degenbot/chainlink.py` — deleted after migration

**Secondary (verify only — no code changes):**
- `src/degenbot/__init__.py` — re-exports `ChainlinkPriceContract` via `from .chainlink import ChainlinkPriceContract`; continues to work through the package's `__init__.py`
- `src/degenbot/erc20/erc20.py` — imports `from degenbot.chainlink import ChainlinkPriceContract`; unchanged via re-export
- `CONTEXT-MAP.md` — add Chainlink module entry

**Test files (import paths unchanged via re-export):**
- `tests/test_chainlink_price_feed.py` — imports `from degenbot.chainlink import ChainlinkPriceContract`
- `tests/erc20/test_erc20_token.py` — imports `from degenbot.chainlink import ChainlinkPriceContract`

**No change needed:**
- `src/degenbot/erc20/CONTEXT.md` — already references Chainlink generically
- `src/degenbot/bot.py` — doesn't import Chainlink

## Implementation Order

### Slice 1: Create package, delete old file

This must be a single step — Python cannot have both `chainlink.py` and `chainlink/` in the same directory.

1. Create `src/degenbot/chainlink/` directory
2. Create `chainlink/price_feed.py` with the `ChainlinkPriceContract` class (without `CHAINLINK_PRICE_FEED_ABI`; remove `import pydantic_core`)
3. Create `chainlink/__init__.py` with re-exports
4. Delete `src/degenbot/chainlink.py`
5. Verify `from degenbot.chainlink import ChainlinkPriceContract` works
6. Verify `degenbot.ChainlinkPriceContract` works via root `__init__.py`
7. Run: `just test-python` — expect all tests green

### Slice 2: Add CONTEXT.md and update CONTEXT-MAP

1. Create `src/degenbot/chainlink/CONTEXT.md` with oracle vocabulary (including Bot dependency note)
2. Add Chainlink entry to `CONTEXT-MAP.md` module contexts section
3. Run: `just test-python` — expect all tests green (documentation-only)

### Slice 3: Validate and clean up

1. Run `just lint` + `just test-all`
2. Verify `grep -rn "from degenbot.chainlink" src/ tests/` — expect all imports to resolve to the new package
3. Verify `src/degenbot/chainlink.py` no longer exists
4. Verify `CHAINLINK_PRICE_FEED_ABI` no longer exists in the codebase
5. Verify `pydantic_core` is no longer imported in the chainlink package

## Testing

### Per-slice test runs

Each slice runs `just test-python`. The `__init__.py` re-export preserves backward compatibility.

### New unit tests

No new tests needed. The existing `tests/test_chainlink_price_feed.py` covers `ChainlinkPriceContract`. Its import path is unchanged.

### Integration tests

No integration test changes needed. The import path is preserved.

## Benefits

- **Locality**: Price oracle concerns are grouped in one package. If someone adds a Pyth oracle or a Uniswap TWAP oracle, they live in `chainlink/` (or a broader `oracles/` package if that becomes the pattern).
- **Dead code removal**: `CHAINLINK_PRICE_FEED_ABI` and its `pydantic_core` import are deleted rather than carried forward.
- **Navigability**: A contributor scanning the package root sees domain-aligned modules instead of a loose `chainlink.py` that could be confused with infrastructure.
- **Vocabulary**: Future oracle integrations have a reference for naming conventions (Price Feed vs Oracle, etc.).

## Risks

- **Import path breakage**: If the `__init__.py` re-export is incorrect, existing imports break. Mitigation: the re-export is trivial (`from degenbot.chainlink.price_feed import ChainlinkPriceContract`), and the test suite covers the import path via 4 import sites.
- **Package name ambiguity**: `chainlink/` as a package name could suggest it contains all-oracle types, but it's named after the Chainlink protocol specifically. Mitigation: if a second oracle type is added later, the package can be renamed to `oracles/` with sub-modules. The `CONTEXT.md` documents this as the Chainlink-specific module.
- **Dead-code ABI deletion**: If `CHAINLINK_PRICE_FEED_ABI` was intended for future use, deleting it means it must be re-created later. Mitigation: zero callers means zero evidence of intent — it can be trivially reconstructed from the Chainlink repo if needed.

## Relationship to Other Plans

- **Plan 058** (Collapse Subscription Stubs): Completed. Orthogonal. Different module.
- **Plan 059** (Delete Deprecated `build_*` Pass-Throughs): Completed. Orthogonal. Different module.
- **Plan 060** (Unify Builder Orchestration): Orthogonal. Different module.
- **Plan 061** (Delete `EthereumProvider` Alias): Completed. Orthogonal. Different module.
- **Plan 031** (Context Docs Cleanup): Completed. Established the CONTEXT.md pattern. This plan follows that pattern for the new module.

## Status

[x] Slice 1: Create package, delete old file
[x] Slice 2: Add CONTEXT.md and update CONTEXT-MAP
[x] Slice 3: Validate and clean up
