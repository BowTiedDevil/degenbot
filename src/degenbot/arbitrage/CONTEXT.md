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

## Engine Orchestration

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Engine Registry** | `degenbot.arbitrage.EngineRegistry` — the **one canonical way to start** a `UniswapArbEngine` operator. Runs the pre-pump startup ritual (`subscribe` → stream snapshots → `backfill_from_snapshot` → verify config) and **stops before `resume()`**, so the caller attaches its result consumer before any batches flow. Maintains the Python pool ↔ Rust `pool_id` key maps + registers paths. Construct with `bot=` (production, builds the engine against the bot's shared `BotState`) or `engine=` (testability seam). | Engine orchestrator, engine wrapper |
| **Engine-Facing Hop Descriptor** | The `V2HopInfo` / `V3HopInfo` / `V4HopInfo` / `HopInfo` / `PathInfo` / `build_hops_from_pools` family in `degenbot.arbitrage.hop_info` — distinct from the *solver's* `HopType`/`BoundedProductHop` shape. Frozen dataclasses reading only pool attributes, used by `EngineRegistry.register_path` to hand hops to the Rust engine and read back by deployment-side `encode_cmd_stream`. | Hop info, hop struct |
| **Pool Admission** | The Rust core's correctness floor: `BotState::register_v4_pool` refuses amount-modifying-hook pools (`hook_flags & 0xCC != 0`) and dynamic-fee pools (`fee == 0x100000`), surfacing as typed `HookedPoolRejectedError` / `DynamicFeePoolRejectedError` (both subclass `ValueError`). The solver's V3-CL math assumes no hook intervention + a fixed fee; admission is enforced in Rust (ADR-005 standalone-core), not as a Python pre-check. | V4 hook filter |

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
| **Pool Cache Adapter** *(removed)* | Formerly a subscriber that auto-registered pools in the Rust `RustPoolCache` mirror on state updates; deleted in ADR-003 Slice 4 (Option D — delete, not migrate). The legacy mirror was a second Rust backend alongside the production `Bot`/`UniswapArbEngine` path | ArbPoolCacheAdapter, cache adapter |
| **SwapEncoder** | The swap encoding layer: each `SwapAmounts` subclass self-encodes into an `EncodedCall`; the pipeline function `generate_payloads()` wires encoding → approval → composition | Calldata builder, payload encoder |
| **EncodedCall** | A minimal EVM call fragment (`to`, `data`, `value`) ready for on-chain submission; produced by `SwapAmounts.encode()` | Payload, call tuple |
| **ApprovalStrategy** | A pluggable protocol that injects ERC-20 approval calls before swap calls | Approval injection |
| **PayloadComposer** | A pluggable protocol that composes a list of `EncodedCall`s into the format a target contract expects | Call composition, multicall wrapper |
| **V4PoolKey** | See [V4PoolKey](../types/CONTEXT.md) in the types context; used by custom **PayloadComposers** for V4 dispatch | Pool key, V4 key |
| **Dynamic Amount** | A V4 swap where `amountSpecified=0` and `dynamic_amount=True`; the contract derives the actual amount from the on-chain delta ledger instead of using a pre-computed value. Used for the second swap in V4-V4 paths where the intermediate delta must cancel exactly | Auto-amount, derived amount |

## Relationships

- An **Arbitrage Cycle** (deprecated) was an ordered sequence of **Pools** that form a closed token loop; replaced by **Arbitrage Path**
- An **Arbitrage Path** wraps a sequence of pools with a **Solver** and subscribes to **Pool State Messages**
- A **Swap Vector** describes the direction of a single hop within an **Arbitrage Path**
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver** (implemented by each pool's `to_hop_state()` method)
- A **Pool Cache Adapter** (removed in ADR-003 Slice 4) formerly subscribed to **Pool State Messages** and auto-registered both orientations in the legacy `RustPoolCache` mirror; that path is deleted
- **Swap Amounts** self-encode into **EncodedCall**s; `generate_payloads()` wires encoding → **ApprovalStrategy** → **PayloadComposer**
- **Swap Amounts** provide `input_amount()` / `output_amount()` for generic amount extraction; pool classes implement `build_swap_amount()` from the `ArbitragePathPool` protocol
- A **V4PoolKey** is available to custom **PayloadComposers** for V4's unlock/swap callback dispatch
- The **Engine Registry** is the single canonical startup orchestrator: it sequences `subscribe` → snapshot stream → `backfill_from_snapshot` → verify config, halting before `resume()` so the consumer attaches safely. **Pool Admission** (hooked/dynamic-fee V4 refusal) is enforced in the Rust core, not as a Python pre-check; rejections surface as `HookedPoolRejectedError` / `DynamicFeePoolRejectedError` for type-safe classification
- **Address table index hygiene**: `AddressTable` deduplicates by checksummed address — see **Encoding footguns** below for the V2 N-hop iteration-variable bug

## Encoding footguns

- **int128 overflow guard** — `fits_int128()` (`degenbot.arbitrage.encoding`) skips V4 paths where `amountSpecified` exceeds ±2^127, preventing V4 `SafeCastOverflow` reverts. Checked by all 5 V4 encoder functions. See: `tests/arbitrage/test_int128_range.py`.
- **V4→V2 amount_out** — V2 `swap(amount0Out, amount1Out, ...)` names what V2 SENDS; for USDC→WETH@V2 the `amount_out` is `weth_out`, not `forward_out`. See: `TestV4ToV2WrongAmountOut`.
- **V2 N-hop encoding** — `_encode_cmd_v2_n_hop` (`eth_backrun_helpers.py`) uses V2_SWAP_COMPACT flash + chained V2_SWAP_CALC. Fixed bug: a list comprehension `for h in hops` referenced `hop.pool_address` from outer scope instead of `h.pool_address`, so all pools resolved to the last hop's deduplicated index → the executor called the wrong pool for every V2_SWAP_CALC; symptom was 100% V2-V2-V2 simulation failure with `V2_SWAP_CALC: no excess balance`.

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
