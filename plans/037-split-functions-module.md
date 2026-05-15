# Plan 037: Split `functions.py` into Domain-Aligned Modules

## Status: PROPOSED

## Overview

Decompose the `functions.py` utility grab-bag (~350 lines, 14 public functions + 1 internal class) into domain-aligned modules. Each module has a coherent interface providing one category of functionality: ABI encoding/call helpers, log fetching, contract address derivation, and EVM math/validation.

## Files Involved

**Primary (source):**
- `src/degenbot/functions.py` — decompose and delete

**Primary (new modules):**
- `src/degenbot/provider/call_helpers.py` — `raw_call`, `async_raw_call`, `encode_function_calldata`, `extract_argument_types_from_function_prototype`
- `src/degenbot/provider/log_fetching.py` — `fetch_logs_retrying`, `fetch_logs_retrying_async`, `_ChunkedLogFetcher`
- `src/degenbot/contract/addresses.py` — `create2_address`, `eip_1167_clone_address`
- `src/degenbot/calculations/evm_math.py` — `evm_divide`, `next_base_fee`, `raise_if_invalid_uint256`
- `src/degenbot/provider/block_helpers.py` — `get_number_for_block_identifier`, `get_number_for_block_identifier_async`

**No new module for `eip_191_hash`** — see Step 5 (dead code).

**Secondary (update imports):**
- 56 import sites across `src/` and `tests/` — update to new module paths
- `src/degenbot/__init__.py` — check for re-exports

## Problem

`functions.py` contains unrelated utilities that fail the coherent-interface test:

| Function | Domain | Src import count | Tests import count |
|---|---|---|---|
| `encode_function_calldata` | ABI encoding | 26 | 8+ |
| `raw_call` | Provider RPC | 15 | 3 |
| `evm_divide` | EVM math | 5 | 1 |
| `fetch_logs_retrying` | Log fetching | 3 | 0 |
| `raise_if_invalid_uint256` | Validation | 2 | 0 |
| `fetch_logs_retrying_async` | Async log fetching | 2 | 0 |
| `create2_address` | Address derivation | 2 | 0 |
| `get_number_for_block_identifier` | Block ID | 1 | 0 |
| `eip_1167_clone_address` | Address derivation | 1 | 0 |
| `async_raw_call` | Async RPC | 1 | 0 |
| `next_base_fee` | EVM gas math | 0 src | 1 test |
| `eip_191_hash` | Cryptographic signing | 0 | 0 |

The module is semantically flat — importing `raw_call` also pulls in `eip_191_hash` and `next_base_fee`. The **deletion test** confirms: deleting `functions.py` would require each function to reappear somewhere, but they'd naturally land in domain-appropriate homes — proving they never belonged together.

`eip_191_hash` is never imported anywhere. It should be deleted, not moved.

`next_base_fee` is only used by `anvil_fork.py`'s `set_next_base_fee()` method internally (the function is defined in `functions.py` but `anvil_fork.py` calls its own method). The only import is in `test_functions.py`. The function is legitimate EVM gas math, so it moves to `calculations/evm_math.py`, but it's worth noting its limited direct usage.

## Solution

### Step 1: Create new modules

Move functions into domain-aligned locations. Each module's interface is coherent:

#### `src/degenbot/provider/call_helpers.py`

Low-level RPC call helpers that bridge `ProviderAdapter` and ABI encoding.

```python
"""Low-level RPC call helpers.

Thin wrappers around ProviderAdapter.call() that handle
ABI encoding/decoding and block identifier resolution.
"""

def encode_function_calldata(function_prototype, function_arguments) -> bytes: ...
def extract_argument_types_from_function_prototype(function_prototype) -> list[str]: ...
def raw_call(provider, address, calldata, return_types, block_identifier) -> tuple: ...
async def async_raw_call(provider, address, calldata, return_types, block_identifier) -> tuple: ...
```

Note: `encode_function_calldata` and `extract_argument_types_from_function_prototype` are pure ABI helpers that don't use `ProviderAdapter` — they could go in a separate `abi_helpers.py`. But they're always used in the same call chain as `raw_call` (encode calldata → call → decode response), and keeping them together with the call wrapper is simpler. The module name `call_helpers` signals "helpers for making contract calls."

#### `src/degenbot/provider/log_fetching.py`

Retry-aware log fetching with adaptive chunk sizing.

```python
"""Retry-aware log fetching with adaptive chunk sizing."""

class _ChunkedLogFetcher: ...
def fetch_logs_retrying(*, provider, start_block, end_block, ...) -> list[LogReceipt]: ...
async def fetch_logs_retrying_async(*, provider, start_block, end_block, ...) -> list[LogReceipt]: ...
```

#### `src/degenbot/contract/addresses.py`

Deterministic address derivation for CREATE2 and EIP-1167.

```python
"""Deterministic contract address derivation."""

def create2_address(deployer, salt, init_code_hash) -> ChecksumAddress: ...
def eip_1167_clone_address(deployer, implementation_contract, salt) -> ChecksumAddress: ...
```

#### `src/degenbot/calculations/evm_math.py`

EVM arithmetic, gas, and validation helpers.

```python
"""EVM arithmetic, gas math, and value validation."""

def evm_divide(numerator, denominator) -> int: ...
def next_base_fee(*, parent_base_fee, parent_gas_used, parent_gas_limit, ...) -> int: ...
def raise_if_invalid_uint256(number) -> None: ...
```

#### `src/degenbot/provider/block_helpers.py`

Block identifier resolution (int, string tag, hex).

```python
"""Block identifier resolution helpers."""

def get_number_for_block_identifier(identifier, provider) -> BlockNumber: ...
async def get_number_for_block_identifier_async(identifier, provider) -> BlockNumber: ...
```

### Step 2: Add backward-compatible re-exports in `functions.py`

To avoid breaking all 56 callers in a single commit, keep `functions.py` as a re-export shim:

```python
"""Backward-compatible re-exports.

All functions have been moved to domain-aligned modules.
Import from the new locations directly; this module will be removed
in a future release.
"""

from degenbot.provider.call_helpers import (
    encode_function_calldata,
    extract_argument_types_from_function_prototype,
    raw_call,
    async_raw_call,
)
from degenbot.provider.log_fetching import (
    fetch_logs_retrying,
    fetch_logs_retrying_async,
)
from degenbot.contract.addresses import (
    create2_address,
    eip_1167_clone_address,
)
from degenbot.calculations.evm_math import (
    evm_divide,
    next_base_fee,
    raise_if_invalid_uint256,
)
from degenbot.provider.block_helpers import (
    get_number_for_block_identifier,
    get_number_for_block_identifier_async,
)
```

### Step 3: Migrate callers file-by-file

Update each file that imports from `degenbot.functions` to use the new module paths. Priority order by call frequency:

1. **`encode_function_calldata`** — 26 src sites. Most are builders, curve detection, and CLI commands.
2. **`raw_call`** — 15 src sites. Builders, ERC20, CLI.
3. **`evm_divide`** — 5 src sites. V3/V4 libraries.
4. **`fetch_logs_retrying` / `fetch_logs_retrying_async`** — 5 src sites. V3/V4 snapshots, CLI.
5. **`raise_if_invalid_uint256`** — 2 src sites. Calculations.
6. **`create2_address`** — 2 src sites. V2/V3 factory functions.
7. **Remaining** — 1-site imports.

Each migration: update import, run `just test-python`, commit.

### Step 4: Delete `functions.py`

Once all callers have been migrated to the new import paths, delete the backward-compatibility shim.

Since this is a dev branch with no external consumers, no deprecation period is needed. The re-export shim in Step 2 exists solely to allow incremental migration — once Step 3 is complete, `functions.py` can be deleted immediately.

### Step 5: Delete `eip_191_hash`

`eip_191_hash` is never imported anywhere in `src/` or `tests/`. It's dead code. Delete it instead of moving it.

## Implementation Order

1. **Step 1**: Create new modules (pure copy + adjust imports within each file). One commit.
2. **Step 2**: Add backward-compatible re-exports in `functions.py`. Same commit as Step 1.
3. **Step 3**: Migrate callers (incremental, one file at a time). Multiple small commits or one batch commit.
4. **Step 4**: Delete `functions.py` + delete `eip_191_hash`. One commit.
5. Run `just test-python` after each step.

Step 1+2 is the safe setup. Step 3 can be done in batches per target module. Step 4 is cleanup.

## Testing

### Regression testing

- `just test-python` after each caller migration in Step 3.
- No functional changes — functions have identical implementations, just different module locations.

### Import hygiene

After Step 3, verify no file imports from `degenbot.functions`:

```bash
grep -r "from degenbot.functions import\|from degenbot import functions" src/ tests/
```

Should return zero matches.

### Test file reorganization

Existing tests for these functions can stay in `tests/test_functions.py` initially. The functions haven't changed, only their location. After Step 4, the test file should be split to match the new modules:

- `tests/provider/test_call_helpers.py`
- `tests/provider/test_log_fetching.py`
- `tests/calculations/test_evm_math.py`
- etc.

This is cosmetic and can happen later.

## Benefits

- **Leverage**: Each module has a coherent interface. Import paths communicate intent — `from degenbot.provider.call_helpers import raw_call` vs `from degenbot.functions import raw_call`.
- **Locality**: ABI encoding helpers live next to the provider code they wrap. EVM math lives in the calculations domain. Log fetching lives next to the provider interface. No more "which of these 14 functions do I need?"
- **Discoverability**: A new contributor looking for RPC call helpers finds them in `provider/call_helpers.py`, not buried in a 350-line utility file between CREATE2 and EIP-191.
- **Dead code removal**: `eip_191_hash` is deleted instead of perpetuated.

## Risks

- **Import churn**: 56 import sites across `src/` and `tests/` need updating. This is mechanical but tedious. The backward-compatibility shim in Step 2 allows incremental migration — no big-bang risk.
- **No circular import risk**: `functions.py` only depends on foundation modules (`degenbot.provider`, `degenbot.checksum_cache`, `degenbot.constants`, `degenbot.exceptions`, `degenbot.logging`). The proposed new locations keep these same dependencies, so no circular imports are possible.
- **External consumers**: Dev branch, no external consumers. No deprecation period needed.

## Relationship to Other Plans

- **Plan 028** (Builder Registry): Complete. Builders already use `raw_call` and `encode_function_calldata` from `functions.py`. Moving these to `provider/call_helpers.py` doesn't change builder logic.
- **Plan 025** (Remove Web3 Bypass): Complete. All RPC calls route through `ProviderAdapter`. `raw_call` wrapping `ProviderAdapter.call()` naturally belongs in `provider/`.
- Independent of Plans 033–036 — this is pure module organization with no behavior change.
