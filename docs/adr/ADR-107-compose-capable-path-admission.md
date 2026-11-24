# ADR-107: Compose-capable path admission — execution-only registry, target-relative execution

**Status: accepted (implemented).** (2026-11-24; ergo tasks `XUGAFQ`,
`AWOBBU`, `HCRBX7` shipped the rule; certification `EIXMQP` measured every
mandatory leg — admission, solve, inline sim, crawl build/verify, and the
all-targets-declined walk — over one corpus on both binaries.) Extends ADR-059's family capability
declarations and ADR-098's target-relative execution model. Supersedes the
downstream-only observability stance established by `be29986f1`; it does not
alter ADR-090's hook execution policy or ADR-099's release commitments. The
implemented shape and the certification replay are recorded in
[Implemented shape and certification](#implemented-shape-and-certification-2026-11-24);
the census above is the PRE-cutover state kept as provenance.

## Context

Settlement registration currently admits paths whose families can participate
in discovery and mixed solving but cannot be projected into command-stream
`PathInfo`. Solidly, Balancer, and Curve paths can therefore occupy the
registry, be solved every cycle, and then disappear during assembly or inline
simulation. Commit `be29986f1` made that waste visible as
`SimDegradeReason::NoEncoderArm`, but deliberately left admission unchanged.

The existing capability table already declares these families as solve-only
for `Arm::Settlement`: discovery and admission are present, composition is
absent. The registration seam nevertheless validates only path structure,
pool identity, routability, and capacity. This creates a class of candidate
that is valid to the registry but permanently unable to produce work for any
execution consumer.

A second boundary is equally important. ADR-098 and the glossary define target
selection as ordered first-compatible selection or explicit all-target fan-out.
A path accepted by one deployment and declined by another is not globally
executable or globally unexecutable. Actual composition also depends on facts
unavailable at registration: solved amounts, target capabilities, capital
policy, command limits, calldata validation, hook behavior, and chain state.

The design must remove permanent waste without pretending registration can
prove successful execution, creating a second simulation-only product, or
coupling the engine registry to session-specific deployment state.

The census that follows records every current drop point with its code anchor,
the family-by-arm matrix, the static-versus-dynamic capability split, and the
pre-cutover measurement that the design must leave unchanged or improve.

## Current-state census

Admission has one live guard, and it reads no capability fact. `register_path`
(`degenbot-bot/src/arb_engine/solve_cycle/mod.rs:1334`) checks arity, signature
dedup, the registered-path cap, pool presence, and structural routability;
derives each hop's `HopType` from whatever pool it finds (`derive_hop_type`,
`:1282`); commits identity (`:1413`); and records a state-derived eligibility row
(`PathSolveStatus::resolved`, `:1419`). `register_and_solve_path` (`:1488`)
then solves the fresh path immediately. Nothing there reads the ADR-059 table,
the encoder-family roster, or the executor's shape rules, so the registry's
admission class is strictly broader than the composition class.

| # | Where work is dropped | Class | Evidence |
| --- | --- | --- | --- |
| C1 | Direct registration and eager solve of a solve-only family | unsupported capability, permanent | `derive_hop_type` derives `SolidlyStable` / `BalancerWeighted` / `BalancerStable` / `CurveStableswap` (`solve_cycle/mod.rs:1282`); `arb_engine/tests/solidly.rs:151` registers through `register_path` and solves an all-Solidly route |
| C2 | Encoder projection refuses the hop | unsupported capability, encoder arm | `PathInfoBuildError::UnsupportedHopType` (`degenbot-substrate/src/path_info.rs:136`); unit test `unsupported_hop_type_is_refused_by_the_encoder_projection` |
| C3 | Inline sim produces no payload | named, but downstream and permanent | `SimDegradeReason::NoEncoderArm` (`arb_engine/inline_sim.rs:311`), classified from hop families only (`of_hops`, `:337`), counted at `degenbot_sim_degrades_total{reason="no_encoder_arm"}` (`:424`) |
| C4 | Whole-route shape the encoder has no producer for | unsupported composition over SUPPORTED families, currently misattributed | `facts_for` declines every non-all-V2 route outside 2-3 hops (`grammar_walker.rs:813`, `:841`), while `of_hops` reads only family `compose` flags, so this degrade lands in `hook_refused` |
| C5 | Mixed-family route the solver refuses after registration | unsupported composition, solver arm | `FAMILY_DISPATCH` accept rows plus one refusal row per solve-only family (`degenbot-pools/src/solve/mixed/solve.rs:248`); `arb_engine/tests/solidly.rs:331` registers a Solidly+V3 route and asserts a `None` solve result |
| C6 | Apply-time relevance silently omits the path | unsupported capability, silent | `session_paths` filters every registered path whose projection fails (`arb_engine/path_info.rs:57`) |
| C7 | Crawl discovery refuses the family | unsupported capability, as a SWEEP-level error | `CrawlDiscoveryError::UnsupportedHopKind` (`crawl/discovery.rs:348`); the graph vocabulary is `PoolKind::{V2,V3,V4}` only (`degenbot-pathfinding/src/graph.rs:72`), so the arm is unreachable from the DB graph today |
| C8 | Construction refuses the pool | infrastructure incapability, NOT composition | `ConstructionRefusal::UnsupportedFamily` (`bot_core/pool_builder/route.rs:96`, ADR-055 D4 loud abort), classified at `crawl/admission.rs:1123` |
| C9 | Target walk declines the candidate | target-relative executability | `select_first_compatible` (`degenbot-arbitrage/src/selection.rs:211`) stores `PreparedEncoding::Declined` (`:112`) per examined candidate; an all-declined path has no outcome of its own |
| C10 | Submission | no capability class exists here | `SubmissionTarget` and `SkipReason` (`degenbot-submission/src/submit.rs:131`, `:163`) are reached only by an already-composed candidate |

Three facts follow, and each one narrows the decision:

- C1, C2, C3, C5, and C6 are one defect seen at five depths: the engine
  registers a path it can never compose. Only C3 names it, and only after the
  path has been solved and every cycle's solve budget has been spent.
- C4 is a different defect. Every hop family declares `compose = true`, yet the
  route has no producer, so a family-level roster check alone cannot make
  `no_encoder_arm` unreachable. Admission must also hold the encoder's
  whole-route shape invariant, or the retirement in D8 would be premature.
- C8 is not this class at all. A pool the builder cannot construct and a path
  the command stream cannot express fail at different doors with different
  remedies; folding one into the other would hide a build gap behind a
  capability refusal.

The write door already enforces one encoder-representability fact with a typed
refusal: `RegisterV4PoolError::FeeExceedsEncoderLimit`
(`degenbot-pools/src/state/v4_state.rs:286`, constant `V4_FEE_ENCODER_MAX` in
`degenbot-executor/src/encoders.rs:67`). That is the shape this ADR follows: an
encoder fact refused at admission by a typed error, with no second registry.

### Resolve misses and quarantine

The census's skip taxonomy has further drop points that are not capability
facts, and the admission rule must not absorb them.

| Class | Where it is decided | Why it is not unsupported capability |
| --- | --- | --- |
| resolve miss | `MissingHopReason` (`degenbot-substrate/src/resolve/mod.rs:288`), one `HopDeficit` per failing hop | Six variants are state facts (`MissingState`, `MissingIdentity`, `MissingTokenPair`, `SequenceUnavailable`, `InvariantError`, `NotViable`). All are per-block and clearable: the path stays registered and becomes `PathSolveStatus::Invalid { responsible }` (`arb_engine/path_lifecycle.rs:121`, `:145`), which re-resolves when a responsible pool goes dirty |
| structural unroutability | `MissingHopReason::is_structurally_unroutable` (`:367`): `TooFewTokens` / `UnknownVariant` / `OutOfRange` | The only resolve class that already refuses registration (`PathRegistrationError::Invalid`). A malformed-path fact, not a capability fact |
| quarantine | ADR-040 pool quarantine, `MissingHopReason::Quarantined` (`:308`) | Deliberately exempt from `is_structurally_unroutable` because `release_pool` re-admits the pool. A tainted-surface policy: the pool's family and the arm's capability did not change |
| hook fee measurement | `HookFeeUnmeasured` / `HookFeeRefused` (`:320`, `:324`) | Both are clearable by the next measurement, which bumps the pool's state nonce and re-walks the hop |

The taxonomy already separates these classes without help from admission:
`HopRefusal::{Abort, Skip}` at construction (`crawl/admission.rs:1051`), the
`BuildFailure` stable-versus-transient split behind the ledger, and
`PathAdmission::{RejectedPathMemo, PolicyDenied, Admitted}` at the eligibility
gate (`registration_ledger.rs:991`). Admission adds one capability class beside
them; it reclassifies none of them.

Two reasons force that boundary. The admission rule must stay *permanent* to be
memo-safe, and D4 keys its memo on the hop signature — a state fact is not a
deterministic property of that signature, so a capability refusal would have to
be revoked later, which is exactly the runtime invalidation loop D4 forbids. The
reverse error matters as much: a guard that read "invalid now" as "unsupported
forever" would drop a path at every quarantine and never restore it, while
quarantine already has its own release path and its own depth gauge
(`degenbot.engine.quarantined_pools`, `instruments.rs:838`). A permanently
unsolvable-but-supported path is a real residual, but its remedy is pool state,
verify, or quarantine policy — never a fabricated capability refusal.

### Cited failures, and what each one moved

| Commit | What it established | What remains after it |
| --- | --- | --- |
| `be29986f1` | The unsimulatable path is named and counted (`no_encoder_arm`) | Named downstream only; registry, construction, and solve cost are still spent (C1, C3) |
| `5d14ef5dc` | Unsupported-family solve verdicts are typed (`DiagnosticPoolState::Unsupported`, `arb_engine/diagnostic.rs:196`) | The typed verdict is diagnostic output; nothing consumes it at admission |
| `98198cade` | Broken pools are classified by capability at the builder (`BuildFailure` taxonomy) | A build refusal is a pool fact; the route's capability is still unasked (C8) |
| `2fa9fca5e` | Submit joins require path identity (`degenbot-batch-executor/src/stage.rs:594`) | The join guard proves the dependency the admission guard must own |
| `59da1dcb6` | V4 reverts are attributed by pool id | Attribution improves after composition exists; unsupported paths never reach it |
| `fcb1ccaa2` | Init-hash provenance is typed; no guessed identity on a real chain | Identity is not capability: a correctly identified pool can still be unencodable |
| `aeb4877ce` | Real Solidly hop identities are projected | Honest identities are what make the admission refusal reportable (C1) |
| `32e08c94c` | Mixed-path math failures propagate as typed errors | A math failure is distinct from an absent engine; the refusal rows cover the latter (C5) |

### Probe evidence

Three scoped, read-only probes ran in this environment against baseline
`d1bfb7bb0`. They are cited as evidence, not as a test run for this document
change.

| Probe | Command | Result |
| --- | --- | --- |
| encoder projection refuses a solve-only hop (C2) | `just test-rust-nextest -p degenbot-substrate -E 'test(unsupported_hop_type_is_refused_by_the_encoder_projection)'` | exit 0, 1 test passed |
| capability roster declares the settlement compose split (C1, matrix) | `just test-rust-nextest -p degenbot-strategy -E 'binary(family_capabilities)'` | exit 0, 4 tests passed, including `hop_type_solve_coverage_and_settlement_compose_match_the_declaration` |
| solve-only registration plus mixed-route solve refusal (C1, C5) | `just test-rust-nextest -p degenbot-bot -E 'test(solve_solidly_2hop_all_solidly_matches_grid_scan) or test(solve_solidly_plus_concentrated_liquidity_path_rejected_by_scope)'` | BLOCKED, exit 101: the `degenbot-bot` lib-test link fails in this environment (`rust-lld: undefined hidden symbol`, reproducible on retry), independent of this document. Both tests are cited by source at `arb_engine/tests/solidly.rs:151` and `:331` |

## Capability matrix

One row per settlement row of the ADR-059 table. The solve arm is the mixed
solver's `HopType` dispatch; the encoder arm is the command-stream `HopInfo`
arm plus the whole-route shape key; simulation covers both the worker-inline
hook and the fan-out batch, because both project a `PathInfo` before they can
simulate anything.

| Family (settlement) | Solve arm | Encoder arm | Simulation arms | Submission target | Chain / deployment dependence |
| --- | --- | --- | --- | --- | --- |
| V2 | yes | `HopInfo::V2`, all-V2 any-N | inline and fan-out | any of `Bundle` / `Public` / `BuilderRelay` once composed | pool rows per chain; capability chain-independent |
| V3 | yes | `HopInfo::V3`, 2-3 hop mixes | inline and fan-out | same | fork slot layout per chain |
| V4 | yes | `HopInfo::V4`, 2-3 hop mixes | inline and fan-out | same | `PoolManager` is a session fact; every V4 hop must match it (`grammar_walker.rs:813`) |
| SolidlyStable (Aerodrome / Camelot stable) | yes | none | none, `UnsupportedHopType` | unreachable | capability chain-independent; math chain-agnostic |
| BalancerWeighted | yes | none | none | unreachable | Vault is a session fact, never reached |
| BalancerStable | yes | none | none | unreachable | same |
| CurveStableswap | yes | none | none | unreachable | pool addresses per chain, never reached |
| LfjBinned | no | none | none | unreachable | roadmap-only (ADR-059 D8) |

How to read it:

- Seven settlement families solve; three compose. Four of seven therefore
  register, solve every dirty cycle, and can never be assembled or submitted.
- Submission venue is orthogonal to the pool family. `SubmissionTarget` is
  session wiring over an already-composed candidate and contributes no
  capability dimension, so the venue column is constant per family. It would
  gain content only if a venue constrained path shape, and none does: the venue
  sees a signed transaction, not a route.
- Chain and deployment decide *which target accepts* a compose-capable route,
  never whether the arm has an encoder. Two facts on that axis are still static
  enough for admission: a V4 pool whose static fee reaches `V4_FEE_ENCODER_MAX`
  (already refused at pool registration; see the write-door precedent in the
  census), and a V4 hop whose `PoolManager`
  disagrees with the session's, which belongs to the candidate because the
  session supplies it.
- The simulation arms are not an independent axis. Both project the same
  `PathInfo` (`degenbot-substrate/src/path_info.rs`), so "can be simulated"
  and "can be encoded" share one representation; the fan-out additionally joins
  results by path identity (`stage.rs:594`). A simulation-only family would
  therefore need a second representation, which is exactly the product surface
  D1 refuses without a consumer.

## Static and dynamic capabilities

| Fact | Kind | Owner | Knowable at | Admission reads it |
| --- | --- | --- | --- | --- |
| tier wiring per `(arm, family)` | static, release-bounded | `degenbot-pools` `TABLE` (`state/capability.rs:391`) | family known | yes, this decision |
| encoder-family membership | static | `ENCODER_FAMILIES` (`:373`, ADR-059 D5) | family known | yes, this decision |
| hop-count class (all-V2 any-N; otherwise 2-3) | static, shape-level | `degenbot-executor` `facts_for` (`grammar_walker.rs:813`) | hop families known | yes, this decision |
| static V4 fee bound | static, per pool identity | `V4_FEE_ENCODER_MAX` (`encoders.rs:67`) | pool identity known | already refused at pool registration (write-door precedent) |
| mixed-solver composition rows | static, per family set | `FAMILY_DISPATCH` (`solve/mixed/solve.rs:248`) | hop families known | yes, this decision |
| pool-state resolvability | dynamic, per block | `resolve_hops` deficits (`solve_cycle/mod.rs:1390`) | solve | no, a state fact with a solve outcome |
| V4 `PoolManager` agreement | dynamic, per target | `facts_for` (`grammar_walker.rs:813`) | composition | no, target-relative |
| target adapter acceptance | dynamic, per target | `ComposeError::Unsupported` (`degenbot-execution/src/payload.rs:122`) | composition | no, target-relative |
| capital policy | dynamic, per session | `ComposeError::InsolventZeroCapital` | composition | no |
| hook behaviour, declared-interface validation, revert, gas | dynamic, per block | sim verdicts, `ComposeError::Validation` | simulation | no |
| transport, nonce, venue refusal | dynamic, per submission | `SkipReason` | submit | no |

The dividing line the census supports is not "cheap versus expensive"; it is
**amount-independent and target-independent**. The five static facts change only
with the binary or the route's identity, so refusing on them is deterministic
and memo-safe. Everything below stays with its existing typed owner, because a
registration-time answer would be a guess about a block, a session, or a
deployment the engine cannot see.

### ADR-099 commitments, and the question they answer

ADR-099 answers a different question, at a different time, for a different
subject. Its `capabilities_hash()` commitment proves that a *deployed executor
contract* declares the release's capability set: binding reads four metadata
getters, resolves the release tag in the bounded registry, and refuses a
disagreeing commitment before anything is encoded
(`degenbot-arbitrage/src/release_binding.rs:392`, ADR-099 D6). The per-release
requirement list sits beside it (`Release::requires`,
`degenbot-executor/src/release_registry.rs:66`; `Release::missing_capabilities`,
`:259`).

The commitment is consumed at target selection rather than at registration, for
three structural reasons:

1. **It is target-keyed.** Registration has no deployment, no release tag, and
   no bound contract to ask; the commitment is a network-read fact about one
   address.
2. **Its requirement set exists only after encoding.** The check compares the
   encoded call's declared requirements against the release
   (`degenbot-arbitrage/src/cmd_executor.rs:447`), and those requirements are
   produced by encoding a real solved route. Registration has no amounts, so it
   has no call and no requirement set.
3. **Its refusal is a routine target decline by construction.** A missing
   capability returns `ComposeError::Unsupported` so the selection walk advances
   to the next target; feeding that verdict back into registration would let a
   session's deployment list mutate engine standing (D6).

It cannot replace the registration guard either. A capability ID names a
contract feature — opcodes, an address table, ERC-6909 claims — never a route
shape, so no commitment can say "4-hop mixed V2/V3 has no producer". ADR-099 D8
keeps a metadata-free custom executor first class, so demanding the commitment at
registration would either exclude such adapters or require metadata the generic
seam deliberately does not carry (ADR-098 D5). Nor is it a parallel registry: it
is one immutable deployment's declaration of already-shipped opcodes, not a
second roster of arms and families.

The three layers therefore stay ordered and non-overlapping: static arm and route
capability at admission (this ADR), release declaration proof at binding and
target selection (ADR-098, ADR-099), and behaviour at simulation (ADR-098 D3 and
ADR-099 D8: declared capability is never implemented capability).

## Pre-cutover baseline

The measurement is defined over instruments that already exist, so capturing the
pre-cutover number needs no new series.

| Quantity | Instrument | Code provenance |
| --- | --- | --- |
| degraded inline candidates by class | `degenbot_sim_degrades_total{reason}` | `inline_sim_degrades` (`instruments.rs:189`), census `InlineSimDegraded` |
| inline-arm candidate denominator | `degenbot_candidates_found_total{arm="inline"}` | `candidates_found` (`:183`), census `CandidatesEntered` |
| registration skips by bounded reason | `degenbot_registration_skips_total{reason}` | `registration_skips` (`:248`), `RegistrationOutcome` tags (`registration_ledger.rs:124`) |
| registered-path count | `degenbot_engine_registered_paths` | `registered_paths` (`:163`) |
| per-path solve cost | `degenbot_solve_path_duration_seconds` | `per_path_solve_duration` (`:155`) |
| per-cycle solve cost | `degenbot_solve_duration_seconds{arm}` | `solve_duration` (`:152`) |

The pre-cutover rate is

```text
increase(degenbot_sim_degrades_total{reason="no_encoder_arm"}[1h])
  / increase(degenbot_candidates_found_total{arm="inline"}[1h])
```

paired over the same window with the per-path solve p50
(`histogram_quantile(0.5, sum by (le) (rate(degenbot_solve_path_duration_seconds_bucket[1h])))`)
and the registered-path gauge at window start and end.

**Capture status.** The structural component is exact and available offline:
four of seven settlement solver families declare `compose = false` (`TABLE`,
`state/capability.rs:391`, settlement rows `:427-461`), so every registered path
containing one of them spends a full per-path solve on every dirty cycle. For a
path containing a solve-only hop the wasted solve fraction is therefore exactly
1.0. Turning that fraction into a total is an estimate, not a measurement. The
registered path count times the per-path solve p50 assumes (a) the refused
population's per-path cost has the same median as the whole path population and
(b) a median can stand in for a sum, which it does only for a symmetric, narrow
distribution. The exact total is a sum over refused paths, and no current series
carries a family or route-shape label, so it cannot be summed from a scrape; the
cutover replay measures it directly instead.

The live *rate* was not capturable in this scoping environment. No bot and no
metrics process are running: `127.0.0.1:9464` accepts no scrape, and the only
telemetry endpoint reachable is a Jaeger UI at `host.docker.internal:16686`;
both documented service APIs (`/api/services` and `/api/v2/services`) answer 404,
so no service inventory and no traces could be obtained. Neither series exists
without a running bot carrying the dev-features OTel layer.

Recording the rate stays an acceptance item for the cutover certification (D10),
which owns the matched before/after replay. The settled split: this scoping task
closes on the structural baseline, the exact queries, and the capture
limitations recorded above, while the cutover task owns mandatory numeric
samples — the old-binary pre-cutover `no_encoder_arm` rate, registration counts,
and solve cost, then the new-binary counterparts from its matched instrumented
replay — and cannot close without them. The split assigns capture; it does not
lower the requirement, which stays numeric in D10.

Two baseline facts the census adds, neither reported by any current series:

- nothing carries family or route shape, so "how many registered paths contain a
  solve-only hop" cannot be read from a scrape today. That is the gap the
  bounded registration reason closes (D9); until then the count is a diagnostic
  hop walk over the registry, not a metric.
- `hook_refused` currently absorbs whole-route shape declines over fully
  supported families (C4), so the pre-cutover `no_encoder_arm` rate
  *undercounts* permanent incapability by exactly that population.

### Cutover effects

- **Discovery ranking**: unchanged. Ranking is computed over the DB path graph
  (`degenbot-pathfinding`), whose vocabulary is V2/V3/V4, and the crawl's
  pool-kind filter never offered these families, so no ranking input moves.
- **Cycle counts**: `degenbot_engine_registered_paths` and the per-cycle
  solved-path counts fall by the refused set. No other counter changes shape.
- **Telemetry**: the degrade classification disappears with its class
  (`...{reason="no_encoder_arm"}`, and the `reason` dimension with it — the one
  bounded outcome class left is the hook's refusal) and one bounded value appears
  (`degenbot_registration_skips_total{reason="unsupported-capability"}`) as a
  new `REGISTRATION_OUTCOMES` member (`registration_ledger.rs:124`) beside
  `V4HookRejected` — a vocabulary member, not a new label dimension.
  `degenbot_sim_degrades_total` keeps its remaining meaning, the hook's own
  refusal: an unregistered pool between solve and dispatch, the u128 projection
  overflow, a target `PoolManager` mismatch, and other genuine hook refusals.
- **Already-registered unsupported paths**: none survive a cutover. The registry
  is process-local identity only, rebuilt at boot, so the new binary admits the
  new rule from its first block: no persisted path state to migrate and no
  in-run reclassification.

## Decision

### D1 — Settlement registration is execution-only

The settlement path registry admits only compose-capable paths. A candidate
whose family or whole-route shape is permanently outside settlement command
composition receives an unsupported-capability outcome instead of entering the
registry or consuming solve budget.

There is no parallel simulation-only registry. Solve-only families remain
representable in taxonomy, discovery, and typed refusal diagnostics; a separate
product surface requires a separate concrete consumer and decision.

This ADR does not add Solidly, Balancer, or Curve encoders. It makes their
current execution ceiling effective at admission.

### D2 — Compose capability and target execution are different facts

The domain progression is:

```text
DiscoveredPath -> ComposeCapablePath -> TargetCandidate
```

A **compose-capable path** satisfies static family and target-independent
whole-route requirements for the relevant execution arm. A **target candidate**
is one compose-capable path offered to one deployment under the session's
target-selection policy.

Registration proves the former, never the latter. The term `VerifiedPath` is
not used: Verify lifecycle already names per-pool state verification.

### D3 — One static evaluator reflects the composition authority

The static evaluator lives in `degenbot-executor`, beside the command
composition rules it reflects, and consumes family declarations from
`degenbot-pools`. `degenbot-bot` orchestrates that evaluator at admission.
There is no new crate, per-path allowlist, or duplicate capability registry.

Evaluation has two fact levels:

- family assessment over the discovered or registered hop taxonomy;
- whole-route assessment over neutral `PathInfo`, limited to invariants that
  do not require solved amounts or a deployment target.

A missing family declaration or an unknown route shape fails closed. Existing
V2/V3/V4 heterogeneous routes need no per-path entries. A new family or a
genuinely new composition requires explicit taxonomy projection, solver and
command support, a capability declaration, and mixed-route acceptance evidence
before its compose declaration can become true.

The whole-route half reads two existing authorities, because the census found
two independent composition facts. The mixed solver's `FAMILY_DISPATCH` rows
(`degenbot-pools/src/solve/mixed/solve.rs:248`) decide which family sets solve
together; the executor's shape key (`grammar_walker.rs:813`) decides which hop
sequences have a command producer.

C4 and C5 prove those checks are independent; neither is the other's inverse.
C4 is a route whose every hop family declares `compose = true` and which still
has no producer, so family-level compose flags cannot answer the encoder-shape
question. C5 is a route that registers and then fails the solver's own
composition row — a refusal that fires in a different crate, with a different
failure vocabulary, before any encoder is consulted. The direction is not
symmetric either: every family set over encoder-representable families
(V2/V3/V4) has a solver composition row, so a static solver refusal can
accompany an absent encoder arm, while an encoder acceptance never accompanies a
static solver refusal. That is a statement about dispatch rows, not about a
route solving: dynamic resolution, directional viability, and the per-family
math can still fail for an individual route. Admission therefore runs both
checks instead of deriving one from the other, and evaluates family compose,
solver composition, and encoder shape together, reading the shape
invariant from `degenbot-executor` — the crate that owns the producer — rather
than restating producer rules in `degenbot-bot`.

### D4 — Admission refuses at the earliest stage with sufficient facts

Crawl discovery evaluates family capability as soon as that family is known,
before pool construction, Verify lifecycle work, registration, or solving.
Post-build crawl and direct registration evaluate whole-route `PathInfo`
before registry commit or eager solve. Both entries use the same evaluator.

The census fixes where that early refusal can fire today. The discovery graph
names only `PoolKind::{V2,V3,V4}` (`degenbot-pathfinding/src/graph.rs:72`), and
`UnsupportedHopKind` (`crawl/discovery.rs:348`) is a sweep-level error rather
than a per-candidate refusal, so no solve-only family reaches the crawl at all.
The pre-build family refusal is therefore not redundant work: it is the rule
that must exist before the graph gains those families, and it must answer per
candidate instead of aborting the pass. Until then the direct `register_path`
seam is the live guard, which is why D1 names it as authoritative.

Stable negative memoization uses the identity available at the proving stage:

- pre-build family refusal memoizes the hop's `PreBuildIdentity` as
  unregistrable;
- post-build whole-route refusal memoizes the constructed `HopSignature` as
  `PathStanding::Rejected`.

Memos live for the process lifetime. A new binary with changed capabilities
creates a fresh ledger. Direct registration does not consume crawl memos and
returns the current typed refusal on every call.

Transient transport, construction, state, and target failures are not stable
capability refusals and are never memoized by this rule.

### D5 — Registration never performs synthetic execution

The admission guard does not invoke an encoder or simulator with fabricated
amounts. Real composition and simulation remain post-solve operations over
actual path results and deployment state.

Consequently, compose capability is a necessary admission condition, not a
promise that actual calldata, capital, hooks, simulation, or submission will
succeed. Those outcomes retain their existing typed downstream classifications.

### D6 — Target declines do not mutate registration

A target-specific composition decline belongs to its target candidate. It does
not deregister the path, revoke arm-level compose capability, or create a
stable negative memo, including when every currently configured target
declines.

The selection pipeline reports a bounded all-targets-declined outcome so
remaining post-solve waste is visible. A persistent disagreement between the
static roster and actual command composition is corrected in the roster,
composition implementation, or their drift evidence, not through a runtime
registration feedback loop.

Ordered first-compatible selection and explicit all-target fan-out are
unchanged.

### D7 — Refusal is structured across both drivers

`PathRegistrationError` gains a dedicated unsupported-capability variant with
structured family, hop, and route-reason data. Invalid identity, transient
construction failure, registry capacity, and target-specific decline remain
distinct outcomes.

Python exposes a corresponding typed exception through the thin PyO3 mapper
and generated stubs. Neither driver matches rendered `ValueError` text to
recover the classification.

### D8 — Cut over once and retire the downstream degrade

The admission rule lands without a feature flag or parallel old/new registry.
Registries are process-local, so restart rebuilds them under the new rule; no
persisted path migration exists.

Once unsupported families cannot reach inline simulation,
`SimDegradeReason::NoEncoderArm` and its now-unreachable handling are retired.
The class existed to name "this path can never be assembled"; admission now
answers that before the path enters the registry, so the inline seam's
`simulate_path` returning `None` has one bounded outcome class — the hook's
refusal — whose dynamic causes are listed in D9. The single-variant classifier, the census field, and the metric's
`reason` dimension are all retired with it; the span keeps
`simulate.error_reason=hook_refused` as diagnostic detail. Admission-refusal
outcomes replace dashboard and operator dependencies on that reason. No
compatibility switch or retirement tripwire remains.

### D9 — Telemetry measures refused work and residual target waste

Before cutover, the scoping task records the `no_encoder_arm` rate —

```text
increase(degenbot_sim_degrades_total{reason="no_encoder_arm"}[1h])
  / increase(degenbot_candidates_found_total{arm="inline"}[1h])
```

— together with the per-path solve-cost histogram and the registered-path gauge
over the same window. The pre-cutover baseline section records what the scoping
environment could capture and what remains for the cutover certification. After
cutover:

- registration reporting gains a bounded `unsupported-capability` reason;
- target selection gains a bounded all-targets-declined outcome;
- family, hop, route, path, pool, and target detail stays in structured logs or
  spans, not unbounded metric labels.

The pipeline census remains the sole event-to-metric authority under ADR-043
and ADR-083. The degrade counter also loses the whole-route shape declines it
absorbed before the cutover (C4): those are admission refusals now, so retiring
`NoEncoderArm` required the shape invariant to land with it rather than after
it.

### D10 — Certification covers admission, parity, and measured savings

Implementation requires deterministic red-first cases and a matched
before/after discovery replay. The replay compares registrations, build and
verify calls, solve counts and timings, and supported-path results.

Acceptance requires:

- each solve-only family and a mixed supported/unsupported route refuses at
  the earliest applicable stage;
- a missing capability declaration fails closed;
- a new route over supported families passes without per-path wiring;
- valid heterogeneous V2/V3/V4 solve and composition results remain identical;
- a broken but supported pool remains a construction/state failure;
- memo replay avoids repeated work while direct registration still returns the
  typed refusal;
- Python exposes the typed exception and regenerated stub;
- target declines do not deregister paths;
- a mixed route over fully supported families but outside the encoder's shape
  key is refused at admission, not counted as `hook_refused`;
- the pre-cutover `no_encoder_arm` rate and solve-cost baseline are recorded
  from a live scrape;
- unsupported paths no longer reach the solver;
- supported-path replay results remain identical.

## Implementation graph

- `XUGAFQ` — static compose-capability guard and typed registration refusal.
- `AWOBBU` — early crawl refusal, stable memos, and admission telemetry; depends on `XUGAFQ`.
- `HCRBX7` — all-targets-declined visibility; independent of the registration implementation.
- `EIXMQP` — hard-cutover certification; depends on `AWOBBU` and `HCRBX7`.

Implementation notes from the census, addressed to those tasks rather than
decided here:

- `XUGAFQ` needs no new registry. `ENCODER_FAMILIES` already states the
  encoder-arm fact, and the shape invariant is one predicate over the same
  `PathInfo` the composer reads.
- `AWOBBU` needs no new standing and no new memo key. `PoolStanding::Unregistrable`
  (`registration_ledger.rs:1140`) already means "no candidate path through this
  pool can register" and is keyed by `PreBuildIdentity`; its
  `UnregistrableRecord` (`:958`) carries a bounded `RegistrationOutcome`, so the
  bounded addition is a vocabulary member, not a new state and not a new label
  dimension.
- `EIXMQP`'s retirement is gated on C4: `SimDegradeReason::of_hops` classifying
  by family alone both undercounts permanent incapability and overcounts
  `hook_refused`.
- `HCRBX7` measures C9's walk. `select_first_compatible` leaves every examined
  candidate `Declined` and retires the unreached, so the all-targets-declined
  outcome is a count over examined candidates, not a new selection policy.

All implementation tasks depend on `W5GKJF` completing the remaining census,
baseline, and ADR review.

## Considered alternatives

**Downstream observability only.** Rejected: naming a degrade does not reclaim
the registry, construction, or solve budget already spent.

**A simulation-only path registry.** Rejected: no current consumer justifies a
second registry and lifecycle. Taxonomy and typed discovery outcomes preserve
the information without preserving the waste.

**Actual encoding during registration.** Rejected: registration lacks real
amounts and target context. Synthetic execution would create misleading proof
and a second execution path.

**Target-aware registration and deregistration feedback.** Rejected: deployment
capabilities and selection policies are session-relative. The engine registry
would otherwise acquire downstream mutable state and contradictory standing.

**A family-only capability guard.** Rejected by C4: every hop family can
declare `compose = true` while the route has no producer, so a roster check
alone would leave `no_encoder_arm` reachable and make its retirement false.

**Evaluation in `degenbot-pools`.** Rejected for whole-route rules: pools owns
family taxonomy and declarations, not command-stream composition details.
`degenbot-executor` owns those rules; bot owns admission choreography.

**A feature-flagged dual registry.** Rejected: the old behavior has no product
consumer, and a permanent parallel path would preserve the ambiguity this
decision removes.

## Consequences

Operators see unsupported families as early, typed registration outcomes rather
than candidates that vanish after solving. Registration and cycle counts become
execution-oriented. Direct Python registration changes from generic refusal or
late disappearance to a typed unsupported-capability exception.

Compose-capable paths may still be solved and declined by every target. This is
intentional target-relative behavior and is measured rather than hidden. A
material residual motivates better declarations or target policy, not implicit
registry mutation.

Adding a route over existing supported families requires no admission wiring.
Adding a family remains explicit end-to-end work; a boolean declaration without
solver, command, and mixed-route evidence is insufficient.

## Implemented shape and certification (2026-11-24)

The decision landed without a flag, in four commits and one certification task:
`XUGAFQ` (the static evaluator + typed registration refusal), `AWOBBU` (early
crawl refusal, stable memos, admission telemetry), `HCRBX7` (all-targets-declined
visibility), `EIXMQP` (this certification: the degrade retirement, the operator
documentation, and the matched replay).

The implemented seams:

- `degenbot-executor::compose_capability` — `assess_families` (the ADR-059
  declaration, fail-closed on a missing row) and `assess_route` (the mixed
  solver's family-set row, then the encoder's whole-route shape key). The doors
  call those two fact levels at the stage that first holds their inputs — the
  family level pre-build in the crawl and at registration, the route level
  post-build in the crawl and at registration — so the shared pieces are the two
  evaluations, not one combined entry point.
- `degenbot-bot` admission — `register_path` evaluates the evaluator before
  registry commit and eager solve and returns
  `PathRegistrationError::UnsupportedCapability(ComposeCapabilityRefusal)`;
  the crawl answers the same refusal pre-build (memoized per `PreBuildIdentity`)
  and post-build (memoized per hop signature).
- Telemetry — `degenbot_registration_skips_total{reason="unsupported-capability"}`
  as a bounded `REGISTRATION_OUTCOMES` member, and
  `degenbot_selection_all_targets_declined_total{class}` emitted once per path
  whose every offered target declined.
- Retirement — `SimDegradeReason` (and `of_hops`), the census event's class
  field, and the degrade counter's `reason` dimension are gone. The inline seam
  keeps one bounded outcome class, the hook's refusal, which its span records as
  `simulate.error_reason=hook_refused` and whose dynamic causes are an
  unregistered pool between solve and dispatch, the u128 projection overflow, a
  target PoolManager mismatch, and other genuine hook refusals.
- Drivers — `UnsupportedPathCapabilityError` with a typed
  `UnsupportedCapabilityKind` reaches Python through the thin mapper and the
  regenerated stubs; no driver parses rendered error text.

Certification replay: `docs/architecture/path-admission-replay-evidence.md`. Over
one fixed corpus on the pre-implementation commit `3a36a098c` and the final tree,
every leg asserts its old/new expectations through explicit environment inputs:

- admission, solve and inline sim: registered paths 10 → 4, inline-arm
  candidates 8 → 4, per-path solve gate evaluations 10 → 4, `no_encoder_arm`
  degrades 3 → the series absent (3/8 = 0.375 of inline candidates
  pre-cutover), `hook_refused` degrades 2 → 1, unsupported admissions refused at
  the door 0 → 6;
- supported-path parity: four profitable rows (V2+V3, V2+V4, all-V2 4-hop, the
  compose-capable V2 pair) with asserted, byte-identical `optimal_input`,
  `profit`, `hop_outputs` and identical inline-sim outcomes in both binaries;
- crawl build/verify: build-stage entries 2 → 1, completed pool verifications
  1 → 0 (the old re-sighting joins the completion its first sighting already
  made — `VerifyLifecycle` runs at most once per pool), registrations 2 → 0,
  with the re-sighting answered at stage 1 by the pool fact,
  and the supported control unchanged (5 stages, 1 verify, 1 registration) in
  both;
- all targets declined: the series is absent pre-cutover and carries exactly one
  `{class="capability"}` fact post-cutover, while the candidate keeps its own
  encode-failed refusal and its registration.

Two items stay outside the replay and are stated in the evidence document rather
than converted into zero-valued measurements: a crawl-level four-hop out-of-shape
candidate (its fixture needs V3 tick reads the crawl fixture does not serve; the
refusal itself is proven at the registration door) and a live production RPC
scrape (the corpus is synthetic and in-process, which is what makes it
reproducible).
