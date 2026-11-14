# Operator Add-Path-At-Any-Time Surface (NWTUM3)

An operator can **add a specific path**, or **trigger a bounded on-demand
discovery sweep**, on a *live, running* bot — without restarting it and without
stalling its update/solve/dispatch. This page documents the three reusable
seams that make that possible, from the innermost (the registration pipeline)
to the wire-facing operator channel.

It complements the verify-lifecycle shared decisions in the NWTUM3 ergo task
(Python driver; the Rust-owned `bot_core/registration_lifecycle.rs` is the
middle/hosted form; **D-B** one shared provider, **D-C** no-config fail-fast,
**D-A** tripwire = verification mismatch).

## 1. The long-lived registration pipeline

`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py::PathRegistrationPipeline` is the
**single** place that turns a path's hop steps into registered, verified,
released pools. Both the built-in discovery source AND an operator add route
through the *same* `_consume` body (build → register + verify → per-path
release → dedup → `register_path`), so an operator-added path is treated
identically to a discovered one.

The pipeline is **long-lived** and owned by the session (`BotRunner`),
not created and dropped inside `build_paths`. That is the key structural
change that makes mid-run adds possible:

- `run()` builds a `ConstructionContext` once and holds it, and creates the
  `PathRegistrationPipeline` on the session (`self._pipeline`).
- `build_paths` reuses the caller-supplied pipeline (falls back to building its
  own only when none is supplied).
- Because the pipeline retains its own `ConstructionContext`, it keeps
  constructing through the Rust `PoolBuilder` even after the main-loop trim
  (`self.bot = None`) drops the Python bot — **adding a path mid-run does not
  resurrect Python construction state**, satisfying the trimmed-state AC.

### Programmatic API

`BotRunner` exposes two coroutines that route into the live pipeline. Both
**never await the pump**, so a mid-run command cannot stall solve/dispatch:

```python
await session.enqueue_path(path_steps, directions=None)   # add ONE path
n = await session.trigger_discovery(bound=None)           # bounded one-shot sweep
```

`enqueue_path` takes the same `path_steps` list the pipeline's discovery uses
(step objects with a `.type` pool-table base class, `.address`, and `.hash` for
V4) plus optional per-hop `directions`. `trigger_discovery` runs a *bounded*
sweep distinct from the unbounded background producer. Both raise
`RuntimeError` loudly if no live pipeline exists (e.g. an injected/fake run).

## 2. The operator command channel (wire)

`src/degenbot/operator/operator_channel.py` hosts a **Unix-domain-socket,
JSON-lines** protocol so an operator can steer the bot from a *separate
process*. The bot runs an `OperatorServer` asyncio task; a client writes one
JSON command line and reads one JSON response line.

Requests / responses (one JSON object per line):

```
{"op": "add_path",
 "steps": [{"family": "V2|V3|V4", "address": "0x..",
            "hash": "0x..(v4 pool_id, v4 only)"}, ...],
 "directions": [true, false]}           # optional; auto-resolved if absent
{"op": "discover", "bound": 5}

{"op": "probe_path",
 "path_id": 42,
 "optimal_input": 1000000000000000,     # optional (wei)
 "capital": "self_fund"}                # optional; production|self_fund|zero_balance

{"ok": true,  "detail": "..."}
{"ok": false, "error": "..."}
```

Key properties:

- **No Python class crosses the wire.** `step_from_wire` maps a `family`
  string (V2/V3/V4) to the matching pool-table base class, the single source of
  the classification `_consume` needs.
- **Wire hygiene.** `wrap_handler` turns any exception into `{"ok": false,
  "error": ...}` so a malformed or failing command never crashes the host, and
  the server guards JSON decode / op dispatch the same way.
- **Self-sufficient `close()`.** It cancels the in-flight `serve_forever` loop
  before `wait_closed()`, so shutdown never blocks on a pending accept loop,
  and unlinks the socket file.

The example bot hosts the server when launched with `--operator-socket <path>`,
routing commands into `session.enqueue_path` / `session.trigger_discovery`.

### Migration: one `capital` field replaces the `zero_balance` boolean

The probe payload carried a `zero_balance` boolean, and the host turned it into
a *pair* of booleans (`self_fund = not zero_balance`) that the FFI then had to
re-validate and that the deployment had to re-derive into two
`Option<Address>` override fragments. One value now names the posture end to
end — operator wire, Python construction boundary, FFI, and deployment:

| Surface | Retired | Now |
| --- | --- | --- |
| Operator wire (`probe_path` payload) | `"zero_balance": false` | `"capital": "production"\|"self_fund"\|"zero_balance"` |
| Python operator verb (`Session.probe_path` / `BotRunner.probe_path`) | `zero_balance: bool = False` | `capital: ProbeCapital = ProbeCapital.SELF_FUND` |
| Python probe construction boundary (`build_probe_executor`) | `self_fund=`, `zero_balance=` | `ExecutorInputs.capital` (one `ProbeCapital`) |
| FFI (`degenbot._ffi.simulation.build_probe_executor_py`) | `self_fund=False, zero_balance=False` | `capital: str \| None = None` |
| FFI (`degenbot._ffi.simulation.build_batch_executor_py`, the SUBMITTING lane) | `self_fund=False, zero_balance=False` | no capital parameter at all: its lane simulates the session account's real balances, and a probe posture cannot be smuggled into a lane that can submit |
| Session probe bookkeeping | `Session.probe_zero_balance` | `Session.probe_executors` (a dict: ONE executor per `ProbeCapital`) |

The modes are `production` (the account's real balances — no capital override at
all), `self_fund` (the known asset set installed at the executor, with the
self-funded stream encoding), and `zero_balance` (that set stripped from the
executor, with the in-path-flash encoding). They are variants of ONE enum, so
"self-fund AND zero-balance" is unrepresentable instead of being validated at
runtime.

There are no aliases: a payload that still sends `"self_fund"` / `"zero_balance"`
instead of `"capital"` is refused by `capital_from_wire` (`{"ok": false,
"error": ...}` over the wire), and an unknown spelling is refused the same way.
An absent `capital` keeps the historical behavior — the self-funded probe.
`scripts/executor_three_hop_matrix.py` follows the wire (`--capital
{production,self_fund,zero_balance}`, default `self_fund`, replacing
`--zero-balance`).

### Migration: a zero-capital refusal is typed data, not an `Unsupported` string

The zero-capital probe's losing answer used to arrive as an encoder refusal: the
release turned the core ledger's typed deficit into a `ComposeError::Unsupported`
message, so every consumer saw the same `encode-failed` bucket a grammar gap
produces and the asset, the amount required, and the credit available were only
readable by parsing prose. One value now crosses every surface as data:

| Surface | Retired | Now |

| --- | --- | --- |
| Core derivation (`degenbot_executor::grammar_shape::Derive`) | `InsolventZeroCapital` collapsed into the public `Option`/`Declined` seam | `Derive::InsolventZeroCapital(InsolventZeroCapital { asset, required, available })` |
| Release encoding (`CmdStreamV1Encoding::encode`) | `ComposeError::Unsupported("the path is insolvent at zero initial capital: …")` | `ComposeError::InsolventZeroCapital(ZeroCapitalDeficit { asset, required, available })` — its OWN routine arm; `Unsupported` still means "no representation for this path" |
| Target-selection walk (`PreparedEncoding`) | `Declined(String)` for both | `Declined(String)` for a capability gap, `InsolventZeroCapital(deficit)` for a capital-policy refusal; both advance the walk |
| Candidate record (`SimFailure`) | bucket `encode-failed`, no detail | bucket `insolvent-zero-capital` + `zero_capital: Option<ZeroCapitalDeficit>` |
| PyO3 (`FailureDetail`) | — (nothing to read) | `FailureDetail.zero_capital -> ZeroCapitalDeficit \| None` (`asset` checksummed, `required`, `available`), and the render dict carries the same three fields under `"zero_capital"` |
| Operator wire (`probe_path` response) | `{"bucket": "encode-failed"}` | `{"bucket": "insolvent-zero-capital", "zero_capital": {"asset": …, "required": …, "available": …}}` |
| Matrix report (`scripts/executor_three_hop_matrix.py probe`) | counted as `encode_failed` | its own `planning_refused` row category (with `reason: typed-insolvency`), and each row keeps the typed `zero_capital` object |

`EncodeOptions.zero_balance` — the public bool that spelled the stance — is gone.
The policy is the field `EncodeOptions.initial_capital`
(`degenbot_executor::grammar_ledger::InitialCapital`), projected from the ONE
capital mode by `ProbeCapital::initial_capital`: `production` and `self_fund`
map to `InitialCapital::Held` (ordinary validation), `zero_balance` maps to
`InitialCapital::Zero` (the foundation's zero-capital policy). The stream's
`funding` axis stays a separate field: it says where a family's stream declares
its entry capital, never what the validator grants. There is no alias for the
retired bool, and no consumer keeps a second copy of the stance.

Valid streams are untouched: a stream the policy finds solvent emits exactly the
production bytes and the same `cmd_executor/2` call encoding, so release identity
and bytes are unchanged. Only the refusal's representation is new.

### Migration: a probe's capital override is path-aware, and an unverified layout refuses

The simulation's capital override used to be a mode plus a holder, with the
token set hard-coded in the engine: a zero-balance probe cleared mainnet USDC,
WBTC, the holder's WETH9 word, and its two PoolManager claims whatever the path
crossed, and a self-funded probe installed a blanket mainnet-USDC balance on
every path. A probe could therefore report a path whose untouched token balance
was still there (the simulation read capital the probe claimed to withhold) or
fund an asset the path never declared.

One **path-aware plan** now crosses the seam, composed once per fan-out, and
refused before any EVM state exists when a covered asset's storage layout is not
verified:

| Surface | Retired | Now |
| --- | --- | --- |
| Core plan (`degenbot_simulation::CapitalOverridePlan`) | a `Copy` enum: `Production` / `SelfFund { holder }` / `ZeroBalance { holder }` | a value carrying the mode, the holder, and every RESOLVED override (`CapitalOverride { asset, target, value }`): the asset identity (native / ERC-20 token / PoolManager ERC-6909 claim of a currency), its concrete storage target (the account's balance word, or a contract+slot), and the value installed |
| Layout source | the engine's hard-coded USDC slot 9 / WBTC slot 0 constants plus warmup-derived WETH and claim slots | `degenbot_simulation::CapitalLayouts` — an explicit, pinned descriptor set (mainnet WETH9 `balanceOf`@3, USDC `balances`@9, WBTC `balances`@0, mainnet PoolManager ERC-6909 `balanceOf`@4). A fixture declares its own with `with_token` / `with_manager` (the LAST declaration for a contract wins, so a fixture may re-pin one deliberately); nothing infers a slot from an ABI, an address, decimals, or a `balanceOf` call, and nothing scans storage. **The pinned set has no operator or FFI declaration surface yet**: it is what a shipped build verifies, and only in-process callers (fixtures, embedding code) can extend it |
| Requirement source | a blanket `EXECUTOR_FUND_ETH` USDC grant on every self-funded path | the built Plan's own `SelfFund` seeds, read across the seam by `ExecutionAdapter::capital_requirements` (the same grammar walk the encode performs, `degenbot_executor::composers::encode_cmd_stream_capital`); a family that funds itself in-path declares nothing |
| Composition point | `SimulationDeployment::with_capital(mode)` projected a plan at deployment construction | `SimulationDeployment::with_capital(mode)` stores the POSTURE; `capital_plan(paths)` / `override_params(paths)` compose the plan per fan-out for the candidates that fan-out can still hand to the EVM (see the conservative-union rule below), so the same plan reaches the engine's override application |
| Override application (`apply_simulation_overrides`) | mode branches, hard-coded token list, warmup-derived clearing slots | applies the plan's resolved writes, LAST — after owner funding, declared code, and the settlement warmup — so the plan is authoritative for every asset it names |
| Refusal | an unknown token kept its balance and the probe reported the resulting revert | `CapitalPlanError::UnverifiedLayout { asset }`; production dispatch reports it as `DispatchRefusal::CapitalPlan { error }`, and the dedicated one-path probe as `ProbeError::CapitalPlan(error)` — its own arm, before any EVM work, naming the asset. It is never `InsolventZeroCapital` (that is a capital-policy answer), never an `encode-failed` bucket, and never a silent skip: the Python inline-sim seam records the refused candidate under its own `capital-plan-refused` orchestration bucket, which carries no deficit, no revert bytes, and no failing step, because nothing ran |

Which assets a plan covers is derived from the paths, never from a fixed list:
the holder's native balance, every ERC-20 a hop names, and every PoolManager
ERC-6909 claim a V4 hop's currencies name, deduplicated by identity. A V4 hop's
native currency contributes the native balance and the claim whose id is
`uint160(address(0))`; it never contributes an ERC-20 at a guessed slot.

The path set a fan-out composes is a CONSERVATIVE union rather than an exact
projection of what the EVM will run. A candidate whose encoding the selection walk
already declined (`Declined` / `InsolventZeroCapital` / `Failed`) is filtered
cheaply — it never reaches the EVM, so its assets need no override — but an
unselected or not-yet-encoded (`None`) candidate is still included, because the
plan is composed before every per-candidate encoding decision is final. One
unverifiable touched asset anywhere in that union therefore refuses the whole
fan-out (and, for the one-path `probe_path` op, the probe) BEFORE any EVM work;
the pre-flight is deliberately the conservative side of the same rule.

`ZeroBalance` installs `0` for every one of them. `SelfFund` installs the
declared requirement where the path declares one and `0` for every other touched
holding — native balance, tokens, and PoolManager claims alike — so a
self-funding family, which declares no external entry, grants nothing and still
clears what it touches. Requirements from several paths in one fan-out combine by
MAXIMUM, never by sum: every candidate starts under the same fan-out bankroll, but
a candidate's stream spends only its own Plan's declared entry, so the shared
simulated database has to cover the largest single entry — a sum would grant
capital no single path declared.

The settlement warmup keeps its own meaning and is applied BEFORE the plan: its
1-wei touches and its operational WETH9 balance make the contract's first run
cheap, and the plan then overwrites exactly the assets it names — so a warmup can
never reintroduce capital a zero-balance clearing removed, the warmup's 1 wei is
never read as declared self-funded capital, and a self-funded requirement is never
overwritten by the operational amount. Owner gas funding, the declared/deployed
code, and every account the plan does not name are untouched.

The pinned set covers the canonical mainnet assets a production path crosses, so
such a path resolves every asset it touches — but the write set is the PATHS',
never the retired hard-coded list. A path crossing only WETH composes one token
holding (not USDC and WBTC), a path that touches nothing but native composes one
native write, and a path naming a token outside the pinned set refuses instead of
leaving its balance in place. A driver that needs to report the refusal sees the
Python boundary raise `RuntimeError` (the planning refusal is its own typed probe
error, never a per-candidate `FailureDetail`), and the production fan-out reports
it as `DispatchRefusal::CapitalPlan`. Valid streams are byte-identical: the plan is
EVM state, and no `cmd_executor/2` command byte or release identity moves.

### The `probe_path` op is non-submitting by construction

An execution probe quotes one explicit amount vector and runs it through the
ordinary assembly, production encoding, and EVM2 simulation stages for
observation only. It does so on the core's DEDICATED probe executor
(`degenbot_batch_executor::ProbeExecutor`), whose construction takes only
sim-side handles: no `SubmitLeaf`, no signer, no nonce lane, no relay fan-out,
no receipt probe, and no broadcast target. Non-submission is therefore a missing
field rather than a dry-run flag, a policy check, or a refused enqueue — there is
no submit stage to reach, so nothing has to be screened afterwards.

The retired surfaces are DELETED, not aliased:

| Retired | Why it is gone |
| --- | --- |
| `BatchExecutor.probe_path` (FFI) / `BatchExecutor::enqueue_probe` + `ProbeRefusal` (core) | A probe used to ride the ordered lane and then find its own record on the shared drain. The dedicated executor returns each call's own outcome instead. |
| `SubmissionStance.PROBE` and the `submission=` argument on `build_batch_executor` | The stance existed only to build a submission-capable executor and then refuse probes at the entry. The probe constructor has no submit half to switch off. |
| `capital=` on `build_batch_executor_py` (the submitting constructor) | A production lane simulates the session account's real balances; the probe postures belong to the probe constructor, so the two halves cannot be crossed. |
| `Session._probe_lock`, the serialized critical section, `PROBE_REAP_TIMEOUT_S`, and the abandoned-probe reaper | Correctness no longer depends on them: correlation is per call, and a dropped call aborts its own stage (see below). |

### The typed probe protocol

One design problem, one protocol pair, shared by the FFI, the session, and the
operator:

| Value | Fields |
| --- | --- |
| `ExecutionProbeRequest` (pyclass) | `path_id`, `optimal_input`, `hop_outputs`, `consumed_inputs`, `current_block`, `base_fee_next`, `block_timestamp` — validated at construction (positive input, at least one hop, one output/consumed entry per hop) |
| `ProbeReport` (pyclass) | `capital` (the posture the call ran under) + `records` (the assembly verdict, the simulate verdict with its receipt or typed failure) + `encoded` (`ProbeEncodedCall`: calldata, native value, target, gross/net profit, gas) |
| `ProbeExecutor` (pyclass) | `capital`, `async probe(request) -> ProbeReport` |
| `build_probe_executor(context, dispatcher, engine, provider, *, capital=None, deployments=None, target_selection=None)` | the sim-half construction, ONE per capital posture |

`degenbot.dispatch` re-exports all five under their stable names, and
`Session.probe_path` / `BotRunner.probe_path` return the `ProbeReport` (not a
`BatchOutcomeSet`). The request states what the stages must simulate; the
VECTORS it carries are quoted by the core (see below), never by the driver.

### Migration: a probe report states evidence, so a revert cannot read as coverage

The `probe_path` response used to collapse both receipt-bearing simulations into
one `sim_ok` spelling, and `wrap_handler` forwarded only a fixed handful of keys,
so the probe's `capital`/`zero_capital`/`planning` evidence never reached the
wire. A consumer could therefore classify a row only by its bucket label: any
`failed` row whose bucket was neither `encode-failed` nor
`insolvent-zero-capital` counted as an execution, whatever its revert said.

The response is now a report, and the wire forwards all of it:

| Surface | Retired | Now |
| --- | --- | --- |
| `probe_path` response `outcome` | `sim_ok` for both `profitable` and `gas_unprofitable` | the simulate verdict's own label (`profitable`, `gas-unprofitable`, `failed`, `exception`), or the assembly skip label when no simulation ran |
| `probe_path` response evidence | `path_id`, `permutation`, `capital`, `outcome`, `bucket` — with `capital`/`zero_capital`/`planning` silently dropped by the wrapper | the same fields plus `current_block` (the probed `ExecutionProbeRequest` block), `assembly`, `receipt`, `revert` (the raw failure bytes) and `submission`; `wrap_handler` forwards `PASSTHROUGH_RESPONSE_KEYS` verbatim, and a test pins the projection against that list |
| Non-submission evidence | a paragraph in the docs | a `submission` object in every response: `{possible: false, observed: false, evidence: "dedicated ProbeExecutor has no submit lane"}` — a property of the executor's construction, never an inference from an absent log line |
| `scripts/executor_three_hop_matrix.py probe` | counted `failed && bucket != encode-failed` as executed | six row categories — `planning_refused`, `executed_success`, `economic_reject`, `unexpected_revert`, `assembly_missing`, `exception`; only `executed_success`/`economic_reject` are coverage, and both require their own evidence: a complete integer `receipt`, or the terminal `InsufficientProfit` revert as exactly the selector `0x4e88422a` plus its two ABI words in hex |
| Expected outcome | implicit | a required `--expect executed` / `--expect insolvent` policy; every row records `expected` and `matches_policy`, and any mismatch fails the command |
| Matrix persistence | one write at the end | the schema-version-2 report exists before the first probe with every expected row `pending`, and is atomically replaced after every response; an interruption leaves a parseable incomplete report and a non-zero exit |
| Registration set | counted responses | validated first: exactly 27 or 54 directed rows, unique non-negative engine path IDs, unique `(permutation, direction)` identities, every expected identity present, and an explicit `direction` on every row (an absent one is refused, never read as forward) |
| Response identity | taken from the transport's own correlation | audited per row: the response's `path_id` must be the requested engine path, its `capital` exactly the requested posture, and its `current_block` a non-negative integer, or the row is an `exception` and matches no policy — a profitable verdict for another path is not coverage |
| Typed deficit `asset` | any string with a `0x` prefix of the right length | exactly 40 hex digits behind `0x` (no underscores, no non-hex), because the field names a 20-byte asset |

`gas_unprofitable` is an `economic_reject`, not a success: it has a receipt and
is therefore execution coverage, but it is not a profitable arbitrage. An unknown
bucket or selector, a failed row with empty or missing revert bytes, a padded or
non-hex terminal payload, and a receipt-bearing label with no complete integer
receipt behind it are all rejections of the row's evidence — `unexpected_revert`
for the revert cases, `exception` for the missing receipt.
`insolvent-zero-capital` is a `planning_refused` only with its complete typed
deficit, and under `--expect insolvent` that refusal is the expected row rather
than a process error.

The block a probe ran at is threaded through the Python call site because the
typed `ProbeReport` carries no request context: `execution_probe_response`
requires `current_block`, and the operator handler reads the session's block
clock before dispatch — the clock `Session.probe_path` quotes its typed request
at. The `submission` evidence is checked field by field: `possible` and `observed`
must be the boolean `false` and `evidence` must be the executor-shape sentence
verbatim, so a missing object, a missing key, a truthy claim, a non-boolean, or a
foreign sentence is an `exception` row that fails the run.

The same audit covers the response's identity. The socket pair correlates a reply
with its request, but a report that trusted that alone would accept a verdict for
some other path, a receipt for a posture nobody asked for, or a block the call
cannot have run at: each row therefore checks the response's `path_id` against the
engine path it requested, its `capital` against the requested posture, and its
`current_block` for a non-negative integer. A row that already classified as an
`exception` keeps its own more specific reason.

### The exact-input quote lives in the core

The fixed-input amount vector is derived by ONE Rust exact-input quote interface,
`EngineStages::quote_exact_input_path` (`degenbot_bot::arb_engine::probe_quote`),
reached from Python through `degenbot.dispatch.quote_execution_probe`
(`degenbot._ffi.simulation.quote_execution_probe_py`). Each hop is priced by its
own family's owner:

| Hop family | Owner |
| --- | --- |
| Reserve pair (V2 / Aerodrome V2) | `degenbot_pools::simulate_swap` over the pool entry SNAPSHOTTED when the quote's plan was built — the same production pool math the solver and the Python companion read |
| V3 | the canonical v3-periphery `Quoter` (`0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6`) at the quote's block |
| V4 (hook included) | the canonical v4-periphery `Quoter` (`0x52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203`) at the quote's block, with the pool's WHOLE key — hook address included — in the calldata |

The vector and its block context belong to ONE block: every quoter call is pinned
to it, the reserve-pair snapshot is taken for it, and the request's
`current_block` / `base_fee_next` / `block_timestamp` are read at it. A refusal
(a quoter revert, a partial fill, an unregistered pool) is typed and stops the
call — a probe never runs on a partial vector.

The Python adapter this replaced is DELETED, not aliased:

| Retired | Why it is gone |
| --- | --- |
| `degenbot.runner.session._quote_concentrated_hop` and its `v2`/`v3`/`v4` family branches in `Session._probe_request` | A driver-side copy of each family's pricing. The core owns the derivation, so the operator adapter names only the engine, the provider, the path, the fixed input, and the block. |
| The Python `_V3_QUOTER` / `_V4_QUOTER` / `_V3_QUOTE_SELECTOR` / `_V4_QUOTE_SELECTOR` constants and their hand-rolled ABI encoding | The quoter interfaces are declared once in `degenbot_rpc::abi` as typed `sol!` reads (`IUniswapV3Quoter` / `IUniswapV4Quoter`), so a selector is derived from the Solidity signature and the call is one typed read. |
| The Python `pool.calculate_tokens_out` call (an `asyncio.to_thread` hop into a Python pool object) | The production math runs in the core over the shared state, so the probe no longer needs Python pool objects at all. |

Two deliberate fidelity notes:

- **V3 price limit.** `Quoter.quoteExactInputSingle` substitutes
  `MIN_SQRT_RATIO + 1` / `MAX_SQRT_RATIO - 1` when its caller passes `0` ("no
  limit"). The core encodes the SUBSTITUTED value directly: identical quote (the
  substitution is the quoter's own arithmetic), and byte-stable calldata — which
  is what lets the committed `py_oracle` quoter corpora replay offline as
  pinned-block parity fixtures.
- **V4 quoter address.** The deleted Python path quoted V4 through
  `0x99bd42B2B22e531f283b7F8a86dFf631EB16A9D1`; the core uses the canonical
  v4-periphery deployment `0x52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203` — the same
  one `degenbot_simulation::sim::evm2::v4_black_box` drives in an anchor-pinned
  scratch EVM and the one the committed quoter corpora were recorded under. This
  is a BEHAVIOR CHANGE for V4 probe quotes: the retired literal named a
  different address, and nothing in the tree records a quote — or a pinned
  corpus — through it.

What did NOT change: the op is still a FIXED-INPUT probe; consumed inputs are
still the previous hop's output with the fixed input first; reverse directions
still flip the token pair (V3) and `zeroForOne` (V4); a refusal still stops the
call rather than producing a partial vector; and the request's shape, validation,
and the executor's stage semantics are untouched.

#### Hooked V4 parity is proven LIVE, at a pinned block

The committed quoter corpora record an UNHOOKED V4 pool, so they are never hooked
evidence. Hooked parity is proven by an `#[ignore]`d Rust leg — no offline gate
needs a node — that quotes a REAL mainnet hooked pool through
`EngineStages::quote_exact_input_path` and compares the whole request vector
against an INDEPENDENTLY hand-encoded canonical-quoter `eth_call` at the SAME
block, in BOTH directions
(`rust/crates/engine/degenbot-bot/tests/probe_quote_live.rs` —
`live_hooked_v4_quote_matches_the_canonical_quoter_both_directions`. The leg is an
INTEGRATION test because an opt-in run reads its endpoint from the environment,
and ADR-062 D7 keeps `std::env::var` out of `src/`; it drives only the public
stage surface):

| Fact | Value |
| --- | --- |
| Pinned block | `26_320_000` (slot0 / liquidity read AT it: the pool is initialized there because the chain says so) |
| Pool key | `hooks 0x322dcEc4958C14e021A9F1cD49DF11b9457968cC`, `0x4E67db19044549ff420860834c91b45bad298722` / `0xAe5b1b51ECB247218157ddc6f1E457db25F971d7`, fee `0`, tick spacing `200` |
| Pool id | `0x004de370c1ef268265d1f3f8a91d00bd4bc2d97699f9877dc3d81286fb657bf3` |
| Fixed input | `1_000_000_000_000_000_000` (one 18-decimal unit) |
| Chain answer, zero for one | `4_854_067_101_419_274_970_894` — the core's `hop_outputs` matches |
| Chain answer, one for zero | `201_912_091_397_373` — the core's `hop_outputs` matches |
| Unhooked twin key (`hooks = 0`) | REFUSED by the chain in both directions — the answer is hook-specific |
| The pool's own white-box walk | `4_903_097_821_562_843_550_261` / `203_951_607_472_093` — a different number; that substitution is the one this seam never makes |

The run also pins the block's own context: the core's `ProbeBlockContext` reports
`block 26_320_000`, timestamp `1_793_537_051`, and base fee `40_859_637` — the
values the node reports for that block.

The registered path is a two-hop ROUND TRIP on the pinned pool, because
`EngineStages::register_path` structurally refuses a one-hop path: the fixed
input enters in the direction under test, and the second hop re-quotes the
opposite direction with the chained amount at the same pin. The two chain answers
above are the FIRST hop's (the fixed input's, exactly as recorded); the reverse
hop is held against its own independent `eth_call` rather than a second
hand-copied constant.

### Correlation replaces serialization

Each call owns a `oneshot` channel and spawns its own stage task behind an
abort-on-drop guard, so:

- **Response identity** needs no lock: concurrent probes overlap freely and each
  caller reads the report its own call produced. There is no drain to
  interleave, so no caller can take another's outcome.
- **A capital-mode change** builds (and caches, in `Session.probe_executors`) one
  executor per posture. The in-flight call holds its own reference, so a later
  posture's construction cannot strand it.
- **Cancellation** cancels the call's own stage. The abandoned result is
  discarded where it is produced and no task is detached, so nothing is queued
  for a successor and no reaper is needed. Abort is not preemption: a stage body
  inside a synchronous span finishes that span and has its future dropped at the
  next yield.

**Wire shape.** A probe that returns answers
`{path_id, permutation, capital, current_block, assembly, submission, outcome}`,
plus `receipt` when the simulate verdict carried one, `bucket`/`revert` on a
failure, `zero_capital` (`asset`/`required`/`available`) when that failure is the
capital-policy refusal, and `planning` (the encoder summary: `execution_target`,
`value`, `calldata_len`) whenever a row was composed. `submission` is the
structural evidence object, never a submit RECORD: the response has no submit
verdict and no submit-record list, and `execution_probe_response` still refuses a
record carrying a submit verdict as the detective check beside the structural
guarantee. `wrap_handler` forwards exactly `PASSTHROUGH_RESPONSE_KEYS`, so a
projection that grows a key must name it there or the client never sees it.

## 3. The CLI client

`degenbot path` in `src/degenbot/cli/path.py` is a thin client over
`send_command` (no local pool/db work):

```
degenbot path add --socket /tmp/bot.sock \
    --hop v3:0x<weth/usdt> --hop v4:0x<manager>:0x<pool_id>:[--direction zfo]
degenbot path discover --socket /tmp/bot.sock [--bound 5]
```

Hop spelling is `FAMILY:ADDRESS` (V2/V3) or `V4:ADDRESS:HASH` (HASH = the 0x+64
pool id). `--direction zfo`/`ozf` applies a per-hop True/False list; omitting it
lets the bot auto-resolve. A rejecting or unreachable host surfaces as a
`click.ClickException`, so a failed command exits non-zero rather than silently.

## Why no in-process CLI hack

The `degenbot` CLI cannot add a path "into" a running bot by importing the
session — the bot lives in a different process. The socket channel is the honest
boundary: the CLI is a driver, the bot's process owns the state, and the wire
is a tiny versioned protocol a client of any vintage can talk to.

## Test coverage

- `tests/operator/test_operator_channel.py` — wire round-trip (`add_path`,
  `discover`), family mapping, error → `ok:false` host-survival, malformed
  request, `wrap_handler` normalization.
- `tests/cli/test_path_cli.py` — hop parsing, `--direction` mapping, live
  round-trip + error exit against a threaded `OperatorServer`.
- `tests/arbitrage/test_arbitrage_session.py` — programmatic surface raises
  without a live pipeline; a mid-run add during an advancing consumer does not
  stall or abort dispatch.
- `tests/arbitrage/test_execution_probe.py` — the typed request the session
  builds (quote derivation, block facts, validation refusals), the
  non-submitting construction (no signer, nonce read, submit provider, relay
  dial, or run-mode stance crosses it), one executor per capital posture, and
  the correlation contract: concurrent probes overlap and each keeps its own
  report, a capital-mode swap strands nothing, a cancelled probe cancels its own
  call, and a cancel during quoting never reaches the executor.
- `tests/dispatch/test_probe_submission_safety.py` — the FFI-level end state: a
  POWERED profitable probe produces a record + an encoder planning row and NO
  submit verdict, submission record, access-list call, or broadcast; malformed
  requests, unknown paths, hop-count mismatches, and every capital posture and
  misspelling are refused or accepted as named; concurrent and cancelled calls
  never take one another's outcome; the retired `probe_path` / `enqueue_probe`
  entries are absent.
