# CoW Protocol Rust Survey — Auction Mechanics, Solvers, and Chain Interaction

> Study of how [cowprotocol](https://github.com/cowprotocol) builds its off-chain solver stack,
> as design input for degenbot's backrun resolver, submission pipeline, and any future
> auction participation. Investigated against `cowprotocol/services` at commit
> `d15021823c75b98f13cd5e885ba96d89d6d55f35` (2026-09-17), shallow clone of `main`.
> Clone parked at `/tmp/cow-survey/services`.

## Executive summary

CoW's `services` monorepo is where the protocol's solver-side engineering lives: a 40+ crate
Rust workspace covering driver/solver engines, the auction arbitration algorithm, chain
interaction, simulation, gas, and price estimation. Four areas are worth mining for design
ideas in degenbot; the verdict table first, findings follow.

| Area | Verdict: relevance with examples |
| --- | --- |
| Solver auction participant | **High.** The production winner-selection algorithm is extracted into a tiny standalone crate (`winner-selection`) with the exact score math (CIP-38), fairness filters, and winner ranking. Wire format (`solvers-dto`) and the driver/solver-engine split (`driver`) give a complete map of what an auction participant must provide. |
| Routing technique | **Medium.** The old arbitrage graph solver is gone; the current `solvers` engine is a baseline bounded-path router over typed AMM states plus an external-DEX engine. The transferable pieces are the `BaselineSolvable` trait (surplus-over-baseline routing metric) and the enum-keyed liquidity domain model. |
| Chain interaction | **High.** `ethrpc`'s WebSocket block stream, `simulator`'s uniform eth_call/Tenderly abstraction with background **state-override streaming for live off-chain-priced venues** (propAMMs), and the instrumented gas estimator are all patterns worth studying against our frame pipeline and fork-fixture simulation gate. |
| Calculation method | **High.** Protocol fee policies (surplus / price-improvement / volume), uniform clearing-price arithmetic, slippage and minimum-surplus domain types, and a staged competitor-racing price estimator. |

## 1. The auction arbitration algorithm (`crates/winner-selection`)

The production winner-selection logic is deliberately extracted from the rest of the stack as
"minimal data structures… sent to the Pod Service for storage and later retrieval", written
once and generic over a `ChainTypes` vocabulary (EVM and Solana instantiations).

### 1.1 Inputs and shape

Two type sets feed the arbitrator:

- **Solutions** (`solution.rs`): `Solution { id: u64, solver: AccountId, orders: Vec<Order>, … }`.
  Each executed `Order` carries the *limit* amounts (`sell_amount`, `buy_amount` = the limit
  price), the *executed* amounts (`executed_sell` includes fees, `executed_buy` is after fees),
  and a `side: Buy | Sell`.
- **`AuctionContext`** (`auction.rs`): per-order `FeePolicy` lists, the set of addresses whose
  JIT orders are allowed to contribute to score (`surplus_capturing_jit_order_owners`), and the
  native-token price of every token in the auction.

An order contributes to its solution's score only if it is a user order from the auction (it
has fee policies) **or** it is a JIT order minted by an allow-listed address — the anti-JIT
spam gate.

### 1.2 Score math (CIP-38, `compute_order_score`)

Per order, in native token:

1. Compute the *custom clearing prices* of the order from its executed amounts
   (`calculate_custom_prices_from_executed`).
2. `surplus_in_surplus_token = user_surplus_over_limit_price + protocol_fees`. The protocol
   fees are *added back* into the score — a solver turns fee revenue into ranking weight.
3. Convert to native: a sell order's surplus already lives in the buy token, so
   `score_native = native_price[buy_token] * surplus`. A buy order's surplus lives in the sell
   token and needs a widening integer conversion into buy tokens first
   (`surplus * buy_amount / sell_amount`, floor, multiply-before-divide to avoid precision loss).

Protocol fees themselves (`FeePolicy` enum in `primitives.rs`):
`Surplus { factor, max_volume_factor }`, `PriceImprovement { factor, max_volume_factor, quote }`,
or `Volume { factor }`. Surplus and price-improvement fees are always capped by a max volume
fraction; multiple policies are applied in reverse order with prices updated iteratively.

### 1.3 Fairness filter, winner picking, reference scores

The sequence in `Arbitrator::arbitrate`:

1. **Score** every solution per directed token pair; any solution whose per-pair aggregate
   cannot be computed is discarded outright.
2. **Filter unfair solutions**: every order execution must beat the *baseline* reference
   solution's score for its token pair (except single-pair solutions, which can't be
   comparison-scaled — see the cited comb-auction fairness note at
   `github.com/fhenneke/comb_auctions/issues/2`). This is the anti-pennying mechanism: a
   solution that sacrifices one order to juice surplus on another doesn't survive.
3. **Pick winners in score order** (`pick_winners`): iterate solutions best-first and accept a
   solution only if none of its `(sell_token, buy_token)` directed pairs were already covered by
   a previously chosen winner. Up to `max_winners`. This enforces *uniform directional clearing
   prices* — two solutions cannot both win the same token pair's direction.
4. **Reference scores** (`compute_reference_scores`): for each winning solver, re-run the
   winner-picking *without that solver* and sum the resulting winner scores. This is the
   counterfactual "what would the auction have earned without you" — the basis for solver
   reward/penalty computation (including the auction's penalty terms; see
   `penalty_cap_native` on the wire order below).

### 1.4 A type-state machine for scoring

`state.rs` models the solution lifecycle as a compile-time state machine:

```text
Unscored —with_score()→ Scored<Score> —with_rank()→ Ranked<Score>
```

via a `HasState` trait (`type Next<NewState>`, `type State`) with `UnscoredItem` / `ScoredItem`
extension traits gating each transition. This is a clean instance of the state-machine
preference we already hold in degenbot (cf. block-state-machine ADR) — worth mirroring in our
own solution/scoring code if we ever rank settlement candidates.

## 2. Participating as a solver (`crates/driver`, `crates/solvers-dto`)

### 2.1 The driver/solver-engine split

The `driver` README formalizes the decomposition: every solver needs liquidity collection,
solution selection, settlement encoding, and publishing — only *solution generation* is
solver-specific. The **driver** wraps a **solver engine** and handles quoting, ranking,
encoding, competition reporting, and submission. The protocol additionally supports *full
solvers* that re-implement the driver-side duties themselves.

For us the frame transfers: whatever contest we enter, separate the *engine* (path/strategy
search) from the *harness* (quoting, deadline plumbing, encoding, telemetry, submission) so
multiple engines can be hung on one harness — our backrun sidecar already has this shape; the
driver naming is just a cleaner articulation of it.

### 2.2 The wire contract

`solvers-dto/src/auction.rs` is what a participant actually receives per auction:

- `Auction { id, tokens, orders, liquidity, effective_gas_price, deadline,
  surplus_capturing_jit_order_owners }` — deadline is an absolute UTC timestamp.
- Orders carry `full_sell_amount` / `full_buy_amount` (pre-partial-fill caps), pre/post
  interactions, `sell_token_source`, `partially_fillable`, and — notably —
  `penalty_cap_native`: a solver-side cap on the penalty incurred for winning an order and
  *failing* to execute it. Winning is a liability, not a license.

Solutions flow back through `solvers-dto/src/solution.rs` DTOs (settlement interactions are
just `(target, value, call_data)` triples — cf. `shared/src/interaction.rs`, the `Interaction`
trait).

### 2.3 Deadline handling inside an engine

The baseline engine (`crates/solvers/src/domain/solver/baseline.rs`) shows the deadline
pattern we already use: solve on a spawned task, drain solutions through an unbounded channel,
stop the drain at `deadline - DEADLINE_SLACK` (500 ms) and `handle.abort()` the search.
Partial-fill retries are capped (`max_partial_attempts`, amount repeatedly halved) so
degenerate orders can't consume the deadline.

## 3. Routing technique (`crates/solvers`, `crates/liquidity-sources`)

### 3.1 What replaced the graph arbitrageur

The famous Bellman-Ford negative-cycle solver is no longer in the tree; `crates/solver` now
holds only AMM adapters and ordering-by-surplus. Route finding lives in the rewritten
`crates/solvers` engine:

- **Baseline engine** (`domain/solver/baseline.rs`, 412 lines): finds the best path of at most
  `max_hops + 1` venues over the collected liquidity, *no order splitting* across paths.
  Paths are ranked by surplus net of gas (`route.gas() + solution_gas_offset`). Buy orders
  that overshoot by rounding are capped back to the buy amount.
- **DEX engine** (`domain/solver/dex/`): routes through external aggregator APIs, then applies
  slippage/minimum-surplus machinery and a fill-search loop for partially fillable orders
  (`fills.rs` maps each order to its next fill attempt; sell-orders count in sell terms,
  buy-orders in buy terms).
- The legacy `crates/solver/src/solver.rs` retains the ordering abstraction wrapped around
  both AMM families.

### 3.2 The transferable abstractions

- **`BaselineSolvable`** (`liquidity-sources/src/baseline_solvable.rs`, 29 lines): three
  methods — `get_amount_out`, `get_amount_in` (explicitly *not* required to be symmetric;
  the inverse query may return `None`), and `gas_cost`. Any venue, including a bespoke
  backrun venue, implements this and plugs into route planning and "does this beat the
  alternative" checks. This is the smallest viable interface for degenbot's resolver to
  compare candidate victim/viable-route combinations.
- **Enum-keyed liquidity state** (`solvers/src/domain/liquidity/mod.rs`):
  `Liquidity { id, address, gas: Gas, state: State }` with
  `State ∈ { ConstantProduct, WeightedProduct, Stable, Concentrated, LimitOrder }` — the same
  AMM family we cover, with gas carried as a first-class field of liquidity, not a bolt-on.
  Their `TokenPair` constructor canonically orders the pair and returns `None` for self-pairs;
  `Reserves` enforces the `uint112` Uniswap-V2 invariant at construction (0..2¹¹²−1) plus
  canonical ordering — construction-time invariants instead of scattered assertions, matching
  our io-free-pools philosophy.
- **Block-aware pool caching** (`liquidity-sources/src/recent_block_cache.rs`): freshness
  metadata per fetch, so a solver pays for a re-fetch only when the consuming route is
  stale relative to the current block. Relevant to our hot-path cache work.

## 4. Chain interaction (`crates/ethrpc`, `crates/simulator`, `crates/gas-price-estimation`)

### 4.1 Block stream with latency capture

`ethrpc/src/block_stream.rs` (451 lines) is a complete, small pattern:

- `BlockInfo { number, hash, parent_hash, timestamp, gas_limit, gas_price, base_fee,
  observed_at: Instant }` — **`observed_at` is stamped at conversion time**, so every consumer
  can compute its own staleness versus the observation moment. Our telemetry-latency playbook
  cares about exactly this quantity (pump latency = observe → act).
- Subscription-first initialization: `subscribe_blocks()` is awaited *before* an HTTP fetch of
  the latest block, deliberately closing the race window between WS subscription and the
  initial state. The initial state arrives via HTTP (no waiting for the next block), updates
  flow over WS, alloy's `WsConnect` owns reconnection, and `handle_new_block` tolerates gaps
  or out-of-order headers by only advancing on newer blocks.
- Broadcast via `tokio::sync::watch` — one subscription, cloneable watchers shared by all
  consumers. The polling variant is deprecated with the rationale inlined in the deprecation
  note.

### 4.2 Uniform simulator + live state overrides

`crates/simulator` presents one `Simulator` interface over two backends: node `eth_call`
(`ethereum/`) and Tenderly (`tenderly/`), with optional access-list and gas-estimation
short-circuits. Two details matter to us:

- **Realistic simulation gas price**: before simulating, they estimate the current gas price
  rather than trusting node defaults, noting that "some sneaky tokens have special code paths
  that detect that case to try to behave differently during simulations" — an anti-
  simulation-fraud measure directly applicable to our fork-fixture oracle harness.
- **`state_override_stream.rs`** is the most novel thing in the repo: a background WebSocket
  stream of `eth_call`-style `StateOverride`s for venues whose price lives in maker memory
  rather than committed chain state (Titan-style propAMMs). Their vocabulary: a **frame** is
  one venue's overrides for the block being built; a **lane** is one storage slot of the
  shared registry holding one venue's quote; each lane contains a **stamp** — bytes encoding
  the timestamp of the block the quote is for, which the venue enforces with
  `StaleUpdate()` / `FeedStalled()` reverts. Crucially the stamp *value* is known before
  reading (the frame names the block; the timestamp is projected from chain time), so only
  the stamp's *location* is searched — no guessing layouts from value content. For degenbot
  this is a preview of where quoting is going: chain state is increasingly stale by half a
  block for internalized venues, and the fix is streaming overrides, not re-fetching.

### 4.3 Gas estimation

`gas-price-estimation` exposes a `GasPriceEstimating` trait (`estimate() -> Eip1559Estimation`,
`base_fee()`, `effective_gas_price()`) with multiple implementations and a
`DriverGasEstimator` that fetches gas from the driver itself when running inside the cluster.
The instrumented wrapper (`gas_price.rs`) publishes base fee / effective price to Prometheus
via the `prometheus-metric-storage` derive macro — a pattern to steal for our submission
crate: one tiny derive (`#[derive(MetricStorage)]` on a struct of gauges) plus a registry call.

## 5. Calculation methods worth lifting

- **Score arithmetic shapes**: the CIP-38 computation leans on `saturating_add`,
  `try_widening_mul_div_floor` (widening products with floor), and explicit side-dependent
  price conversion, with the algebra derived step by step in comments. Money-math hygiene to
  mirror: widen before dividing, saturate rather than panic, convert once at the boundary.
- **Fee policies as data**: the three-axis `FeePolicy` enum (surplus / price-improvement /
  volume, each with caps) is a compact encoding of "this venue's take" that any revenue-
  sharing contest needs once it has multiple participants.
- **Slippage and minimum-surplus domain types** (`solvers/src/domain/dex/`): `Slippage` and
  `MinimumSurplus` are newtypes over `BigDecimal` combining a relative bound with an optional
  *absolute* bound (denominated in native, converted to relative at the amount's price). The
  effective bound is `min(relative, absolute_as_relative)` for slippage and
  `max(relative, absolute_as_relative)` for minimum surplus — correct composition of both
  bounds on both sides of the trade.
- **Competition price estimation** (`price-estimation/src/competition/mod.rs`, 884 lines):
  `CompetitionEstimator` runs *stages* (outer list = sequential, inner list = parallel) of
  estimators, supports early return after N successful results, and ranks verified estimates
  above unverified ones when configured — a staged race-and-verify pattern applicable any
  time we have cheap/noisy estimators plus a trusted slow one (e.g. our on-chain quoter
  versus fork simulation).
- **Revert risk pricing**: outside the monorepo, `risk_adjusted_rewards` (Python) regresses
  settlement revert probability into expected-reward terms — the same expected-value
  adjustment our backrun resolver applies to revert risk, viewed from the protocol side.

## 6. Applicability to degenbot

Concrete takeaways, ranked:

1. **Steal the `BaselineSolvable` trait shape** for resolver route evaluation (3 methods,
   asymmetric inverse, gas included). Small enough to adopt without importing anything.
2. **Type-state scoring** (Unscored → Scored → Ranked via a `HasState` trait) for any ranking
   of candidate settlements/backruns; matches our FSM predilection.
3. **`BlockInfo.observed_at` + watch-channel single-subscription block stream** as a reference
   implementation for the pump's block clock and for notification latencies in the frame
   pipeline.
4. **Simulation gas-price realism + stamp-located live overrides** as defenses/simplifications
   for the fork-fixture oracle and any future internalized venues.
5. **`penalty_cap_native` as a concept**: in any contest we join, win-backable liabilities
   must be priced with a cap — build it into quote/profit evaluation proactively.
6. **`prometheus_metric_storage`** derive for metric registration without boilerplate.

Not worth importing: the engines themselves (tightly coupled to the CoW order model),
`ethcontract-rs` (pre-alloy binding generation), `arak` (we have our own indexer).

## 7. Licensing and dependency notes

`services` is alloy-2.4-based (degenbot targets alloy 1.6) and leans on `bigdecimal`,
`chrono`, and a large internal type vocabulary (`chain-types`). Copying code wholesale would
drag these in; the recommended mode is transcribing designs into our stack, which this
document inventories. Audit individual crate licenses (`[package] license` in each
`Cargo.toml`) before any verbatim vendoring — several CoW crates historically carry copyleft
terms.

---

*Sources: all file paths relative to `cowprotocol/services` @
`d15021823c75b98f13cd5e885ba96d89d6d55f35`: `winner-selection/{arbitrator,solution,auction,state}.rs`,
`solvers-dto/src/auction.rs`, `solvers/src/domain/{solver/baseline,solver/dex/*,liquidity/*}.rs`,
`liquidity-sources/src/baseline_solvable.rs`, `ethrpc/src/block_stream.rs`,
`simulator/src/{ethereum/mod,state_override_stream}.rs`,
`gas-price-estimation/src/{gas_price,driver}.rs`, `price-estimation/src/competition/mod.rs`,
`shared/src/interaction.rs`; driver README (`crates/driver/README.md`); plus the standalone
repos `gas-estimation`, `ethcontract-rs`, `arak`.*
