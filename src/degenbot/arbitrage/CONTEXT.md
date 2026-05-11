# Context — Arbitrage, Solvers & Adapters

## Arbitrage

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Arbitrage Cycle** | A sequence of pools forming a closed loop where an input token is swapped through intermediate tokens and back to the original | Arb cycle, circular path, cycle |
| **Arbitrage Path** | An event-driven wrapper around a sequence of pools that validates token flow, subscribes to state updates, and delegates solving | Arb path |
| **Input Token** | The token supplied to the first swap in an arbitrage cycle | Starting token |
| **Profit Token** | The token in which arbitrage profit is measured (always equals the Input Token for a cycle) | Output token |
| **Input Amount** | The quantity of Input Token to be swapped into the first pool | Swap amount, trade size |
| **Profit Amount** | The net token gain after completing all swaps in the cycle (negative = unprofitable) | PnL, gain |
| **Rate of Exchange** | The ratio of output to input across the entire cycle; values > 1 indicate a profitable opportunity | Exchange rate, arb rate |
| **Swap Vector** | A directed pair (token_in, token_out) plus a zero_for_one flag describing the direction of a single swap within a path | Swap direction, flow |
| **Swap Amounts** | The per-pool input/output amounts and parameters needed to execute the swaps in an arbitrage cycle | Swap details |
| **Calculation Result** | The complete output of an arbitrage calculation: input amount, profit amount, per-pool swap amounts, and state block | Arb result |

## Solvers & Optimizers

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Solver** | An algorithm that finds the optimal **Input Amount** for a single arbitrage path given **Hop States**; operates on one path at a time | Optimizer, finder |
| **Optimizer** | A higher-level routine that coordinates **Solvers** across multiple paths or state configurations; finds the best path/strategy across alternatives | Solver |
| **Hop State** | The numerical representation of a single pool's state in a form suitable for solver consumption (e.g., virtual reserves) | Pool state (solver context), solver state |
| **Mobius Solver** | A solver using the Möbius transformation approach for constant-product and concentrated-liquidity pools | — |

## Pool Adapters

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Adapter** | A protocol object that translates a specific pool type into solver-compatible Hop State, extracts fees, and builds Swap Amounts | Adapter, bridge |
| **Pool Cache Adapter** | A subscriber that auto-registers pools in the Rust solver cache on state updates, eliminating manual cache management; uses **CacheablePool** protocol methods instead of `getattr` introspection (Plan 019) | ArbPoolCacheAdapter, cache adapter |
| **SwapEncoder** | The swap encoding layer: each `SwapAmounts` subclass has an `encode()` method that produces an `EncodedCall` for its pool type. The pipeline function `generate_payloads()` wires encoding → approval → composition | Calldata builder, payload encoder |
| **EncodedCall** | A minimal EVM call fragment (`to`, `data`, `value`) ready for on-chain submission; produced by `SwapAmounts.encode()` | Payload, call tuple |
| **ApprovalStrategy** | A pluggable protocol that injects ERC-20 approval calls before swap calls; default `NoApprovals` adds none; library callers implement custom strategies (e.g. `ExactApproval`, `UnlimitedApproval`, `Permit2Approval`) | Approval injection |
| **PayloadComposer** | A pluggable protocol that composes a list of `EncodedCall`s into the format a target contract expects; default `FlatComposer` returns the list as-is; library callers implement custom composers (e.g. `Multicall3Composer`, custom executor wrappers) | Call composition, multicall wrapper |
| **V4PoolKey** | A frozen dataclass carrying the V4 pool identification struct (`currency0`, `currency1`, `fee`, `tick_spacing`, `hooks`); stored on `UniswapV4PoolSwapAmounts` and used by custom PayloadComposers for V4 dispatch | Pool key, V4 key |

## Relationships

- An **Arbitrage Cycle** contains an ordered sequence of **Pools** that form a closed token loop
- An **Arbitrage Path** wraps a sequence of pools with a **Solver** and subscribes to **Pool State Messages**
- A **Swap Vector** describes the direction of a single hop within an **Arbitrage Path**
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver**
- A **Pool Cache Adapter** subscribes to **Pool State Messages** and auto-registers forward and reverse orientations in the Rust pool cache; uses **CacheablePool** protocol
- **Swap Amounts** carry per-pool swap parameters and know how to `encode()` themselves into **EncodedCall**s
- `generate_payloads()` wires **Swap Amounts** → per-hop `encode()` → **ApprovalStrategy** injection → **PayloadComposer** composition → final `list[EncodedCall]`
- A **V4PoolKey** lives on `UniswapV4PoolSwapAmounts` and is available to custom **PayloadComposers** for V4's unlock/swap callback dispatch

## Resolved ambiguities

### Solver vs Optimizer

**Ruling: **Solver** = single-path input optimization. **Optimizer** = multi-path coordination. Never substitute.**

The codebase enforces this hierarchy: `Solver` / `SolverProtocol` receives a sequence of **Hop States** and returns a result for one path. `ArbitrageOptimizer` coordinates across multiple paths, calling Solvers internally. An Optimizer decides *which* path is best; a Solver decides *how much* to input on a given path.

- ✅ "The **Mobius Solver** found an optimal **Input Amount** of 5 ETH for this path"
- ✅ "The **Optimizer** compared 12 paths and selected the best one"
- ❌ "The solver compared 12 paths" (that's an **Optimizer**)
- ❌ "The optimizer found the input amount" (that's a **Solver**)
