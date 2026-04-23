# Ubiquitous Language — Arbitrage, Solvers & Adapters

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
| **Pool Compatibility** | An enum indicating whether a pool can participate in an arbitrage path (COMPATIBLE, INCOMPATIBLE_INVARIANT, INCOMPATIBLE_TOKENS) | — |

## Relationships

- An **Arbitrage Cycle** contains an ordered sequence of **Pools** that form a closed token loop
- An **Arbitrage Path** wraps a sequence of pools with a **Solver** and subscribes to **Pool State Messages**
- A **Swap Vector** describes the direction of a single hop within an **Arbitrage Path**
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver**

## Resolved ambiguities

### Solver vs Optimizer

**Ruling: **Solver** = single-path input optimization. **Optimizer** = multi-path coordination. Never substitute.**

The codebase enforces this hierarchy: `Solver` / `SolverProtocol` receives a sequence of **Hop States** and returns a result for one path. `ArbitrageOptimizer` coordinates across multiple paths, calling Solvers internally. An Optimizer decides *which* path is best; a Solver decides *how much* to input on a given path.

- ✅ "The **Mobius Solver** found an optimal **Input Amount** of 5 ETH for this path"
- ✅ "The **Optimizer** compared 12 paths and selected the best one"
- ❌ "The solver compared 12 paths" (that's an **Optimizer**)
- ❌ "The optimizer found the input amount" (that's a **Solver**)
