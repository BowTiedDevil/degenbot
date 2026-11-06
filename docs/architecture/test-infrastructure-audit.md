# Test infrastructure audit: the pump's sealed test seams and the Python helper surface

**Status: audited.** Two dispositions, both durable notes rather than sweeps: the Rust pump test
seams seal one coverage claim into a single crate (recorded here so it is deferred, not silently
assumed), and the Python test-helper surface's overlaps are inventoried below with one consolidation
taken and the rest left with reasons. Paths in the Rust section are relative to `rust/crates/`; the
Python ones from the repo root.

## Rust: the pump seams are `#[cfg(test)]`, so one coverage claim is deferred

A session is put into `Subscribed` with a live stream, or into `Running` over a stand-in loop, only
by two seeds inside `degenbot-bot`:

| Seam | Home |
|---|---|
| `PumpSession::seed_connected_for_test` — a session already in `Subscribed`, owning a local pump and its stream | `engine/degenbot-bot/src/arb_engine/pump_session.rs:688` |
| `PumpSession::seed_running_for_test` — a session already in `Running` over a stand-in loop | `.../arb_engine/pump_session.rs:695` |

Both are `#[cfg(test)] pub(crate)`, so they compile for that crate's own unit tests and for nobody
else's. The driver's injected faults and holds are the same shape: `connect_fault`,
`connect_gate`, `run_gate`, `backfill_fault` (`engine/degenbot-bot/src/arb_engine/driver.rs:482`,
`:485`, `:488`, `:491`), `InFlightHold` (`:383`), `ConnectFault` (`:396`), and
`local_pump_for_test` (`:1263`).

**The deferred claim:** a successful `resume()` followed by `run()` consuming the armed stream.
`degenbot-batch-executor` owns the session tests that drive a consumer against a driver, so it can
pin every route that ends in a refusal and the report that follows one —
`a_refused_resume_still_closes_the_session`
(`engine/degenbot-batch-executor/tests/session_consumption.rs:500`) and
`the_session_report_carries_the_typed_end_and_teardown` (`:550`) — but it cannot build the armed
case at all.

The claim lands when one of two things moves it:

- the seeds go behind a `test-utils` feature, the shape `degenbot-simulation`'s `test-harness`
  already has (`engine/degenbot-simulation/Cargo.toml:165` `test-harness = []`, with the gated test
  targets declaring `required-features`); or
- the session-consuming coverage relocates into `degenbot-bot`, the crate that owns the seeds.

Until one of those lands, that route is untested by design: no test may be read as covering it.

## Python: the helper surface, and what overlaps what

`tests/helpers/` is 27 modules and 4,624 lines; `tests/fakes/` is 10 modules and 1,087. The
pairs below are the ones that looked like a second home for the same thing. Only the first is a real
duplicate; it was consolidated, and everything else is recorded with the reason it stays.

### Consolidated: the provider double

`FakeAsyncW3` was defined twice — a flat copy in `tests/helpers/session.py` and the copy with an
`.eth` façade in `tests/helpers/boot_actors.py:93` — with the same external surface and the same
answers (block `12_345`, tx count `7`, empty `make_request`, `http://fake:8545`, no
`as_async_alloy`). It is now one class in `tests/helpers/boot_actors.py:93`, imported by the cockpit
fixture (`tests/helpers/session.py`) and by `tests/arbitrage/test_arbitrage_session.py`. The provider
is an injected *actor*, which is what that module is the one home for.

### Left with a reason

| Overlap | Evidence | Disposition |
|---|---|---|
| Two `InjectedActors` builders | `injected_test_actors` (`tests/helpers/session.py:125`) and `boot_runner`'s inline record (`tests/helpers/boot_actors.py:121`) each spell the seam record out by hand — 11 of its 13 fields apiece, and not the same 11 (`boot_runner` sets `settlement_arm`, the cockpit fixture sets `relay_posture`; neither sets `stop_engine`) | **Leave; named consolidation.** `boot_runner` should pass overrides into `injected_test_actors`. Not trivially safe: the two disagree on purpose — a `None` `readiness`/`settlement_endpoints` is the REAL resolver in the boot pair (the gates those tests exercise) and a fake in the cockpit pair — so the merge needs a typed real-vs-fake choice plus a red/green pass over `tests/arbitrage/test_posture_driven_boot.py` and `tests/dispatch/test_relay_posture.py` |
| Two readiness doubles | `FakeReadiness` (`tests/helpers/session.py:108`: `active_backrun_facets` + `settlement_active`, default ON) vs `FakeBootReadiness` (`tests/helpers/boot_actors.py:74`: `settlement_active` is the only field, default OFF) | **Leave.** The opposite defaults encode the two seams' postures: a cockpit session starts settlement-active, a boot gate starts refused and the test opts in. One class would make the default a required argument at every call site for no behavioural gain |
| Three "session" modules | `tests/helpers/session.py` (the fake posture `Session.for_test` starts over), `tests/helpers/session_drive.py` (a REAL core executor driven through `SessionConsumer.scripted`, `:81` `offline_executor`), `tests/fakes/session.py` (the session-registry identity methods a `Bot` double must carry) | **Leave.** Three subjects share one word; the fake-vs-real split is the point of the first two. `tests/fakes/session.py` has one consumer (`tests/test_bot.py:51`), so the shared filename is a naming smell, not duplication |
| Six pool factories + the handle factory | `make_v2_pool`, `make_v3_pool`, `make_v4_pool`, `make_curve_pool`, `make_balancer_*_pool`, `make_aerodrome_v2_pool`, and `pool_handle_factory`'s registrars | **Leave.** The shared default already has one home — a fresh isolated `Bot` (`tests/helpers/pool_bot.py:20`). What differs per family is the typed boundary conversion (V2's fee to a retained-fraction pair, `tests/helpers/v2_pool_factory.py:34`; V3's tick-row reorder, `tests/helpers/v3_pool_factory.py:36`) and each registration's keyword set, which a generic factory would take as a family-shaped dict. Residual: five copies of pool_bot's isolation paragraph in prose — cosmetic, no code duplicate |
| Two log builders | `tests/helpers/synthetic_logs.py` (pool events for `Bot.dispatch_log`) vs `tests/helpers/aave_logs.py` (Aave V3 `eth_getLogs` entries for the mock-RPC market) | **Leave.** Both encode decoder wire shapes, but the Aave set is keyed to the seeded market and stays importable alone on purpose — a fixture-only test must not pull in the HTTP server and the SQLite seeding (stated in its module docstring) |
| Swap-amount matrices | `tests/helpers/swap_amount_multipliers.py`: three matrices | **Leave.** The disposition is already recorded in the module docstring: a matrix moves here only when its value set is identical to an existing one, so the parity ladder is not the Uniswap matrix with extras |
| Three config doors | `tests/helpers/verdict_probe.py` (fresh interpreter + `resolve_hypothetical`), `tests/helpers/rpc_env.py` (the installed config holder), `offline_cfg` (`tests/helpers/session.py:41`, the explicit override layer) | **Leave.** Three different questions about one cascade; the rule that tells them apart is single-homed in `verdict_probe.py`'s module docstring and `rpc_env.py` cites it |
| Two `Bot` names | `tests.helpers.seam.Bot` is the `_ffi` handle (`tests/helpers/seam.py:51`, ADR-013's one test-side door); `degenbot.bot.Bot` is the Python facade | **Leave.** The collision is in the domain vocabulary, not in the helpers: the seam re-export is what makes an `_ffi` rename a one-file edit |
| Tests living inside `tests/helpers/` | `test_contract_compat.py`, `test_v2_pool_factory.py` | **Leave.** Each is its helper's own test, kept beside it and collected by the suite's `test_*.py` pattern |

The rest of the surface has one home and no twin, so it is listed rather than tabulated:
`seam.py` (the one test-side door to `_ffi`), `boot_actors.py`, `pool_bot.py`, `v2_cycle_engine.py`
(the one offline two-hop engine, `:30` `build_two_hop_v2_engine`), `erc20_factory.py` (the token half
all the pool factories seed through), `sim_records.py`, `database.py`, `contract_compat.py`,
`balancer_queries_abi.py`, and the ten `tests/fakes/` doubles.
