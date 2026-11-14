# ADR-030: Derivation distinguishes decline and encoding failure; a validator Reject is always fatal

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

The derivation result distinguishes `Encoded`, `Declined`, `EncodingFailed`,
and `Rejected`. The public seams (`encode_cmd_stream` and the `*_detailed`
derivation entries it projects) keep returning `Option<Vec<u8>>`: Decline and
primitive encoding failure map to `None`. Detailed adapters preserve the
distinction: an unsupported path is a per-candidate decline, while a primitive
range or length failure is a typed encoding error. Neither case panics.

A fifth arm joins the set for the same reason `Declined` is not a Reject:
`InsolventZeroCapital` answers a *built, structurally sound* Plan the declared
capital policy cannot fund. It is routine (the target-selection walk advances
past it), it carries its own `asset` / `required` / `available` data instead of a
message, and it maps to `None` at the public `Option` seam. It is unreachable
under the production `InitialCapital::Held` stance, so production rejection
classification is untouched.

A **Reject is always fatal** — the revm matrix and honesty suite hard-fail on
it, and a live run aborts. It is never swallowed, never degraded to a skip.


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

- Primitive encoding failure is separate from validator Reject: representable
  caller input may exceed a wire field even when ledger ordering is valid, so
  the emitter returns its primitive error instead of asserting that validation
  proved all wire ranges.
- A validator Reject is reachable even after the grammar generalizes to a
  facts-driven walker (ADR-030's companion deepening): the net-zero invariants
  (PM-at-unlock-close, flash-debt-at-finish) accumulate solver-derived amounts
  and hand-authored facts can encode an invalid stream — the validator is not
  redundant, and Reject stays live.
- The revm matrix / honesty suite treat any Reject as a hard, suite-failing
  assertion — "bad streams are unrepresentable" becomes loudly testable.

### Addendum: held capital is *declared*, and the zero-capital deficit is where its amount comes from (2026-11-20)

The stance above answers "can this stream run with no entry capital?".  A
**self-funded** stream (`FundingSource::SelfFund`) asks the neighbouring
question — "does this stream declare the entry capital it spends?" — and the same
one ledger evaluation answers both.

A self-funded stream pays its entry capital out of the executor's balance
mid-stream (an `ERC20_TRANSFER` into a pool, or the currency leg of a
`V4_SETTLE*`) where a flash stream borrows it.  Under `InitialCapital::Held` that
payment was the *assumption*, and an assumption nobody wrote down is invisible to
everything downstream: the Plan declared no `SelfFund` seed, the capital override
installed nothing, and on-chain the payment ran against the path's short proceeds
— WETH9's bare `require` reverting with empty return data, a failure with no
typed error and no frame to attribute it to.

The fix is not a new rule per family.  `LedgerValidator::undeclared_seed`
evaluates the built Plan's projected op trace under exactly the `Held` reading,
plus the two measurements that reading suppresses: an executor-paid `V4_SETTLE*`
debt is modeled as the debit it is on-chain, and a credit shortfall is typed as
the same `InsolventZeroCapital` deficit the zero-capital stance reports.  Its
`required - available` is therefore the *minimum* the stream must declare, and
`grammar_plan::declare_held_seed` prepends that `SelfFund` at the Plan head (the
producers' own placement) until no payment is uncovered.  Seeds a builder already
declares credit the evaluation, so they stay authoritative and are never
restated; a second asset's later shortfall is simply a second declaration.

Consequences for the outcome set:

- `InsolventZeroCapital` stays unreachable under a production `Held` pass — the
  self-funded evaluation is a derivation input, not a stream verdict.
- A losing self-funded stream still **encodes** and still executes to its
  economic receipt (`InsufficientProfit`): the declaration is the shortfall, not
  the entry, so the installed balance is the measured-loss baseline and no
  leftover seed can be reported as profit.
- `Rejected` keeps ADR-030's fatality exactly where it belongs.  An ordering
  violation (`SwapCalcBeforeCredit`, `PmDeltaNonzero`, `FlashDebtUnpaid`, …) is
  not a capital fact, so the derivation declines to invent a seed for it, and an
  in-path-flash stream that pays capital it never declared still rejects — a
  flash stream must not spend held capital, which is the whole distinction the
  `funding` axis draws.
- The declaration emits no command byte (`SelfFund` is ledger-only), so every
  valid command stream, and the release's golden calldata, is byte-identical.
