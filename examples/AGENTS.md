# AGENTS.md — examples/

## Executor Contract

The on-chain arbitrage executor contracts live in a sibling repo at `~/code/executor/`.

Two implementations exist — **cmd_executor** (compact command stream, production) and **tstore_executor** (static payload queue, legacy). Both execute identical arbitrage paths via different dispatch mechanisms.

### Key documentation

| Doc | Path | When to read |
|-----|------|-------------|
| README (full command set, gas benchmarks, test suite) | `~/code/executor/README.md` | Before modifying encoding logic or adding new command types |
| Pool mechanics (sync/settle ordering, IIA, K-invariant, reverse-order execution) | `~/code/executor/docs/pool-mechanics.md` | Before constructing multi-hop swap paths or debugging reverts |
| PM as bank (zero-fee flash loans from PoolManager) | `~/code/executor/docs/pm-as-bank.md` | When routing working capital through V4 for non-V4 paths |
| ERC6909 arbitrage (mint/burn vs take/settle) | `~/code/executor/docs/erc6909-arbitrage.md` | When optimizing V4→V4 settlement or using internal PM balances |
| Transfer count investigation (why "4 transfer" claim was wrong) | `~/code/executor/docs/transfer-count-investigation.md` | Historical context on transfer counting methodology |
| Fake contract audit (invariant enforcement) | `~/code/executor/FAKE_CONTRACT_AUDIT.md` | When verifying test coverage matches on-chain behavior |
| Security review | `~/code/executor/SECURITY_REVIEW.md` | Before deploying or modifying the executor |

### Contract source

| Contract | Path | Purpose |
|----------|------|---------|
| `cmd_executor.vy` | `~/code/executor/contracts/cmd_executor.vy` | Production executor (Vyper 0.5.0a2, Venom codegen) |
| `tstore_executor.vy` | `~/code/executor/contracts/tstore_executor.vy` | Legacy executor |
| Fake V2 pair | `~/code/executor/contracts/fake_uniswap_v2_pair.vy` | Test double with K-invariant, configurable fee, 3 callback variants |
| Fake V3 pool | `~/code/executor/contracts/fake_uniswap_v3_pool.vy` | Test double with balance-delta check (IIA) |
| Fake V4 PM | `~/code/executor/contracts/fake_uniswap_v4_pool_manager.vy` | Test double with exttload + ERC6909 |

### Tests

224 tests in `~/code/executor/tests/`. Run with:

```bash
cd ~/code/executor && uv run ape test tests/ -v -s
```

Key test files:
- `test_cmd_executor_three_hop_permutations.py` — all 27 V2/V3/V4 three-hop permutations
- `test_cmd_executor_three_hop_optimized.py` — all 27 with optimal routing (≤4 transfers)
- `test_cmd_executor_three_pool_v2.py` — V2-V2-V2 with Approach 1/2/3 comparison
- `test_v2_configurable_fee.py` — per-swap fee handling for V2 forks
- `test_v2_swap_calc_excess.py` — V2_SWAP_CALC with excess balance

### zfo is NOT deterministic from token names

The `zfo` flag (zero_for_one) depends on the deployed pair's `token0`/`token1` ordering, which is determined by `sorted([addr_a, addr_b])`. **A token's name or symbol does NOT determine its position.** On mainnet, COMP (lower address) is `token0` in the COMP/WETH pair, but USDC (lower address) is `token0` in the USDC/COMP pair. When writing Ape tests with fake contracts, the auto-generated addresses may produce different orderings — always compute `zfo` dynamically from the deployed pair:

```python
zfo = (pool.token0() == selling_token_address)
```

### resolve_directions: zfo from token flow

The `resolve_directions()` function in `eth_backrun_v2_v3_v4_rust.py` computes per-hop `zfo` flags by tracing the token flow through the path. Starting from the WETH input token, it walks each pool's `token0`/`token1` to determine which token is being sold, then sets `zfo=True` if `token0` is the selling token. This is the authoritative source for `zfo` — the encoding functions receive these flags as input and must not recompute them.

V4 pools use `NATIVE_CURRENCY_ADDRESS` (address(0)) for ETH, which is treated as equivalent to WETH for matching purposes.

## Pool Mechanics Quick Reference

Read `~/code/executor/docs/pool-mechanics.md` in full before changing encoding. The critical constraints:

- **V2**: K-invariant checks total balances — timing of deposits doesn't matter. But `swap(to=X)` calls `uniswapV2Call(X)`, so forward-order V2→V2 flash swaps are impossible.
- **V3**: IIA balance-delta check — tokens must arrive *during* the callback window, not before `swap()` starts. This forces reverse-order execution for V3 chains.
- **V4**: All deltas must net to zero by end of `unlock()`. Sync before deposit, settle after.

### Reverse-order execution

All multi-hop paths use reverse-order (start from the last pool, chain backwards):

```
V2c.swap(to=executor) → callback:
  Transfer to V2a → V2a V2_SWAP_DIRECT/CALC → V2b V2_SWAP_DIRECT/CALC → V2c
```

```
V3c.swap(to=executor) → callback:
  V3b.swap(to=V3c) → callback:
    V3a.swap(to=V3b) → callback:
      auto-pay WETH to V3a
```

### V2 fee handling

V2 forks have different fees (Uniswap 0.3%, PancakeSwap 0.25%, Aerodrome 0.04%–0.3%). The executor stores the per-pair fee in transient storage (`t_v2_pair_fee`) and computes owed amounts with `_v2_get_amount_in()`. The solver must use the correct fee for each pool.

### Address table index hygiene

The `AddressTable` deduplicates by checksummed address — calling `at.add()` twice with the same address returns the same index. When building `pool_indices` in list comprehensions, **the iteration variable must match the attribute access variable**. A mismatch (e.g., `for h in hops` but referencing `hop.pool_address`) silently references an outer-scope `hop` bound to the **last** iteration of a prior loop, causing all pool addresses to resolve to the same deduplicated index. The resulting command stream calls the wrong pool for every hop.

```python
# Bad: iterates as `h` but accesses `hop` from outer scope
pool_indices = [at.add(hop.pool_address) for h in path_info.hops]

# Good: iteration variable matches attribute access
pool_indices = [at.add(h.pool_address) for h in path_info.hops]
```

This class of bug produces zero sub-calls in `debug_traceCall` — the executor calls the wrong pool immediately and reverts. It is invisible in unit tests with fake contracts unless the test also validates the address table order.

## Runtime Bytecode Recompilation

The injected executor bytecode must have mainnet immutable values (OWNER_ADDR, WETH_ADDR, POOL_MANAGER_ADDR) baked in. **Do NOT regenerate the bytecode with `ape compile` alone** — that produces naked runtime code with zeroed immutables, causing `!OWNER` reverts on every execution.

Use the dedicated recompilation script:

```bash
cd ~/code/degenbot && python3 contracts/recompile.py
```

This script:
1. Compiles `cmd_executor.vy` from `~/code/executor/` via Vyper
2. Appends 9 x 32-byte immutable slots after the CBOR metadata
3. Patches the POOL_MANAGER_ADDR slot to the mainnet address
4. Writes `contracts/cmd_executor_runtime_bytecode.txt` (and ABI, init bytecode)

Pass `--no-patch` to skip the PM patch (e.g. for testnets).

The CBOR metadata is preserved in the compiled output. Vyper's CODECOPY
offsets assume deployed layout `[code][CBOR][immutables]`; the CBOR bytes
also serve as the function dispatch jump table and JUMPDEST targets.
Stripping the CBOR breaks immutable reads, the jump table, and JUMPDEST
targets.

Verify after recompilation — the script prints the tail slots:

```
Verification:
  Code + CBOR:  16476 bytes
  Immutables:   288 bytes
  Total:        16764 bytes
  OWNER slot:   0x9c56a29c7231974c269e24f9fb3c29203039089e
  ...
```

If `OWNER slot` shows anything other than `0x9c56a29c...`, the bot will fail with `!OWNER` on every simulated path.

### Encoding pipeline

The `encode_cmd_stream()` function in `eth_backrun_helpers.py` dispatches to type-specific encoders based on the hop types. The V2 N-hop encoder uses **Approach 2** (V2_SWAP_COMPACT flash + chained V2_SWAP_CALC) — pool A optimistically sends the forward token to the executor, then the callback transfers it to pool B (creating excess balance), and each subsequent V2_SWAP_CALC reads excess balance, computes output, and sends directly to the next pool.

Key invariants enforced by the encoder:
1. **Address table indices** must be built from the correct iteration variable (see Address table index hygiene above)
2. **zfo flags** must match the deployed pair's actual `token0`/`token1` ordering, not hardcoded assumptions — fake contract addresses in tests may have different ordering than mainnet
3. **Forward token** is the output of pool A (token1 if zfo, token0 otherwise) — this is the intermediate token sent to pool B
4. **WETH repayment** is the input token to pool A — the executor receives WETH from the last pool and transfers it back to pool A in the callback

### Ad-hoc mainnet-reserve testing

When a V2-V2-V2 (or any multi-hop) path fails in `eth_simulateV1` but passes in the Ape test suite with fake contracts, create an ad-hoc test using the **exact mainnet reserves** (fetched via `getReserves()`). The fake contracts in the Ape suite may have different `token0`/`token1` orderings because the auto-generated Foundry addresses differ from mainnet — always compute `zfo` dynamically from the deployed pair's `token0()`.

To create such a test:
1. Fetch `getReserves()` and `token0()`/`token1()` for each pool from the RPC
2. Deploy fake ERC-20 tokens, fake V2 pairs, and fund pools with the exact mainnet reserve amounts
3. Compute `zfo` dynamically: `zfo = (pool.token0() == selling_token_address)`
4. Use the same encoding as the production path (e.g., Approach 2 for V2-V2-V2)

V3 and V4 use **opposite** sign conventions for `amountSpecified` (V3 exact-input = positive, V4 exact-input = negative). See [`contracts/README.md` § "V3 vs V4 amountSpecified Sign Convention"](../contracts/README.md).
