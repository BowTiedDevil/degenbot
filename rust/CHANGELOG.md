# Changelog

All notable changes to the Rust workspace crates are recorded here.

## Unreleased

### Added

- **One ERC-20 metadata read: `degenbot_rpc::erc20`.** `name()` / `symbol()` /
  `decimals()` now have a single chain-read home over the `ContractRead` seam:
  `read` (one token, with a `FieldMask` for Pool construction's partial
  fallback reads) and `read_batch` (ordered Multicall3 reads for the two
  eager shapes; `LowerUpperFallThrough` degrades directly to per-token
  `read` calls), under a closed `MetadataShape` — `LowerOnly` (lower
  spelling, ABI-value decode, short-circuiting sequential form),
  `LowerUpperFallThrough` (both spellings,
  any failure falls through, `bytes32` string answers with NULs trimmed,
  `u8`-width decimals), and `LowerUpperStopOnCallError` (both spellings, a
  call failure ends the field, strict dynamic-string decode with the
  trailing-NULs-only `bytes32` fallback). Results carry per-field absence;
  consumer policy stays with the consumers (Pool construction's DB-first /
  UNKNOWN sentinels / all-or-nothing write-back, Aave's per-field nullable
  cells and `i64` decimals width). `degenbot_rpc::multicall3::multicall3_batch`
  now crosses the same `ContractRead` seam (signature-only change; wire
  behavior unchanged).

  | Was | Is |
  | --- | --- |
  | `degenbot_bot::bot_core::pool_builder::builder::fetch_erc20_metadata` + private selector/decode/fallback helpers | deleted — `build_erc20_metadata` resolves through `degenbot_rpc::erc20::read` (`LowerOnly`, then a masked `LowerUpperFallThrough` fallback) |
  | `degenbot_bot::bot_core::fetch_erc20_metadata_batch`'s private multicall mechanics | the shared `degenbot_rpc::erc20::read_batch` (`LowerOnly`) folded to the seam's all-or-nothing per-token tuple |
  | `degenbot_rpc::abi::fetch_string_field` | deleted — the name/symbol spelling read lives in `degenbot_rpc::erc20`; non-metadata declared-shape reads keep `fetch_uint256_field` |
  | Aave `config_dispatch::fetch_erc20_metadata` / `fetch_erc20_metadata_batched` / `decode_dynamic_string` / `string_field_from_batch` / `decimals_from_batch` | deleted — `degenbot_rpc::erc20::read` / `read_batch` (`LowerUpperStopOnCallError`); the market-id read decodes through `degenbot_rpc::erc20::decode_string_return` |

### Changed

- **`PoolConstruction` is the one construction request from `Bot::construct_pool`
  through route dispatch — a breaking cut of the `bot_core::V3RouteInputs`
  re-export (ADR-085).** The door hands its family-tagged request to
  `route::construct_pool` unchanged, and the route dispatches over that same
  value: a family variant change touches one request definition and one route
  arm, not two enum definitions and a translation match. The V3 family's DB,
  tick-word source, and slot-layout values ride `PoolConstruction::V3` as
  before; no side request value survives. The migration rows:

  | Was | Is |
  | --- | --- |
  | `PoolConstruction::into_route_request` split the door request into a `(RequestedPool, V3RouteInputs)` pair the route re-matched | `route::construct_pool` takes `&PoolConstruction` and matches its family variants directly |
  | `RequestedPool` — the route's own copy of the family vocabulary | deleted — `PoolConstruction` is the walk's request |
  | `V3RouteInputs` — the V3-only side request (`db`, `fetcher`, `slot_layout`) | deleted — those values ride `PoolConstruction::V3` |
  | `degenbot_bot::bot_core::V3RouteInputs` (public re-export) | deleted — construction is `Bot::construct_pool` over `PoolConstruction` |

- **Non-consuming consumer attach refusal: the second-attach legality rule is
  the core's.** `degenbot_batch_executor::session::SessionConsumer::attach` is
  now a typed move returning `Result<(), AttachRefusal>`: while a session is
  attached, a second attach refuses replacement instead of silently swapping
  the attached session, and `AttachRefusal { error, handoff, driver }` hands
  back every un-installed value so the refusal consumes nothing by
  construction — the refused hand-off and driver attach to another consumer
  unchanged and the first session keeps working (attach-after-run, second-run
  `NoSession`, run-without-resume, and the unguarded `consume(EventFeed)` are
  unchanged). `SessionConsumer::attach_with(driver, acquire)` is the
  transactional form for a caller that takes a hand-off from somewhere else:
  the core checks admission FIRST and invokes the acquisition closure only when
  attachment can proceed (the Python adapter's `engine.take_handoff()` cannot
  run for an already-attached consumer, so a refused attach leaves that
  hand-off claimable), and installation after acquisition has no refusal path.
  Its `AttachFailure` keeps the stages distinct — `Refused` (the core's
  `AlreadyAttached`, the closure returned UN-invoked) versus `Acquisition` (the
  caller's own error, passed through untouched) — and `attach_admissible()` /
  `already_attached()` are private admission internals, with
  `ConsumerErrorKind::AlreadyAttached` stamped at the core
  `already_attached()` constructor — the ONE stamping site. The
  Python surface gains the typed classification: every attach/resume/run
  refusal raises `degenbot.dispatch.SessionConsumerError` (adapter-local slot
  refusals no longer raise a bare `RuntimeError`) with a `kind` attribute — a
  `degenbot.dispatch.ConsumerErrorKind` member for core-stamped refusals and
  failures, `None` for adapter-local ones — while `batch_index`/`report` stay
  uniform and only `LeafFailure` wears the loud-abort prefix. The
  `ConsumerSlot` keeps the GIL one-in-flight take/restore, terminal `Spent`,
  and the scripted-origin attach rules unchanged — deliberately stricter than
  the core (the slot refuses attach-after-run where core `attach` admits it):
  an asymmetry between calling-convention guards and session legality, not a
  divergence.

  | Was | Is |

  | --- | --- |

  | `SessionConsumer::attach(handoff, driver) -> ()` silently replaced an attached session | `attach` returns `Result<(), AttachRefusal>` (non-consuming; un-installed values handed back); `attach_with(driver, acquire)` is the transactional form returning `Result<(), AttachFailure<E, A>>`, admission checked BEFORE the acquisition closure runs |

  | `ConsumerErrorKind` — `NoSession` / `ClaimSpent` / `DriverRefusedTheMove` / `LeafFailure` | + `AlreadyAttached`, stamped at the core `already_attached()` constructor (private; shells stamp no kind of their own) |

  | the Python slot's own `Fresh(_, Attached)` attach refusal (`RuntimeError`, "one session has one consumer") | deleted — the core's shared rule refuses it (`SessionConsumerError.kind == AlreadyAttached`) before the engine's hand-off is taken |

  | Python slot refusals raised bare `RuntimeError`, and `SessionConsumerError` exposed no typed kind | every attach/resume/run refusal raises `SessionConsumerError`; `kind` is the core `ConsumerErrorKind` member or `None` (adapter-local), and `ConsumerErrorKind` joins `degenbot.dispatch` (FFI `degenbot._ffi.simulation.ConsumerErrorKind`) |


- **Core-owned Session-end ranking: one closed end observation, one pure `rank`
  fold.** `degenbot_bot::arb_engine::session_end` (umbrella
  `degenbot::session_end`) now owns the deterministic fold of the gathered end
  facts — detection cause, translated consumer outcome, registration outcome,
  and the consumer's already-retrieved output — into one `SessionEndVerdict`
  (`PumpEnded` / `RegistrationFailed` / `WatchdogTripped`), one `RaiseSlot`
  (`Nothing` / `Registration` / `Consumer` / `SessionEnd`), and the retained
  consumer output. Registration failure is maximal (a registration task panic
  is never clean completion); an intended consumer end disarms a simultaneous
  detection fact (the expected close also publishes `PumpFinished`); detection
  without an intended consumer end and the consumer's `PumpStopped` end are the
  unexpected session end (`WatchdogTripped` + the `SessionEnd` raise slot,
  whose loud failure is the one delivered primary failure), while the empty
  no-failure remainder is `PumpEnded` + `Nothing`; the fold is total over every
  input combination and independent of fact arrival order.
  New public items: `SessionEndObservation`, `SessionEndRanking`,
  `SessionEndVerdict`, `RaiseSlot`, `ConsumerOutcome`, `RegistrationOutcome`,
  `rank`, and `SessionEndCause::from_name`. `degenbot-batch-executor` owns the
  `ConsumerEnd` / `ConsumerReport` -> `ConsumerOutcome` translation — the core
  module never names `ConsumerEnd`. Both Session watches become adapters over
  the fold (gather/retrieve facts, cancel/reap, render at most one primary
  failure; the consumer output is retained and never re-raised over a
  registration or session-end failure).

  | Was | Is |
  | --- | --- |
  | each driver's Session watch ranked its own end verdict from task-completion order | one core pure fold: `degenbot_bot::arb_engine::session_end::rank` over the closed `SessionEndObservation` |
  | `examples/settlement_bot`'s local `session_watch::SessionEndVerdict` enum | the core `SessionEndVerdict` + `RaiseSlot` (the example re-exports the verdict) |
  | Python `SessionEndVerdict` + the `_WatchKind` local task-completion table | `SessionEndVerdict` / `RaiseSlot` constructed from `session_end_rank`'s stable names (`degenbot.arbitrage`) |
  | `consume_result_batches` raised the `pump-stopped` abort inside the consumer task | it returns the typed report; the session watch raises `pump_stopped_abort()` at its one `SessionEnd` slot |
  | the Rust watch lost the consumer output at its top-of-loop finished check and masked a missing consumer as `WatchdogTripped` | the output is retrieved on every path (including a simultaneous registration failure) and the missing-consumer refusal propagates (`supervise_consumer` returns `Result`) |

- **The registration outcome ledger reports its own registration-skip census
  facts.** The one outcome fold (`PipelineReport::absorb`) is now the sole
  emitter of `CensusEvent::RegistrationSkip`: the fine-to-coarse label collapse
  lives beside the census event, the driver-side reason-string round trip is
  deleted, and the metric can no longer disagree with the counters. One label
  change: `BuildV2`/`V3`/`V4Refused` now count as `pool-build-error` (the dead
  `build-v2:`-prefix arm's intent) where they previously fell to `other`.

  | Was | Is |
  | --- | --- |
  | `degenbot._ffi.Bot.record_registration_skip(reason)` | deleted — the fold reports the fact at classification |
  | `registration_skip_kind` / `Bot::record_registration_skip` | deleted — one typed mapping beside the census event |
  | `build_paths._observe_skip`'s meter hop | deleted — the cockpit keeps display only |
  | the registry-dedup site's `"dup"` census literal | deleted — both dedup producers fold once, the fold reports once |

- **ADR-093 (the settlement session): one lifecycle-only Driver seam, one
  ordered Session event stream with its Consumer claim, and one Session
  consumer in the core.** The driver stops forwarding engine operations and
  stops owning the consume loop; both first-class consumers (the pure-Rust
  `examples/settlement_bot` and the Python cockpit) become adapters over the
  same core values. The eight public was→is rows a consumer migrates by:

  | Was | Is |
  | --- | --- |
  | `take_result_receiver` / `take_block_receiver` — two once-only channel hand-offs latched by `result_receiver_taken` | `claim_consumer() -> Result<EventHandoff, ClaimRefusal>`: one hand-off carrying the ordered `SessionStream` and the non-cloneable `ConsumerClaim`; `run(claim)` starts production |
  | `subscribe` → `resume` → `stop` | `connect` → `run` → `close`; `close()` is asynchronous, total from every pose, idempotent and shared, and returns the typed `TeardownReport` |
  | the driver's engine mirrors (`register_path`, `register_and_solve_path`, `deregister_path`, `set_path_cap`, `path_count`, `path_dedups`, `path_info_for`, `apply_retune`, `set_inline_simulator`, `latest_results`) | `EngineStages` directly: take one `driver.stages()` handle and keep it |
  | the verify method matrix (`set_verify_rpc_url` / `set_verify_state_view`, the verify reads, the four `run_*_registration_lifecycle` wrappers) | one Verify lifecycle: `degenbot_bot::bot_core::{VerifyLifecycle, run_verify_lifecycle, run_registration_lifecycle}`, with the provider and the bounded retry as construction values, plus `PoolVerifications` for the claim facts |
  | hand-wired registration (the crawl unit's stage order, the example's `prepare_candidate`, `build_paths.py`) | one Registration admission (`degenbot_bot::arb_engine::crawl::admission::RegistrationAdmission`) behind the crawl and direct-registration adapters |
  | shell consume loops (`examples/settlement_bot/src/consume.rs`, `src/degenbot/runner/_consume.py`) | `degenbot_batch_executor::session::SessionConsumer`: the shells keep only the display drain and the report rendering |
  | `ConsumerError` with no outcome class: a refusal read as a leaf failure (the Python `SessionConsumerError` wore the loud-abort prefix for both) | `ConsumerError { kind: ConsumerErrorKind, .. }` — `NoSession` / `ClaimSpent` / `DriverRefusedTheMove` / `LeafFailure`; the Python class maps the three refusal kinds prefix-free and only the leaf failure under the prefix |
  | `SessionEventSource` — one name for two roles: the batch-executor's consumer-input trait and `degenbot-bot`'s event producer | `EventFeed` for the input trait (`degenbot_batch_executor::session::EventFeed`; umbrella `degenbot::batch_executor::EventFeed`), the bot's producer keeping `SessionEventSource` and the FFI-private origin enum being `EventOrigin` — three roles, three names |

  The stream is `SessionEvent::{Block, Batch}` — a `Block(N)` precedes every
  `Batch(N)`, no variant carries an end item, natural end is the stream's
  exactly-once closure, and the typed terminal fact rides the session-end
  surface. The Python surface moves with it: the engine object keeps
  `connect`/`start`/`close`/`session_end_future` over the seam, the hand-off
  moves through `degenbot.dispatch.SessionConsumer.attach(engine)`, and that
  consumer's `resume`/`run` are the session moves; `block_stream` and the
  driver's own consume loop are gone. The one mapping wart ADR-093 §6.6 records
  — a `resume`/`run` refusal raised as `SessionConsumerError` under the
  executor's loud-abort prefix, told from a leaf failure only by the message
  body — is closed by the typed classification below.

- **The Python `SessionConsumer` attach transition is refused by name where it
  used to replace or discard.** `degenbot.dispatch.SessionConsumer` holds its
  lifecycle as a slot (`Fresh(consumer, origin)` / `Running(origin)` / `Spent`)
  instead of an `Option` + `ran` pair, and `attach` is guarded by it: a consumer
  that already holds an engine hand-off (previously a second `attach`, even with
  a different engine, silently replaced the live session) and a scripted
  consumer (previously the attach silently discarded the scripted receiver and
  drove the live arm instead) now raise a `RuntimeError` naming the refused
  transition. The slot is checked before the engine's one-shot hand-off is
  taken, so a refused attach leaves the other engine's stream claimable. The
  unattached / claim-spent / driver refusals keep delegating to the core, which
  stamps their kind (see the classified abort mapping below).

- **The session consumer's abort mapping is classified by typed kind — a
  Rust-API break; the Python class keeps its name.** `ConsumerError` in
  `degenbot_batch_executor::session` gains a `kind: ConsumerErrorKind` field
  (`NoSession`, `ClaimSpent`, `DriverRefusedTheMove`, `LeafFailure`), stamped
  only where the core constructs the error — the three refusal constructors and
  the two consume-loop leaf-failure sites — and `ConsumerErrorKind` is
  re-exported from the crate root. `ConsumerError` is a `pub` struct of `pub`
  fields with no `#[non_exhaustive]`, so an external struct-literal constructor
  must name the field: a breaking additive change, licensed by ADR-088's hard
  cutover. The existing `detail` / `batch_index` / `report` fields and the
  report shape are unchanged. The FFI classifier reads that kind and never
  re-classifies: `degenbot.dispatch.SessionConsumerError` keeps its name, its
  base and its `batch_index` / `report` attributes, and splits its message — the
  three refusal kinds map prefix-free carrying the refusal's own text (no leaf
  ran), and `LeafFailure` maps under the shared loud-abort prefix with the
  leaf's detail. The prefix string has one home (`submission::sim_pipeline`'s
  `pub(crate)` constant) instead of two byte-identical copies. The spent-claim
  refusal in `SessionConsumer::resume` now names its own transition through a
  new `claim_spent` constructor — `the consumer's claim was already used;
  resume refused` — where it used to borrow the no-session text
  (`... no attached session to resume (the claim was already used)`).

- **The Python seam's dict boundary is documented, and the one payload
  projection it had duplicated is back in the core.** Exactly two shapes cross
  the Python seam as dicts: a **scripted session's ordered events** (`block` /
  `batch`, the engine result-channel shape, accepted only by
  `degenbot.dispatch.SessionConsumer.scripted` — the live door attaches the
  engine's hand-off in Rust, so no dict crosses on the production path) and the
  **drained submit-lane records** (`BatchOutcomeSet.submit_records`, decoded by
  `degenbot.dispatch.typed_submit_record`). Everything else the seam exposes is
  a typed pyclass (the outcome records, both verdicts, the report counts on
  `SessionConsumerError`), and no derived product — the block clock, a batch's
  work, an assembly/sim/submit verdict — crosses as data for a shell to
  re-derive. The adapters own the translation only: `payload_row_from_dict`
  parses a caller's payload dict into `degenbot_batch_executor::row::PayloadRow`,
  and the projection back into the engine's `SimulatedPathResult` is now the
  core's own `degenbot_batch_executor::session::simulated_path` (umbrella
  `degenbot::batch_executor::simulated_path`), the pinned inverse of
  `payload_row` — the field list has one home instead of a hand-written copy in
  the Python shell.

- **ADR-084 D2 (the engine's doors):** `degenbot-bot`'s two capability homes
  are curated surfaces. Every `bot_core` / `arb_engine` submodule is
  `pub(crate)`; the items the drivers use are re-exported at those two module
  level, so a consumer names ONE segment below the door
  (`degenbot_bot::bot_core::Bot`, `degenbot::bot::arb_engine::EngineDriver`)
  and never an implementation module beneath it. The external-import census is
  pinned by `architecture_gates::no_bot_core_or_arb_engine_deep_paths`, which
  covers every member crate's sources, test and example targets included.
  `degenbot_bot::fleet_intake` and `degenbot::session_end` stand as module
  doors in front of the crate-private trees. The removed published paths, with
  their replacements:

  | Was | Is |
  | --- | --- |
  | `degenbot_bot::bot_core::pool_builder::builder::*` | `degenbot_bot::bot_core::*` (the build verbs, `PoolFamily`, `BalancerFamily`, `PoolBuilderError`, `V4BuildResult`, `V4PoolBuildIdentity`, `V4PoolBuildOverrides`, `derive_hook_flags`) |
  | `degenbot_bot::bot_core::pool_builder::construction_door::*` | `degenbot_bot::bot_core::{PoolConstruction, ConstructionVerdict}` |
  | `degenbot_bot::bot_core::pool_builder::route::*` | `degenbot_bot::bot_core::{BuiltIdentity, ConstructedPool, ConstructionRefusal, ConstructionRoute, V3RouteInputs}` |
  | `degenbot_bot::bot_core::pool_builder::curve_choreography::fetch_curve_balances` | `degenbot_bot::bot_core::fetch_curve_balances` |
  | `degenbot_bot::bot_core::pool_builder::parity::IdentityDivergence` | `degenbot_bot::bot_core::IdentityDivergence` |
  | `degenbot_bot::bot_core::construction_io::*` | `degenbot_bot::bot_core::*` (`ConstructionIo`, the two adapters, `DbConstruction`/`RpcConstruction`, `BlockSelector`, `PacedRead`, `TickFetchPace`) |
  | `degenbot_bot::bot_core::registration::*` | `degenbot_bot::bot_core::*` (`parse_address_str`, `tick_rows`, `ensure_registration_fleet_hosted`, `V4RegisteredEcho`) |
  | `degenbot_bot::bot_core::registration_ledger::*` | `degenbot_bot::bot_core::*` |
  | `degenbot_bot::bot_core::registration_lifecycle::*` | `degenbot_bot::bot_core::*` |
  | `degenbot_bot::bot_core::build_register::*` | `degenbot_bot::bot_core::*` |
  | `degenbot_bot::bot_core::bot::*` | `degenbot_bot::bot_core::*` (`Bot`, `SnapshotOpenError`, `CloseSnapshotTxError`, `BlockStreamError`) |
  | `degenbot_bot::bot_core::reorg_coordinator::*` | `degenbot_bot::bot_core::*` |
  | `degenbot_bot::bot_core::liquidity_verifier::verify_live_map` | `degenbot_bot::bot_core::verify_live_map` |
  | `degenbot_bot::bot_core::pool_ops::*` | `degenbot_bot::bot_core::*` (the calc/simulate/encode entries + `SwapOpError`) |
  | `degenbot_bot::bot_core::refresh::*` | `degenbot_bot::bot_core::*` (`refresh_pool`, `RefreshError`, `RefreshOutcome`) |
  | `degenbot_bot::bot_core::session_boot::*` | `degenbot_bot::bot_core::*` |
  | `degenbot_bot::bot_core::verification_retry::*` | `degenbot_bot::bot_core::*` (`RetryPolicy`, `retry_policy_from_config`, `retry_verification_call`, `is_retryable`) |
  | `degenbot_bot::bot_core::stage_telemetry::StageTelemetry` | `degenbot_bot::bot_core::StageTelemetry` |
  | `degenbot_bot::arb_engine::lifecycle::PathRegistrationError` | `degenbot_bot::arb_engine::PathRegistrationError` |
  | `degenbot_bot::arb_engine::fleet_status::*` | `degenbot_bot::arb_engine::*` (`fleet_runtime_status`, `FleetBinding`, `FleetProfileSummary`) |
  | `degenbot_bot::arb_engine::fleet_wake::wake_hosts` | `degenbot_bot::arb_engine::wake_hosts` |
  | `degenbot_bot::arb_engine::inline_sim::*` | `degenbot_bot::arb_engine::*` |
  | `degenbot_bot::arb_engine::session_end::*` | `degenbot_bot::arb_engine::*` (umbrella: `degenbot::session_end::*`) |
  | `degenbot_bot::arb_engine::fleet_intake::*` | `degenbot_bot::fleet_intake::*` |
  | `degenbot_bot::arb_engine::{driver, engine_stages, path_info, path_objects}::*` | `degenbot_bot::arb_engine::*` |

  Two crate-owned labs needed the tree itself rather than a consumer door: the
  backrun-resolver lab is now an in-crate `#[cfg(test)]` module
  (`bot_core/backrun_resolver_tests.rs`) because an integration target cannot
  see `pub(crate)`, and the observability snapshot lab reaches
  `StageTelemetry` through the door (ADR-084 D2(b), decided per lab).

- **ADR-084 D7 (the umbrella construction surface):** the umbrella's
  construction re-exports are pruned to the ADR-078 D2 door — `PoolConstruction`,
  `ConstructionVerdict`, and the types those two publicly name
  (`ConstructedPool`, `BuiltIdentity`, `ConstructionRefusal`, and the
  `PoolConstruction::V4` overrides type). The builder verbs and the construction
  interior stay engine-side, and `route::construct_pool` is now `pub(crate)`
  behind the door verb, so construction has one consumer entry — the verdict.
  `RequestedPool::V4` carries its own tick-map handle, so no V4 data rides under
  the `V3RouteInputs` name. The removed published paths, with their
  replacements:

  | Was | Is |
  | --- | --- |
  | `degenbot::build_v2` | `degenbot_bot::bot_core::pool_builder::builder::build_v2` |
  | `degenbot::build_v3` | `degenbot_bot::bot_core::pool_builder::builder::build_v3` |
  | `degenbot::build_v4` | `degenbot_bot::bot_core::pool_builder::builder::build_v4` |
  | `degenbot::build_aerodrome_v2` | `degenbot_bot::bot_core::pool_builder::builder::build_aerodrome_v2` |
  | `degenbot::build_balancer_weighted` | `degenbot_bot::bot_core::pool_builder::builder::build_balancer_weighted` |
  | `degenbot::build_balancer_stable` | `degenbot_bot::bot_core::pool_builder::builder::build_balancer_stable` |
  | `degenbot::build_curve_pool` | `degenbot_bot::bot_core::pool_builder::builder::build_curve_pool` |
  | `degenbot::build_erc20_metadata` | `degenbot_bot::bot_core::pool_builder::builder::build_erc20_metadata` |
  | `degenbot::probe_pool_type` | `degenbot_bot::bot_core::pool_builder::builder::probe_pool_type` |
  | `degenbot::resolve_v4_identity` | `degenbot_bot::bot_core::pool_builder::builder::resolve_v4_identity` |
  | `degenbot::PoolBuilderError` | `degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError` |
  | `degenbot::PoolFamily` | `degenbot_bot::bot_core::pool_builder::builder::PoolFamily` |
  | `degenbot::V4BuildResult` | `degenbot_bot::bot_core::pool_builder::builder::V4BuildResult` |
  | `degenbot::V4PoolBuildIdentity` | `degenbot_bot::bot_core::pool_builder::builder::V4PoolBuildIdentity` |
  | `degenbot::construct_pool` | `degenbot_bot::Bot::construct_pool` over `PoolConstruction` (the free function is `pub(crate)`) |
  | `degenbot::RequestedPool` | `Bot::construct_pool` over `PoolConstruction` (the interior request is `pub(crate)`) |
  | `degenbot::ConstructionRoute` | `degenbot_bot::bot_core::pool_builder::route::ConstructionRoute` (the door verb's plan argument) |
  | `degenbot::V3RouteInputs` | `degenbot_bot::bot_core::pool_builder::route::V3RouteInputs` (the interior is `pub(crate)`) |

- **ADR-084 D2b (`test-harness`):** `degenbot_simulation::harness` — the
  executor-grammar lab (43 items, no production consumer) — is behind the
  non-default `test-harness` feature. The shipped crate root is the engine
  (`sim`), the fixture driver (`oracle`), and the capture generator
  (`capture`), so `degenbot-arbitrage`, `degenbot-batch-executor`,
  `degenbot-strategy`, `degenbot-submission`, the umbrella, the cdylib, and the
  console no longer compile it. Sources are unchanged in place (the tier-3
  artifact drift guard hashes files, not the API, and still runs in the
  harness-free default build).

  | Was | Is |
  | --- | --- |
  | `degenbot_simulation::harness` (default features) | `degenbot_simulation::harness` (feature `test-harness`) |

- **ADR-084 capability-surface cut (W2 - the solve door):** the
  `degenbot-pools` solve tree is curated behind intent-named doors. The
  published-path removals, with their replacements:

  | Was | Is |
  | --- | --- |
  | `degenbot_pools::solve::mixed::solve_path` | `degenbot_pools::solve::solve_path` (umbrella: `degenbot::solvers::solve_path`) |
  | `degenbot_pools::solve::mixed::solve_path_with_min_profit` | `degenbot_pools::solve::solve_path_with_min_profit` |
  | `degenbot_pools::solve::mixed::solve::SolveOutcome` | `degenbot_pools::solve::SolveOutcome` |
  | `degenbot_pools::solve::mixed::{ResolvedMixedPath, ResolvedHop, SolvePathResult, HopType, MixedPoolRef, PoolHop, MixedPath, SolveArmOutcome, UnsupportedHopIdentity, V3SnapshotData, V4SnapshotData, INT128_MAX}` | the same names at `degenbot_pools::solve::*` |
  | `degenbot_pools::solve::mixed::{BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState, SolidlyHopState}` | the same names at `degenbot_pools::solve::*` (homes unchanged in `families`) |
  | `degenbot_pools::solve::mixed::simulate_solidly_path` | `degenbot_pools::solve::simulate_solidly_path` |
  | `degenbot_pools::solve::profit_envelope::{GateDeps, Envelope, GateStats, GateSkipCause, DegeneratePathSink, PrefixCache, HopMath, Line, PathBoundLines, path_*_bound*}` | `degenbot_pools::solve::gate::*` (the door module; the numeric kernel, line derivation, and capture serializer stay internal to the `profit_envelope` home) |
  | `degenbot_pools::solve::affected_keys::AffectedKey` | `degenbot_pools::affected::AffectedKey` (moved out of the solve tree) |
  | `degenbot_pools::solve::capture_fixture::read_fixture` | `degenbot_pools::solve::fixtures::read_fixture` (feature `capture-fixtures`) |

  Everything else under `solve/` (the mobius kernels, `bounded_brent`, the
  intake internals) is now `pub(crate)`: no external crate can name it. The
  door's external naming surface is pinned by
  `architecture_gates.rs::solve_door_census_is_the_external_surface` over
  `rust/crates/facade/degenbot/tests/solve_door_census.txt`.

- **ADR-084 W5 (pools namespacing, phase 1):** the 34 root modules of
  `degenbot-pools` moved under the four capability modules. The was-to-is
  rule is uniform: every `degenbot_pools::<module>` path (and every
  `crate::<module>` path inside the crate) gained its capability segment -
  `state::` for the per-family value structs, storage slots, tick map/bitmap,
  spec bounds, journal data, and family-capability declarations; `sim::` for
  the simulators, `PoolEntry`/registry, seam traits, and registration
  refusals; `families::` for the event plumbing (`buffered_event`,
  `liquidity_event`, `liquidity_event_buffer`, `state_update`); `solve::` for
  `affected`. The crate root keeps the umbrella-grade re-exports (`TickInfo`,
  the storage-slot encoders, the seam traits, `CurveInputsError`, the
  buffered-event/`PoolStateUpdate` types) so a flat import continues to work
  for the major types; module-qualified paths must add the segment. The root
  `pub mod` surface is 4, pinned by `pools_root_surface_is_pinned`.

- **ADR-084 D3 (W3 - `capture-fixtures`):** `zstd` (the C-build fixture
  dependency) is optional in `degenbot-pools` behind the non-default
  `capture-fixtures` feature; the default dependency tree of every crate -
  including `degenbot-execution`, `degenbot-rpc`, and the other seam crates -
  compiles no C library for fixtures. Test lanes enable
  `degenbot-pools/capture-fixtures` and `degenbot-bot/executor-ab-probe`
  explicitly, so the golden corpora stay executed. The dependency charter
  (`just check-dependency-charter`, ratchet #7) denies `pyo3` (outside the
  binding shell) and `zstd`/`zstd-sys` (default features) workspace-wide.

- **ADR-084 D4 (W4 - capability layers):** `degenbot-ingestion`,
  `degenbot-runs`, and `degenbot-pool-updater` moved from
  `crates/integrations/` to `crates/foundation/` (pure `git mv`; crate names
  and APIs unchanged). Zero upward role edges remain, enforced by
  `role_edge_direction_is_enforced`.

- **degenbot-pathfinding:** the core now floors the effective minimum depth at a
  per-depth pool-kind filter's length (`effective_min_depth = max(min_depth,
  filter.len())` in `BundledSearch::with_params`). Per policy D1, a filter of
  length N pins an exactly-N-hop permutation regardless of kind constraints —
  all-`None` filters included. This is an enumeration-semantics change for
  direct callers of `find_paths` / `find_paths_iter` / `OwnedPathFinder::new`
  passing `min_depth < filter.len()`: walks shorter than the filter that
  prefix-matched its early depths no longer leak through. Callers via the
  Python driver are unaffected — the plan layer (`prepare_traversal_plan`)
  already applied the same floor, and re-applying it is an idempotent `max()`.
  The two benches that passed a redundant all-`None` filter
  (`pathfinding_sweep/grid_4x4_d5`, `pathfinding_snapshot/native_min2_emd3`)
  now pass `max_depth` alone (their paired `node_valid_depths` tables dropped
  with the filters) and enumerate exactly the path sets they enumerated before.

- **The fee ring's three write surfaces are documented without an API change:**
  one writer home (`Dispatcher::record_priority_fees`) is reached by the session
  consumer's advisory fire-and-forget spawn
  (`degenbot_batch_executor::session`, the one live in-tree caller), by the
  public FFI `record_priority_fees` pymethod as the driver-facing recording
  door, and by the settlement example's `fetch_priority_fees` /
  `record_fee_history`, which stay uncalled by design as the row-17
  standalone-RPC reach proofs.

- **The sim divergence tally is a value; the process-global stays the
  production default — a Rust-API break.** `DivergenceTallyHandle`
  (`degenbot_simulation::sim::divergence_probe`) is a caller-owned accumulator.
  The two observation arms (`observe_storage_read` /
  `observe_storage_read_forced`, which every `BotStateDb` storage read drives)
  take `Option<&DivergenceTallyHandle>`; `BotStateDb::with_divergence_tally` /
  `with_divergence_tally_opt` point one wrapper at a handle beside
  `with_divergence_probe`, and `Evm2BlockSimHandle::build` threads one optional
  handle into both stacks it constructs (the block DB and the frame-replay
  scratch). Handing none — every production call site — accumulates into the
  process-global tally that `divergence_tally_snapshot` /
  `dump_divergence_summary` still read. A caller that asserts absolute counts
  now owns them as a value, which retires the shipped `TALLY_TEST_GUARD`
  convention: the six lib tests whose reads reach a tracked slot no longer
  serialize against each other, and no future test can forget to join them.

### Removed

- **Direct Verify lifecycle configuration: `Bot::set_verify_rpc_url` /
  `Bot::set_verify_state_view` are deleted — every caller configures
  `VerifyLifecycle` directly through its `config()`/`configure()` door.**
  `degenbot_bot::bot_core::build_verify_provider` becomes the public
  provider-construction door and returns `Result<AlloyProvider, ProviderError>`
  instead of `Option`: a refused construction — unsupported transport,
  malformed URL, or a failed build-time WS/IPC connect — fails at the
  configuration call instead of silently leaving the verify home
  unconfigured (the registration missing-provider precondition remains the
  backstop for a home nobody configured). The Python pymethod surfaces
  (`Bot.set_verify_rpc_url` / `set_verify_state_view` and the `ArbitrageEngine`
  twins) keep their shape but
  fail loud at the adapter: a missing engine raises `RuntimeError`, a provider
  construction failure raises immediately with the configuration unchanged,
  and a malformed `StateView` raises `ValueError` with the configuration
  unchanged — an invalid address never becomes the zero address. Pure Rust
  callers await provider construction (the settlement example's nested
  `block_on` hazard is gone); the pymethod bodies own the GIL detachment and
  blocking. The retry-merge behavior (`config()` -> set one field ->
  `configure()`) and the connect-then-configure ritual are preserved.

  | Was | Is |
  | --- | --- |
  | `degenbot_bot::bot_core::Bot::set_verify_rpc_url(Option<&EngineDriver>, &str)` | deleted — `bot_core::build_verify_provider(rpc_url).await` + `VerifyLifecycle::configure` over a `config()` merge |
  | `degenbot_bot::bot_core::Bot::set_verify_state_view(Option<&EngineDriver>, &str)` (a parse failure silently became `Address::ZERO`) | deleted — parse at the seam (`ValueError` on malformed) + `VerifyLifecycle::configure` |
  | `build_verify_provider -> Option<AlloyProvider>`, module-private | `degenbot_bot::bot_core::build_verify_provider -> Result<AlloyProvider, ProviderError>` (public; umbrella `degenbot::bot_core`) |
  | the no-op unit test `set_verify_config_noops_without_an_attached_driver` | deleted with the statics; the fail-loud error paths are tested at the PyO3 adapter |

- **`degenbot_simulation::sim::divergence_probe::reset_divergence_tally`**
  (umbrella-reachable as `degenbot::simulation::divergence_probe`): the
  test-only drain of the process-global divergence tally. A test that needs
  absolute counts constructs a `DivergenceTallyHandle` and hands it to
  `BotStateDb::with_divergence_tally` instead; production never reset the
  tally between assertions. A breaking public-Rust-API deletion under the
  standing hard-cutover rule (no compatibility layer) with zero in-tree
  consumers outside the module; the `pub(crate)` `TALLY_TEST_GUARD` mutex and
  its comment go with it.

### Migration notes

The native-token identity (ADR-097), the core account read-through, and the
core batch construction cutover change several public surfaces at once. One
table, was → is:

| Was | Is |
| --- | --- |
| Python `EtherPlaceholder` (an `Erc20Token` subclass registered at the all-Es / zero sentinel, metadata `"Ether Placeholder"/"ETH"/18`) | deleted with no alias — the conceptual `NativeToken` (FFI `degenbot._ffi.NativeToken`, Python `degenbot.NativeToken`), one per chain, **no address property**; metadata from the core chain-facts manifest (`degenbot_core::chain_facts`, Ethereum → `Ether`/`ETH`/18) |
| Raw sentinel strings/addresses accepted as generic token input (`get_token` / `register_token` / `build_erc20_token` / `Erc20Builder.build*`) | refused loudly at every generic entry point (FFI `ValueError`, builder `DegenbotValueError`): a sentinel is one protocol's wire spelling, never a token contract address. Protocol call sites translate their own spelling (Curve all-Es ↔ `NativeToken` at construction/coin projection; V4 `address(0)` ↔ `NativeToken` at `PoolManager`/`PoolKey`/pool-id/execution and the identity getters) |
| Python token-object account caches (`Erc20Token._cached_balance` / `_cached_approval` / `_cached_total_supply`) | session live state: `degenbot_substrate::account_cache::AccountCache` on `BotState` (per-block depth-8 FIFO stream per `(token, owner)` balance, per token total supply, and per `(token, owner, spender)` approval — bounded, so an approval stream cannot grow for the session's lifetime). The `Erc20Token.get_cached_*` / `set_cached_*` accessors remain as thin adapters over the same store (FFI `Erc20Token.get_cached_*` / `set_cached_*`) |
| Python account-query block coercion (`erc20_builder._resolve_block_number`, which mapped every string tag to the head) | deleted — `Bot.fetch_token_balance` / `fetch_token_allowance` / `fetch_token_total_supply` resolve the typed block selector at the seam that owns the RPC clock (`earliest` → 0, `latest` → head, `pending` → head + 1), and the **resolved block is the cache key and the read's block** |
| Python `Erc20Builder.build_many` batch DB/network/fallback/registration choreography (and its `io=` parameter) | deleted — one core implementation, two call shapes: `build_erc20_metadata` / `build_erc20_metadata_batch` (`degenbot_bot::bot_core`), surfaced as `Bot.build_erc20_token(s)` (FFI `build_erc20_tokens` returns one `(handle, error)` pair per input, in input order, with per-token failure isolation — a broken token never aborts the batch, and the tokens that resolved stay registered) |
| `PyBotIo.fetch_token_balance` / `fetch_token_allowance` / `fetch_token_total_supply` / `fetch_erc20_metadata_batch` | deleted (ADR-023 A6) — the account reads are the core read-through above, and the metadata batch is the core batch construction; the pure-Rust `degenbot_bot::bot_core::fetch_erc20_metadata_batch` and the stateless `degenbot_rpc::erc20::{read, read_batch, balance_of, balance_of_batch, allowance, total_supply}` remain the core capabilities |
| Public token results typed `Erc20Token` only (pool token getters, curve coins, construction outcomes, account queries) | the `Erc20Token | NativeToken` union: FFI `get_token0` / `get_token1` / `get_tokens` / `get_tokens_underlying` return either handle (Python `wrap_token_handle` wraps the union), `Erc20Builder`/`Bot` account queries take the union, and the conceptual native token participates in session identity (`TokenIdentity::Native`) and account caching exactly like an ERC-20 |
| `degenbot._ffi.SessionObject.address: str` and `degenbot_substrate::TokenIdentity::address() -> Address` | `address` is `Optional[str]` (`None` for a native-token object) and `TokenIdentity::address() -> Option<Address>` — the native token has no protocol address of its own; `SessionObject.family` reports `"native"` for it |
| `BuildPoolRequest.state_cache_depth` / `BuildManagedPoolRequest.state_cache_depth` (the old ADR-005 companion cache-depth option) | removed with no alias or ignored-kwarg shim — the account streams' fixed depth-8 FIFO is core-owned (`degenbot_substrate::account_cache`) and supersedes the option; the `Erc20Token` cache accessors keep their same signatures over that store |
