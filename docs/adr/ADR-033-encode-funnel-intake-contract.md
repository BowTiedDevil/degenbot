# ADR-033: The encode funnel's intake contract — one encode request per path, one context per session

**Status: accepted.** Decision recorded 2026-08-17 (architecture review, encode-seam candidate). Implementation: ergo task `NSOFR2` under epic `4GHYBP` (follow-up `SMOZG3` for the ERC6909 capture it keeps first-class).

## Context

The composer funnel `encode_cmd_stream` (degenbot-executor) was the single public entry
the whole repo encodes through, and its interface carried the grammar's internal
vocabulary at the seam:

- **8 positional args** — path infos, three parallel amount arrays
  (`optimal_input` / `hop_outputs` / `consumed_inputs`), three session-scoped
  deployment addresses, and an options bundle.
- **A solver-internal invariant re-asked of every caller** — `consumed_inputs[i]`
  must be the *executable* input to hop i; for an over-fed CL hop the solver's
  clamp sets it to `input_consumed − 1` so the on-chain exact-in loop terminates
  at `amountRemaining == 0` (UO3JM4 / path-5000 EMPTY-HALT). The invariant is
  documented in ≥4 files, yet the declarative harness — the one tool meant to
  exercise encode behaviour on the real `cmd_executor` — synthesizes
  `consumed_inputs` as `once(optimal_input).chain(hop_outputs)`, so the clamp
  path was never tested at runtime.
- **A legacy precedence rule at the seam** — `erc6909_profit: bool` vs the
  `capture` axis, collapsed by `resolve_axes`.
- **Facade rot** — a 43-line one-statement `grammar.rs` pass-through; a crate
  header still describing the crate as a warmup-slot math leaf; a doc/code
  split on `check_mode` (doc: 0 for non-ERC-6909; code emits 1, plus an
  undocumented `SweepToAddress → 3`); an ignored `_pool_manager` parameter;
  dead `fits_int128` tombstones.

## Decision

1. **`EncodeRequest`** (new, `degenbot-executor::composers`) is the funnel's
   intake: it owns the `PathInfo` plus the amount triple plus the axis options,
   checked at construction (array-length alignment; a mismatch panics loudly
   naming the array — a programmer error, not a `Result`). A request without
   its path is the shape that let amounts be synthesized blind to what the
   path constrains; path-and-amounts are one unit.
2. **`EncodeContext`** (new, same module) carries the session-scoped
   deployment addresses (executor / PoolManager / WETH); one per session,
   never per-path.
3. **The funnel** becomes `encode_cmd_stream(&EncodeContext, &EncodeRequest)
   -> Option<Vec<u8>>` — name and decline-`None` semantics unchanged (ADR-030's
   public-seam shape stands), 8-arg form deleted in the same commit. Hard
   cutover: every in-tree caller migrates in one pass; there is no parallel
   implementation (AGENTS.md: no back-compat layer; byte-identity is already
   gated by the golden corpus + revm matrix).
4. **`erc6909_profit` stays a first-class option.** It is the operator's
   toggle for Uniswap-V4 ERC6909-vault profit capture — modeled end to end
   (axis, `check_mode = 2` on-chain assert, warm slots) but not yet
   production-wired. It is a live feature knob, not dead weight; wiring it
   correctly is follow-up task `SMOZG3`, not this ADR's scope.
5. **The sweep** deletes the pass-through, fixes the facade and the
   `check_mode` doc to the code's reality, drops the ignored parameter (and its
   `_ffi.executor` stub ripple), and removes the dead tombstones.
6. **The harness earns the depth**: a new declarative entry accepting
   caller-supplied amounts plus an over-fed CL fixture — clamped via the pure
   pools rule (`V3SwapOutcome::exact_input_clamp_bound`) to a profitable
   execution, unclamped to the EMPTY-HALT verdict — so the clamp invariant is
   tested at runtime against the real contract, the first time.
7. **Internal encoders stay** (`encode_cmd_3_hop`'s 8 pinned call sites
   included) — the deepening is the interface move; the walker/encoder
   internals are the grammar-walker continuation's territory.

## Considered options

- **Path stays a funnel argument** (`(ctx, &PathInfo, &EncodeRequest)`): rejected —
  the request without its path is exactly the drift shape; the funnel's whole
  interface should be “session context + per-path request.”
- **Addresses folded into the request**: rejected — session scope re-stated per
  path, and it would make the harness (which has no deployment addresses) build
  fakes.
- **Kill the legacy `erc6909_profit` bool at the seam** (collapse into the
  `capture` axis, deprecate the Python kwarg): rejected on operator decision —
  the bool is the user-facing toggle for V4 ERC6909-vault capture, which is an
  intended feature awaiting correct implementation, not a legacy relic.
- **Additive cutover** (new entry alongside the 8-arg funnel, later deletion):
  rejected — no external surface depends on the arg shape (the encode funnel is
  not exposed to Python at all; `_ffi` exposes only warmup-slot/config
  helpers), and the byte-gates make a hard cutover low-risk.
- **Result at the value level** for length misalignment: rejected — a funnel
  argument-shape error is a programmer error; the fixture-driver convention in
  this repo is a loud, named failure, and `Option` at the seam stays ADR-030's
  decline semantics.

## Consequences

- The interface a caller (or test) must learn is two small values; the
  amount-triple invariants become a property of one constructor instead of
  prose repeated at every crossing.
- The clamp invariant gains a runtime home: the declarative matrix can — and
  now does — encode an over-fed CL hop at the clamped value and observe
  execution on the real `cmd_executor`.
- One new acyclic edge (`degenbot-simulation → degenbot-pools`, leaf math
  crate) lets the harness compute the clamp bound with the same pure rule the
  solver uses; the engine keeps the application policy (margin, which hops,
  forward-alignment) in `clamp_cl_hop_capacity`.
- The `degenbot-execution` seam's decoupled amount bundle (ADR-025 D5) is
  deliberately untouched; unifying it, if the default adapter deepening
  proceeds, is a separate decision.
- Semver: the public funnel signature changes within 0.6.x — acceptable per
  the repo's migration posture; standalone Rust consumers of
  `encode_cmd_stream` (examples, `standalone_consumer.rs`) migrate in-tree.
