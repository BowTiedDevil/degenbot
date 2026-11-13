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
`BatchOutcomeSet`). The request's per-hop vectors stay the Python boundary's own
exact-input quote derivation; the request only states what the stages must
simulate.

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
`{path_id, permutation, capital, outcome[, bucket][, planning]}`;
`planning` is the encoder summary (`execution_target`, `value`, `calldata_len`)
whenever a row was composed. There is no submission record in the response, and
`execution_probe_response` still refuses a record carrying a submit verdict as
the detective check beside the structural guarantee.

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
