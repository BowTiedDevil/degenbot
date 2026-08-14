# ADR-030: The derivation outcome is a tri-state; a validator Reject is always fatal

**Status: accepted.**

## Context

`derive_shape`/`build_plan_bytes` (degenbot-executor) returned `Option<Vec<u8>>`,
collapsing two meaningfully-different outcomes into one value: a **Decline** (the
derivation layer has no producer for this shape-class — a routine, expected
outcome the strategy skips) and a **Reject** (a Plan *was* built but the
`LedgerValidator` rejected it — by the ADR-029 D4 contract a successfully-built
Plan never violates the ordering invariants, so a Reject is definitionally a
latent bug). Folding a Reject into `None` silently dropped a computable,
profitable path with no diagnostic.

## Decision

The derivation result is a tri-state `Encoded / Decline / Reject`. The public
seams (`encode_cmd_stream` / `encode_grammar`) keep returning `Option<Vec<u8>>`:
Decline maps to `None` (a routine no-path). A **Reject is always fatal** — the
revm matrix and honesty suite hard-fail on it, and a live run aborts. It is
never swallowed, never degraded to a skip.

## Considered options

- **Reject = loud log + skip** at runtime: rejected, because a correct system
  never produces a Reject; a skip would hide a would-be-bug from every profiting
  path and contradict the fail-fast posture (ADR-021, "do not restore the
  swallow").
- **Error propagates up through `encode_cmd_stream` as a `Result`**: rejected,
  because `None` at that seam is a *routine decline* (unsupported family) and
  forcing ~20 positive proofs to handle a `Result` churns callers for no runtime
  gain. The Decline/Reject distinction lives at the derivation layer where its
  meaning differs.

## Consequences

- A validator Reject is reachable even after the grammar generalizes to a
  facts-driven walker (ADR-030's companion deepening): the net-zero invariants
  (PM-at-unlock-close, flash-debt-at-finish) accumulate solver-derived amounts
  and hand-authored facts can encode an invalid stream — the validator is not
  redundant, and Reject stays live.
- The revm matrix / honesty suite treat any Reject as a hard, suite-failing
  assertion — "bad streams are unrepresentable" becomes loudly testable.
