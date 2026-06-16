# Contracts

On-chain executor contracts for MEV arbitrage.

## Files

```
contracts/
├── README.md                            ← you are here
├── cmd_executor_bytecode.txt            ← Init bytecode (for deployment)
├── cmd_executor_init_bytecode.txt       ← Init bytecode (pre-constructor-args)
├── cmd_executor_runtime_bytecode.txt    ← Runtime bytecode + CBOR + immutables (for code injection)
├── cmd_executor_abi.json                ← ABI (for web3.py contract objects)
└── tests/                               ← Ape + Foundry test suite
```

## Bytecode Files

| File | Contents | Use |
|------|----------|-----|
| `*_bytecode.txt` | `vyper -f bytecode` output | Deployment (`cast send --create`) |
| `*_runtime_bytecode.txt` | Runtime bytecode + CBOR + 32-byte-padded immutables | Code injection (`eth_simulateV1`) |

### Vyper bytecode layout

Vyper immutables are loaded via `CODECOPY` from fixed offsets in the deployed
code. The Vyper compiler outputs runtime bytecode as
`[code_section][CBOR_metadata]`, where the CBOR bytes serve dual purpose:
compiler identification AND runtime data (function dispatch jump table at
offset `0x404a`, JUMPDEST target at `0x4046`).

The deployed bytecode appends immutable data **after** the CBOR:

```
[code_section][CBOR_metadata][immutable_data]
```

The CODECOPY offset `0x405c` (= code_section + CBOR size) reads the first
immutable; subsequent offsets read later slots. **The CBOR metadata must NOT
be stripped** --- removing it breaks the jump table, JUMPDEST targets, and
CODECOPY offsets.

The `*_runtime_bytecode.txt` files have immutables pre-appended (after the
CBOR) so no storage overrides are needed.

### cmd_executor immutables (9 × 32 bytes, appended after CBOR metadata)

| Slot | Immutable | Value | Notes |
|------|-----------|-------|-------|
| 0 | `OWNER_ADDR` | `0x9C56a29c7231974c269E24F9FB3c29203039089E` | Must match `EXECUTOR_OWNER` in the backrun script. `execute()` checks `msg.sender == OWNER_ADDR`, so the simulation's `from` address must equal this |
| 1 | `WETH_ADDR` | `0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2` | WETH on mainnet |
| 2 | `POOL_MANAGER_ADDR` | **MUST match the target chain's PoolManager** | Controls all V4 operations. If wrong, V4-hybrid paths revert at ~38K gas |
| 3 | `USER0_ADDR` | `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48` | USDC on mainnet --- used for V4 delta slot precomputation |
| 4 | `USER1_ADDR` | `0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599` | WBTC on mainnet --- used for V4 delta slot precomputation |
| 5 | `WETH_DELTA_SLOT` | `keccak256(self, WETH)` | V4 CurrencyDelta precomputed slot |
| 6 | `NATIVE_DELTA_SLOT` | `keccak256(self, NATIVE)` | V4 CurrencyDelta precomputed slot |
| 7 | `USER0_DELTA_SLOT` | `keccak256(self, USER0)` | V4 CurrencyDelta precomputed slot |
| 8 | `USER1_DELTA_SLOT` | `keccak256(self, USER1)` | V4 CurrencyDelta precomputed slot |

> ⚠️ **Critical**: The `POOL_MANAGER_ADDR` immutable in the runtime bytecode file **must
> match the PoolManager address on the target chain**. On Ethereum mainnet, this is
> `0x000000000004444c5dc75cB358380D2e3De08A90`. The recompile.py script patches it
> automatically. If wrong, all V4-hybrid paths will revert immediately inside
> `execute()` with an empty revert at ~38K gas --- the `V4_UNLOCK` command calls
> `extcall IPoolManager(POOL_MANAGER_ADDR).unlock()` against the wrong address.
> V2-only and V3-only paths work regardless because they never touch the PoolManager.

### Patching immutables

If the runtime bytecode was compiled with wrong immutables (e.g., a throwaway PM address),
you can patch the 9 × 32-byte tail without recompiling:

```python
# Patch POOL_MANAGER_ADDR in the runtime bytecode tail
pm = "0x000000000004444C5dc75cB358380D2e3De08A90"  # mainnet PM
pm_padded = "0" * 24 + pm[2:].lower()  # left-pad to 32 bytes

with open("contracts/cmd_executor_runtime_bytecode.txt") as f:
    code = f.read().strip()[2:]  # strip 0x

# Last 576 hex chars = 9 × 32-byte immutable slots
# Slot layout: [0:OWNER][1:WETH][2:PM]...[8:USER1_DELTA]
tail = code[-576:]
pm_offset = 2 * 64  # slot index 2, each slot is 64 hex chars
new_tail = tail[:pm_offset] + pm_padded + tail[pm_offset + 64:]

with open("contracts/cmd_executor_runtime_bytecode.txt", "w") as f:
    f.write("0x" + code[:-576] + new_tail + "\n")
```

### Recompiling

Source lives in `~/code/executor/` (separate project). The `recompile.py` script
handles the full pipeline: compile, append immutables after CBOR metadata, patch
PM address, and copy:

```bash
python3 contracts/recompile.py          # compile + patch mainnet PM
python3 contracts/recompile.py --no-patch  # compile without PM patch (testnet)
```

The script reads `cmd_executor.vy` from `~/code/executor/`, compiles with Vyper,
appends the 9 × 32-byte immutable slots after the CBOR metadata, patches
`POOL_MANAGER_ADDR` to the mainnet address, and writes all 3 output files into
`contracts/`. The CBOR metadata is preserved in the compiled output --- it must
NOT be stripped (see "Vyper bytecode layout" above).

---

## cmd_executor — Command-Stream Executor

**Vyper**: 0.5.0a2 | **EVM**: Cancun | **Runtime**: 16,764 bytes (code + CBOR + immutables)
**Source**: `~/code/executor/contracts/cmd_executor.vy`
**Tests**: `~/code/executor/tests/` (224 passing, Ape + Foundry)

Compact command-stream executor. All execution decisions are made off-chain and encoded as a byte stream; the contract is a pure command interpreter. Two modes are freely mixed:

1. **Explicit** — off-chain pre-computes amounts (`V4_TAKE`, `ERC20_TRANSFER`)
2. **Dynamic** — on-chain reads from PM exttload (`V4_TAKE_DELTA`, `V4_SETTLE_DELTA`, `V4_SETTLE_ALL`)

Full documentation: `~/code/executor/README.md`

### Interface

```vyper
@payable
def execute(commands: Bytes[512]) -> uint256
```

Single entry point. The command stream format:

```
[0xFE][SET_ADDRESS commands][SKIP_PROFIT_CHECK][0xFF][execution commands]
```

If the first byte is not `0xFE`, the entire stream is execution (no preprocessing).

### Command Set

| Opcode | Name | Encoding | Description |
|--------|------|----------|-------------|
| **Control** |||| |
| `0x00` | SET_ADDRESS | `[0x00][address:20]` | Append address to lookup table |
| `0x01` | SKIP_PROFIT_CHECK | `[0x01]` | Skip post-execution profit check |
| `0x02` | BRIBE_COINBASE | `[0x02][bips:2]` | Bribe block.coinbase |
| `0x03` | BRIBE_ADDRESS | `[0x03][recipient_idx:1][bips:2]` | Bribe arbitrary address |
| `0x04` | TSTORE_CONTINUATION | `[0x04][len:2][commands:N]` | Write commands to tstore |
| **ERC20 / ETH** |||| |
| `0x10` | ERC20_TRANSFER | `[0x10][token_idx:1][recipient_idx:1][amount:32]` | Transfer ERC-20 |
| `0x11` | ERC20_XFER_BALANCE | `[0x11][token_idx:1][recipient_idx:1]` | Transfer entire balance |
| `0x12` | WETH_DEPOSIT | `[0x12][amount:32]` | Wrap ETH → WETH |
| `0x13` | WETH_WITHDRAW | `[0x13][amount:32]` | Unwrap WETH → ETH |
| `0x14` | WETH_DEPOSIT_ALL | `[0x14]` | Wrap all ETH |
| `0x15` | WETH_WITHDRAW_ALL | `[0x15]` | Unwrap all WETH |
| `0x16` | SEND_ETH | `[0x16][recipient_idx:1][amount:16]` | Send uint128 ETH |
| `0x17` | SEND_ETH_ALL | `[0x17][recipient_idx:1]` | Send all ETH |
| **V2** |||| |
| `0x20` | V2_SWAP_COMPACT | `[0x20][pool:1][zfo:1][amt:16][rcpt:1][flen:2][fwd:N]` | V2 swap + forward data |
| `0x21` | V2_SWAP_CALC | `[0x21][pool:1][zfo:1][rcpt:1][fee:2]` | V2 swap from excess balance |
| `0x22` | V2_SWAP_DIRECT | `[0x22][pool:1][zfo:1][amt:16][rcpt:1]` | V2 swap, explicit amount |
| **V3** |||| |
| `0x30` | V3_SWAP_COMPACT | `[0x30][pool:1][zfo:1][amt:16][rcpt:1][flen:2][fwd:N]` | V3 swap + auto-pay |
| `0x31` | V3_SWAP_DELTA | `[0x31][pool:1][zfo:1][rcpt:1]` | V3 swap from PM exttload |
| **V4 Swaps** |||| |
| `0x40` | V4_SWAP_COMPACT | `[0x40][c0:1][c1:1][fee:3][ts:3][hooks:1][zfo:1][amt:16]` | V4 swap, explicit amount |
| `0x41` | V4_SWAP_DYNAMIC | `[0x41][c0:1][c1:1][fee:3][ts:3][hooks:1][zfo:1]` | V4 swap from PM exttload |
| `0x42` | V4_BATCH | `[0x42][n:1][entry:26]...` | Multi-swap + auto-settle |
| **V4 Settlement / ERC6909** |||| |
| `0x50` | V4_UNLOCK | `[0x50][len:2][data:N]` | Enter PM unlock context |
| `0x51` | V4_TAKE | `[0x51][currency:1][rcpt:1][amount:32]` | Take from PM |
| `0x52` | V4_TAKE_COMPACT | `[0x52][currency:1][rcpt:1][amount:16]` | Take, uint128 amount |
| `0x53` | V4_TAKE_DELTA | `[0x53][currency:1][rcpt:1]` | Take from PM exttload |
| `0x54` | V4_SYNC | `[0x54][currency:1]` | Sync at PM (anytime) |
| `0x55` | V4_SETTLE | `[0x55]` | Settle at PM |
| `0x56` | V4_SETTLE_DELTA | `[0x56][currency:1]` | Settle one currency from exttload |
| `0x57` | V4_SETTLE_ALL | `[0x57]` | Settle all nonzero deltas |
| `0x58` | V4_MINT_COMPACT | `[0x58][currency:1][rcpt:1][amount:16]` | Mint ERC6909 (no transfer) |
| `0x59` | V4_BURN_COMPACT | `[0x59][currency:1][amount:16]` | Burn ERC6909 (no transfer) |
| **Separators** |||| |
| `0xFE` | BEGIN_PREPROCESSING | First byte of stream with preprocessing | |
| `0xFF` | BEGIN_EXECUTION | Marks end of preprocessing | |

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
| `enc_preamble()` | Build preprocessing section (SET_ADDRESS + SKIP_PROFIT_CHECK) |
| `V4V4ArbitragePayload` | Build V4→V4 2-pool command stream |
| `V4V3ArbitragePayload` | Build V4→V3 2-pool command stream |
| `CmdExecutorComposer` | `PayloadComposer` impl — composes `SwapAmounts` → command stream |

### Key Design Decisions

1. **PM exttload for authoritative deltas** — V4 deltas read from PoolManager's own transient storage via `exttload()`, eliminating tracker drift. `t_v4_currencies_touched` kept only for V4_SETTLE_ALL iteration.
2. **V3 auto-pay** — Empty `forward_data` → callback reads owed amounts from parameters, auto-transfers. Saves ~37 bytes/swap.
3. **V2 auto-pay** — `V2_AUTO_PAY_SENTINEL` (1-byte `0xFE` data) triggers auto-pay in callback via `_v2_auto_pay()`.
4. **Address table in-stream** — SET_ADDRESS commands inside the stream replace the separate `addresses[]` parameter. 1-byte indices save ~19 bytes per reference.
5. **V4_BATCH (0x42)** — Packs multiple swaps into one command with auto-settle. Dynamic amounts decoded from previous swap's BalanceDelta return value.
6. **ERC6909 mint/burn** — V4_MINT_COMPACT/V4_BURN_COMPACT for internal PM balance holding without physical transfers. Saves ~20K gas vs V4_TAKE.
7. **V2_SWAP_CALC excess balance** — Reads `balanceOf(pair) - reserves` for input amount from tokens deposited via V4_TAKE directly to V2 pair.
8. **Address table index hygiene** — `AddressTable` deduplicates by checksummed address. In list comprehensions building `pool_indices`, the iteration variable **must** match the attribute access variable (e.g., `at.add(h.pool_address) for h in hops`, not `at.add(hop.pool_address) for h in hops`). A mismatch silently references an outer-scope variable from a prior loop, causing all pool addresses to resolve to the same deduplicated index. The resulting command stream calls the wrong pool for every V2_SWAP_CALC/V2_SWAP_COMPACT, producing `V2_SWAP_CALC: no excess balance` reverts.
8. **Tstore continuation (0x04)** — For protocols without data passthrough, stores commands in transient storage; `onExternalCallback()` reads and processes them.
9. **0xFE/0xFF preprocessing** — SET_ADDRESS, SKIP_PROFIT_CHECK, BRIBE live inside the command stream, eliminating DynArray overhead (~190+ bytes saved for 3 addresses).
10. **Function extraction for Venom** — Dispatch split into 21+ `@internal` `_cmd_*` functions, enabling Venom's `ConcretizeMemLocPass` to reclaim memory across handlers. Memory: 22,976 → 8,544 (−62.8%).

### V3 vs V4 amountSpecified Sign Convention

| | Exact INPUT | Exact OUTPUT |
|---|---|---|
| **V3** | `amountSpecified > 0` | `amountSpecified < 0` |
| **V4** | `amountSpecified < 0` | `amountSpecified > 0` |

For arbitrage (always exact-input): V3 uses **positive** values, V4 uses **negative** values.
