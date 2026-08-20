# Contracts

On-chain executor contracts for MEV arbitrage.

## Files

```
contracts/
├── README.md                            ← you are here
├── cmd_executor_bytecode.txt            ← Creation bytecode (deployment + constructor args)
├── cmd_executor_runtime_bytecode.txt    ← Runtime bytecode + CBOR + immutables (for code injection)
├── cmd_executor_abi.json                ← ABI (for web3.py contract objects)
└── tests/                               ← Ape + Foundry test suite
```

## Bytecode Files

| File | Contents | Use |
|------|----------|-----|
| `*_bytecode.txt` | `vyper -f bytecode` output (artifact creation hex) | Deployment (`cast send --create` + constructor args `(weth, pool_manager)`) |
| `*_runtime_bytecode.txt` | Runtime bytecode + CBOR + 5 x 32-byte-padded immutables | Code injection (`eth_simulateV1`) |

### Vyper bytecode layout

Vyper immutables are loaded via `CODECOPY` from fixed offsets in the deployed
code. The Vyper compiler outputs runtime bytecode as
`[code_section][CBOR_metadata]`, where the CBOR bytes serve dual purpose:
compiler identification AND runtime data (the function dispatch jump table
and its JUMPDEST targets, at compiler-generated offsets)

The deployed bytecode appends immutable data **after** the CBOR:

```
[code_section][CBOR_metadata][immutable_data]
```

The CODECOPY offsets are compiler-generated against the end of the runtime
section (with the current 16,097-byte artifact the first slot sits at
0x3EE1) --- the one invariant the bake must hold is that the 160-byte
immutables tail is appended AFTER the runtime (i.e. after the CBOR).
**The CBOR metadata must NOT be stripped** --- removing it breaks the jump
table, JUMPDEST targets, and CODECOPY offsets.

The `*_runtime_bytecode.txt` file has the immutable tail appended after the
CBOR, so injection needs no storage overrides.

### cmd_executor immutables (5 × 32 bytes, appended after CBOR metadata)

| Slot | Immutable | Value | Notes |
|------|-----------|-------|-------|
| 0 | `OWNER_ADDR` | `0x9C56a29c7231974c269E24F9FB3c29203039089E` | Must match `EXECUTOR_OWNER` in the settlement-arbitrage script. `execute()` checks `msg.sender == OWNER_ADDR`, so the simulation's `from` address must equal this |
| 1 | `WETH_ADDR` | `0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2` | WETH on mainnet |
| 2 | `POOL_MANAGER_ADDR` | **MUST match the target chain's PoolManager** | Controls all V4 operations. If wrong, V4-hybrid paths revert at ~38K gas |
| 3 | `WETH_DELTA_SLOT` | `keccak256(self, WETH)` | V4 CurrencyDelta precomputed slot |
| 4 | `NATIVE_DELTA_SLOT` | `keccak256(self, NATIVE)` | V4 CurrencyDelta precomputed slot |

The constructor is `__init__(weth, pool_manager)`. Only the two hot protocol
currencies (WETH, NATIVE) get precomputed delta slots; every other currency
(including path-specific tokens like USDC/WBTC) computes its delta slot
on-chain via `keccak256`. Per-path token immutables (the old `USER0`/`USER1`)
were removed --- those tokens now go through `t_addresses` via `SET_ADDRESS`.

> ⚠️ **Critical**: The `POOL_MANAGER_ADDR` immutable in the runtime bytecode file **must
> match the PoolManager address on the target chain**. On Ethereum mainnet, this is
> `0x000000000004444c5dc75cB358380D2e3De08A90`. The bake (`contracts/recompile.py`)
> writes it in by default; `--no-patch` bakes a zero PM (testnet/dev) If wrong, all V4-hybrid paths will revert immediately inside
> `execute()` with an empty revert at ~38K gas --- the `V4_UNLOCK` command calls
> `extcall IPoolManager(POOL_MANAGER_ADDR).unlock()` against the wrong address.
> V2-only and V3-only paths work regardless because they never touch the PoolManager.

### Patching the injected-code tail

After an unpatched bake (`--no-patch`, PM slot zero), point the PoolManager at the
target chain by patching slot 2 of the 160-byte tail — no re-bake needed:

```python
# Patch POOL_MANAGER_ADDR in the injected runtime tail
pm = "0x000000000004444C5dc75cB358380D2e3De08A90"  # mainnet PM
pm_padded = "0" * 24 + pm[2:].lower()  # left-pad to 32 bytes

with open("contracts/cmd_executor_runtime_bytecode.txt") as f:
    code = f.read().strip()[2:]  # strip 0x

# Last 320 hex chars = 5 × 32-byte immutable slots
# Slot layout: [0:OWNER][1:WETH][2:PM][3:WETH_DELTA][4:NATIVE_DELTA]
tail = code[-320:]
pm_offset = 2 * 64  # slot index 2, each slot is 64 hex chars
new_tail = tail[:pm_offset] + pm_padded + tail[pm_offset + 64:]

with open("contracts/cmd_executor_runtime_bytecode.txt", "w") as f:
    f.write("0x" + code[:-320] + new_tail + "\n")
```

A real (non-injected) deployment needs NO file patching: the constructor is
`__init__(weth, pool_manager)`, `OWNER_ADDR` is `msg.sender` (the deployer),
and the two delta slots are computed on-chain from the deployed address.

### Rebaking the injected bytecode (X6OKMV re-sync)

`contracts/recompile.py` bakes the `contracts/cmd_executor_*` files from the
COMMITTED tier-3 artifacts (toolchain-free): the runtime file is `0x` +
`tier3-oracle/artifacts/executor/cmd_executor.runtime.hex` + the 5 x 32-byte
immutable slots (mainnet values, delta slots precomputed for
`INJECTED_EXECUTOR_ADDRESS`); the two init/bytecode files are the artifact
creation hex; the ABI is copied verbatim. Before writing, it pins the
artifacts to the current in-repo source via the manifest source-sha
(fail-closed on drift -> run `just rebuild-tier3-artifacts`).

```bash
uv run python contracts/recompile.py            # default: mainnet PM baked
uv run python contracts/recompile.py --no-patch # POOL_MANAGER slot baked as zero (testnet/dev; patch later with the recipe above)
uv run python contracts/recompile.py --compile  # additionally compile the in-repo source (vyper 0.5.0a3 via the executor uv project) and fail on any sha256 drift vs the artifacts
```

The previous pipeline compiled from the retired `~/code/executor/` path
(pre-vendoring); derivation from the committed artifacts replaces it outright.

---

## cmd_executor — Command-Stream Executor

**Vyper**: 0.5.0a3 | **EVM**: Cancun | **Runtime**: 16,257 bytes (16,097 B code + CBOR + 160 B immutables)
**Source**: in-repo `executor/contracts/cmd_executor.vy` (vendored); committed artifacts in `tier3-oracle/artifacts/executor/` (vyper 0.5.0a3, compile-vs-use gate: `just verify-tier3-executor-artifact`)
**Tests**: in-repo `executor/tests/` (Ape + Foundry)

Compact command-stream executor. All execution decisions are made off-chain and encoded as a byte stream; the contract is a pure command interpreter. Two modes are freely mixed:

1. **Explicit** — off-chain pre-computes amounts (`V4_TAKE`, `ERC20_TRANSFER`)
2. **Dynamic** — on-chain reads from PM exttload (`V4_TAKE_DELTA`, `V4_SETTLE_DELTA`, `V4_SETTLE_ALL`)

Full documentation: `executor/README.md`

### Interface

```vyper
@payable
def execute(commands: Bytes[288], config: uint256 = 0) -> uint256
```

Single entry point. `config` packs profit-check mode, bribe bips/recipient,
and expected pre-tx balance (see `pack_config()` in `contracts.cmd_stream`):
`config = (expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | check_mode`.
`config=0` skips the on-chain profit check and sends no bribe.

The command stream format:

```
[SET_ADDRESS commands][0xFF][execution commands]
```

`_preprocess` reads opcodes from offset 0 and accepts ONLY `0x00`
(SET_ADDRESS) and `0xFF` (BEGIN_EXECUTION). There is no `0xFE` prefix.
Opcodes `0x01`–`0x03` are reserved (revert if emitted) --- the
SKIP_PROFIT_CHECK / BRIBE behavior they used to encode moved into `config`.

### Command Set

| Opcode | Name | Encoding | Description |
|--------|------|----------|-------------|
| **Control** |||| |
| `0x00` | SET_ADDRESS | `[0x00][address:20]` | Append address to lookup table |
| `0x01`–`0x03` | *(reserved)* | — | Were SKIP_PROFIT_CHECK / BRIBE_COINBASE / BRIBE_ADDRESS; now packed into `config` — emit reverts |
| **ERC20 / ETH** |||| |
| `0x10` | ERC20_TRANSFER | `[0x10][token_idx:1][recipient_idx:1][amount:12]` | Transfer ERC-20 (uint96 amount) |
| `0x11` | ERC20_XFER_BALANCE | `[0x11][token_idx:1][recipient_idx:1]` | Transfer entire balance |
| `0x12` | WETH_DEPOSIT | `[0x12][amount:32]` | Wrap ETH to WETH |
| `0x13` | WETH_WITHDRAW | `[0x13][amount:32]` | Unwrap WETH to ETH |
| `0x14` | WETH_DEPOSIT_ALL | `[0x14]` | Wrap all ETH |
| `0x15` | WETH_WITHDRAW_ALL | `[0x15]` | Unwrap all WETH |
| `0x16` | SEND_ETH | `[0x16][recipient_idx:1][amount:12]` | Send uint96 ETH |
| `0x17` | SEND_ETH_ALL | `[0x17][recipient_idx:1]` | Send all ETH |
| **V2** |||| |
| `0x20` | V2_SWAP_COMPACT | `[0x20][pool:1][zfo:1][amt:12][rcpt:1][fee:2][flen:1][fwd:N]` | V2 swap + forward data (uint96 amt) |
| `0x21` | V2_SWAP_CALC | `[0x21][pool:1][zfo:1][rcpt:1][fee:2]` | V2 swap from excess balance |
| `0x22` | V2_SWAP_DIRECT | `[0x22][pool:1][zfo:1][amt:12][rcpt:1]` | V2 swap, explicit amount |
| **V3** |||| |
| `0x30` | V3_SWAP_COMPACT | `[0x30][pool:1][zfo:1][amt:12][rcpt:1][flen:1][fwd:N]` | V3 swap + auto-pay (uint96 amt) |
| `0x31` | V3_SWAP_DELTA | `[0x31][pool:1][zfo:1][rcpt:1]` | V3 swap from PM exttload |
| **V4 Swaps** |||| |
| `0x40` | V4_SWAP_COMPACT | `[0x40][c0:1][c1:1][fee:2][ts:2][hooks:1][zfo:1][amt:12]` | V4 swap, explicit amount (uint96) |
| `0x41` | V4_SWAP_DYNAMIC | `[0x41][c0:1][c1:1][fee:2][ts:2][hooks:1][zfo:1]` | V4 swap from PM exttload |
| `0x42` | V4_BATCH | `[0x42][n:1][entry:20]...` | Multi-swap + auto-settle (max 8) |
| `0x43` | V4_BATCH_OPEN_WETH | `[0x43][n:1][entry:20]...` | As `0x42` but skips the WETH tail-settle (leaves the positive WETH delta open for a follow-up `V4_MINT_COMPACT` — ERC6909 capture; TGUZCT) |
| **V4 Settlement / ERC6909** |||| |
| `0x50` | V4_UNLOCK | `[0x50][len:1][data:N]` | Enter PM unlock context |
| `0x51` | V4_TAKE | `[0x51][currency:1][rcpt:1][amount:32]` | Take from PM |
| `0x52` | V4_TAKE_COMPACT | `[0x52][currency:1][rcpt:1][amount:12]` | Take, uint96 amount |
| `0x53` | V4_TAKE_DELTA | `[0x53][currency:1][rcpt:1]` | Take from PM exttload |
| `0x54` | V4_SYNC | `[0x54][currency:1]` | Sync at PM (anytime) |
| `0x55` | V4_SETTLE | `[0x55]` | Settle at PM |
| `0x56` | V4_SETTLE_DELTA | `[0x56][currency:1]` | Settle one currency from exttload |
| `0x57` | V4_SETTLE_ALL | `[0x57]` | Settle all nonzero deltas |
| `0x58` | V4_MINT_COMPACT | `[0x58][currency:1][rcpt:1][amount:12]` | Mint ERC6909 (no transfer) |
| `0x59` | V4_BURN_COMPACT | `[0x59][currency:1][amount:12]` | Burn ERC6909 (no transfer) |
| **Separators** |||| |
| `0xFF` | BEGIN_EXECUTION | Marks end of preprocessing / start of execution | |

### Callbacks

| Callback | Source | Selector |
|----------|--------|----------|
| `uniswapV2Call` | Uniswap V2, SushiSwap | `0x10d1e85c` |
| `hook` | Aerodrome, Velodrome | `0x9a7bff79` |
| `pancakeCall` | PancakeSwap V2 | `0x84800812` |
| `uniswapV3SwapCallback` | Uniswap V3, SushiSwap | `0xfa461e33` |
| `pancakeV3SwapCallback` | PancakeSwap V3 | `0x23a69e75` |
| `unlockCallback` | Uniswap V4 PM | `0x91dd7346` |
| `onExternalCallback` | External protocols | `0x8a6be710` |

### Python Encoding

`contracts.cmd_stream` provides:

| Component | Purpose |
|-----------|---------|
| `AddressTable` | Track addresses for compact index-based referencing |
| `enc_v4_swap_compact()`, `enc_v4_take()`, etc. | Individual command encoders |
| `enc_preamble()` | Build preprocessing section (SET_ADDRESS commands + `0xFF`) |
| `pack_config()` | Pack the `execute()` `config` param (check_mode + bribe bips/recipient + expected_value) |
| `V4V4ArbitragePayload` | Build V4→V4 2-pool command stream |
| `V4V3ArbitragePayload` | Build V4→V3 2-pool command stream |
| `CmdExecutorComposer` | `PayloadComposer` impl — composes `SwapAmounts` → command stream |

### Key Design Decisions

1. **PM exttload for authoritative deltas** — V4 deltas read from PoolManager's own transient storage via `exttload()`, eliminating tracker drift. `V4_SETTLE_ALL` iterates `t_addresses` directly --- no touched-currency tracking is stored.
2. **V3 auto-pay** — Empty `forward_data` → callback reads owed amounts from parameters, auto-transfers. Saves ~37 bytes/swap.
3. **V2 auto-pay** — `V2_AUTO_PAY_SENTINEL` (1-byte `0xFE` data) triggers auto-pay in callback via `_v2_auto_pay()`.
4. **Address table in-stream** — SET_ADDRESS commands inside the stream replace the separate `addresses[]` parameter. 1-byte indices save ~19 bytes per reference.
5. **V4_BATCH (0x42)** — Packs multiple swaps into one command with auto-settle. Dynamic amounts decoded from previous swap's BalanceDelta return value.
6. **ERC6909 mint/burn** — V4_MINT_COMPACT/V4_BURN_COMPACT for internal PM balance holding without physical transfers. Saves ~20K gas vs V4_TAKE.
7. **V2_SWAP_CALC excess balance** — Reads `balanceOf(pair) - reserves` for input amount from tokens deposited via V4_TAKE directly to V2 pair.
8. **Address table index hygiene** — `AddressTable` deduplicates by checksummed address. In list comprehensions building `pool_indices`, the iteration variable **must** match the attribute access variable (e.g., `at.add(h.pool_address) for h in hops`, not `at.add(hop.pool_address) for h in hops`). A mismatch silently references an outer-scope variable from a prior loop, causing all pool addresses to resolve to the same deduplicated index. The resulting command stream calls the wrong pool for every V2_SWAP_CALC/V2_SWAP_COMPACT, producing `V2_SWAP_CALC: no excess balance` reverts.
9. **Protocol sentinels only** — Only 4 address indices are sentinels: `0xFC`=PM, `0xFD`=self, `0xFE`=WETH, `0xFF`=NATIVE. Per-path tokens (USDC, WBTC, ...) are NOT baked into the contract --- they go through `t_addresses` via SET_ADDRESS. The old `USER0`/`USER1` sentinels were removed to avoid benchmark-overfitting and silent mis-resolution.
10. **Config in ABI param, not the stream** — Profit check mode, bribe bips/recipient, and expected pre-tx balance are packed into the `config` uint256 argument of `execute()` (see `pack_config()`). The old stream opcodes `0x01`–`0x03` (SKIP_PROFIT_CHECK / BRIBE_COINBASE / BRIBE_ADDRESS) are reserved --- emitting them reverts.
11. **Function extraction for Venom** — Dispatch split into 21+ `@internal` `_cmd_*` functions, enabling Venom's `ConcretizeMemLocPass` to reclaim memory across handlers. Memory: 22,976 → 8,544 (−62.8%).

### V3 vs V4 amountSpecified Sign Convention

| | Exact INPUT | Exact OUTPUT |
|---|---|---|
| **V3** | `amountSpecified > 0` | `amountSpecified < 0` |
| **V4** | `amountSpecified < 0` | `amountSpecified > 0` |

For arbitrage (always exact-input): V3 uses **positive** values, V4 uses **negative** values.
