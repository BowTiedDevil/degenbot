# Context — Arbitrage, Solvers & Adapters

## Arbitrage

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Arbitrage Cycle** | ~~Deprecated.~~ An ordered sequence of pools forming a closed loop. Use **Arbitrage Path** instead. Legacy implementations live in `_legacy/` with `DeprecationWarning`. | Arb cycle, circular path, cycle |
| **Arbitrage Path** | An event-driven wrapper around a sequence of pools that validates token flow, subscribes to state updates, and delegates solving; the replacement for the deprecated **Arbitrage Cycle** classes | Arb path, path |
| **Input Token** | The token supplied to the first swap in an arbitrage cycle | Starting token |
| **Profit Token** | The token in which arbitrage profit is measured (always equals the Input Token for a cycle) | Output token |
| **Input Amount** | The quantity of Input Token to be swapped into the first pool | Swap amount, trade size |
| **Profit Amount** | The net token gain after completing all swaps in the cycle (negative = unprofitable) | PnL, gain |
| **Rate of Exchange** | The ratio of output to input across the entire cycle; values > 1 indicate a profitable opportunity | Exchange rate, arb rate |
| **Swap Vector** | A directed pair (token_in, token_out) plus a zero_for_one flag describing the direction of a single swap within a path | Swap direction, flow |
| **Swap Amounts** | The per-pool input/output amounts and parameters needed to execute the swaps in an arbitrage cycle. `input_amount()` / `output_amount()` provide generic extraction; `build_swap_amount()` on pool classes replaces isinstance-chain factory. | Swap details |
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
| **Pool Adapter** | A protocol object that translates a specific pool type into solver-compatible Hop State; pool `to_hop_state()` is the single source of truth; N-token pools accept `token_in`/`token_out` kwargs for pair selection | Adapter, bridge |
| **Pool Cache Adapter** | A subscriber that auto-registers pools in the Rust solver cache on state updates; uses **CacheablePool** protocol methods | ArbPoolCacheAdapter, cache adapter |
| **SwapEncoder** | The swap encoding layer: each `SwapAmounts` subclass self-encodes into an `EncodedCall`; the pipeline function `generate_payloads()` wires encoding → approval → composition | Calldata builder, payload encoder |
| **EncodedCall** | A minimal EVM call fragment (`to`, `data`, `value`) ready for on-chain submission; produced by `SwapAmounts.encode()` | Payload, call tuple |
| **ApprovalStrategy** | A pluggable protocol that injects ERC-20 approval calls before swap calls | Approval injection |
| **PayloadComposer** | A pluggable protocol that composes a list of `EncodedCall`s into the format a target contract expects | Call composition, multicall wrapper |
| **V4PoolKey** | See [V4PoolKey](../types/CONTEXT.md) in the types context; used by custom **PayloadComposers** for V4 dispatch | Pool key, V4 key |
| **Dynamic Amount** | A V4 swap where `amountSpecified=0` and `dynamic_amount=True`; the contract derives the actual amount from `t_v4_deltas` instead of using a pre-computed value. Used for the second swap in V4-V4 paths where the intermediate delta must cancel exactly | Auto-amount, derived amount |
| **V4 Delta Ledger** | `t_v4_deltas: transient(HashMap[address, int128])` in the tstore executor — tracks ALL currency deltas (not just ETH/WETH) across V4 swaps. Enables correct settlement of intermediate ERC-20 tokens | Delta map, delta tracker |

## Relationships

- An **Arbitrage Cycle** (deprecated) was an ordered sequence of **Pools** that form a closed token loop; replaced by **Arbitrage Path**
- An **Arbitrage Path** wraps a sequence of pools with a **Solver** and subscribes to **Pool State Messages**
- A **Swap Vector** describes the direction of a single hop within an **Arbitrage Path**
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver** (implemented by each pool's `to_hop_state()` method)
- A **Pool Cache Adapter** subscribes to **Pool State Messages** and auto-registers both orientations in the Rust pool cache
- **Swap Amounts** self-encode into **EncodedCall**s; `generate_payloads()` wires encoding → **ApprovalStrategy** → **PayloadComposer**
- **Swap Amounts** provide `input_amount()` / `output_amount()` for generic amount extraction; pool classes implement `build_swap_amount()` from the `ArbitragePathPool` protocol
- A **V4PoolKey** is available to custom **PayloadComposers** for V4's unlock/swap callback dispatch
- A **Dynamic Amount** is a V4 swap where the contract derives `amountSpecified` from the **V4 Delta Ledger** instead of a pre-computed value; ensures intermediate deltas cancel exactly in V4-V4 paths
- The **V4 Delta Ledger** (`t_v4_deltas`) tracks all currency deltas across V4 swaps; replaces the former two-accumulator pattern (`ether_delta`/`weth_delta`) to properly handle intermediate ERC-20 tokens
- **int128 overflow guard** (`fits_int128()`) prevents V4 `SafeCastOverflow` reverts by skipping paths where `amountSpecified` exceeds ±2^127; checked by all 5 V4 encoder functions

## Resolved ambiguities

### Solver vs Optimizer

**Ruling: **Solver** = single-path input optimization. **Optimizer** = multi-path coordination. Never substitute.**

The codebase enforces this hierarchy: `Solver` / `SolverProtocol` receives a sequence of **Hop States** and returns a result for one path. An **Optimizer** coordinates across multiple paths, calling Solvers internally. An Optimizer decides *which* path is best; a Solver decides *how much* to input on a given path.

- ✅ "The **Mobius Solver** found an optimal **Input Amount** of 5 ETH for this path"
- ✅ "The **Optimizer** compared 12 paths and selected the best one"
- ❌ "The solver compared 12 paths" (that's an **Optimizer**)
- ❌ "The optimizer found the input amount" (that's a **Solver**)

## Example dialogue

> **Dev:** "The **Solver** compared 12 paths and picked the best one."
> **Domain expert:** "That's the **Optimizer**, not the **Solver**. A **Solver** finds the optimal **Input Amount** for one path. An **Optimizer** coordinates multiple Solvers across paths and picks the best."
>
> **Dev:** "OK, so the Solver returns a **Calculation Result** — what's in it?"
> **Domain expert:** "The **Input Amount**, **Profit Amount**, and per-pool **Swap Amounts** — the full output for that single path."
>
> **Dev:** "And to actually execute the swaps, I just use the Swap Amounts?"
> **Domain expert:** "Each **Swap Amounts** subclass has an `encode()` method that produces an **EncodedCall**. The pipeline function `generate_payloads()` wires encoding → **ApprovalStrategy** injection → **PayloadComposer** composition. You can plug in custom strategies — for example, a V4 **PayloadComposer** that handles the unlock/swap callback pattern."
