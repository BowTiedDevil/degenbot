# ADR-029: The executor command grammar — axes over an open ledger, with a runtime-matrix source of truth

**Status: accepted.**

> **Implementation: landed** (epic `463V2C`, closed). See
> [executor-command-grammar.md](../architecture/executor-command-grammar.md)
> (the architecture) and [executor-grammar-usage.md](../architecture/executor-grammar-usage.md)
> (the usage guide) for the realized model. This ADR is the decision record.

## Context

The `degenbot-executor` command grammar is the model that turns a solver result
into the `bytes` a searcher's `cmd_executor` contract executes (ADR-025 — the
`ExecutionStrategy` seam's default adapter). Today the grammar is keyed by a
single axis — the hop-protocol tuple `(V2|V3|V4)^n` — with 35 hand-written
per-family adapters. Every other concern — where the stream's capital comes
from, where its profit goes, which hop wraps which `unlock`/callback, and the
delta/orderings that make a stream valid — is **implicit and hand-written into
each adapter**.

That single axis failed to express the invariants, and the runtime harness
(caught after byte-parity reified the bugs) proved it:

- `v2_v2_v4` / `v2_v4_v4` encoded `V4_TAKE_COMPACT(WETH, …)` **before** any swap
  created a positive PoolManager WETH delta, so real v4-core rejected the take
  (`require(cur > 0)` → `"D0"`). Fixed by self-funding the leading V2 hop
  (`e7182b95`) — the *funding source* was wrong for that shape.
- The terminal exact-out `V2_SWAP_COMPACT`/`DIRECT` 1-wei `UniswapV2: K`
  over-draw class (path-182449; `2PT5HH`) is the same disease: the terminal V2
  hop's *funding* (an exact-draw against a solver amount) is not an encoded
  invariant.

Byte-parity is self-referential — it derives expected bytes from the same
`enc_*` primitives the composer uses — so it cannot see ordering defects.

Separately, the searcher will add non-Uniswap DEX/lender families after the
Uniswap-only variations are proven correct: **Balancer** (a single external
*Vault* that holds all tokens and executes swaps), **Aave** (an external
*lender*), then **Curve**. Each needs new `cmd_executor` primitives. The grammar
must not be shaped so that these force a rewrite.

## Decision

### D1 — The grammar is keyed by orthogonal axes, not a bare protocol tuple.

A family is no longer `(V2|V3|V4)^n`; it is a **shape class** over independent
dimensions:

- **hop-protocol sequence** (`V2|V3|V4|…` with swap direction per hop),
- **funding source** — the declared origin of the stream's **entry** (seed)
  capital; **one per stream, chosen at runtime by the strategy/operator** as an
  economic knob (self-fund = cheaper gas for small opportunities; flash =
  access to outside capital for large ones). Values: self-funded, pool
  flash-loan (a **flash source pool**, in-path or off-path), PoolManager free
  take, external-lender flash (Aave), ERC-6909 burn-to-settle.
- **profit capture** — the declared destination of the stream's **terminal
  profit** (the excess over the entry capital refunded); **one per stream**.
  Values: custody, owner (`OWNER_ADDR`), native, ERC-6909 mint, and (with the
  Balancer integration) the Balancer Vault. Modeled values are declared even
  where the current executor cannot yet express them.
- **builder bribe** — a **separate** output axis (recipient + amount), paid
  via `execute`'s `config` parameter; orthogonal to profit capture and needed
  to incentivize builder inclusion on mainnet.

The inter-hop amounts are an implementation detail — they must be sized
correctly to achieve optimum swaps and avoid reversion, but they are not a
user-facing axis.

### D2 — The **ledger** is an open abstraction, not a closed set.

Operations read/write an accounting target — the **ledger**: the executor's
ERC-20 balance, the PoolManager delta, an ERC-6909 held balance, a direct
pool-to-pool handoff, or (with Balancer/Aave) an **external** Vault/lender. The
ledger is an **interface-defined location**, with the current four as concrete
instances — never a closed enum — so a Balancer Vault or Aave lender is one
more ledger, not a new grammar shape. The ordering invariant the grammar
enforces is **credit-before-debit within a ledger**.

A **flash source pool** may be **in-path** (also a hop — the unified
borrow-and-swap callback) or **off-path** (an independent borrowing point). The
**repayment pivot** — the derived hop or mechanism that settles a borrowed
ledger — is chosen by token roles + **hop coupling**, never hand-picked.

### D3 — Enclosure/call-structure is **derived**, not chosen.

Which hop wraps which `unlock`/callback is a **consequence** of the ordering
required to satisfy each contract's invariants (ERC-20 funding, transfer
allowances, callbacks, ledger credit) — not a user-facing axis. It is computed
by the grammar, so the author cannot hand-reason it into the D0-style bug.

### D4 — Hybrid representation: coupling/ledger facts are data; mechanics are code.

A protocol's **coupling and ledger facts** (which ledgers a hop touches,
credit/debit, direction rules) are **declarative data** so the ledger validator
can prove ordering generically and exhaustively over every
(protocol × funding × capture × bribe) combination. A protocol's **swap and
callback mechanics** (Solidity callback return-wiring, a Vault `deposit`, a
lender `flashLoan`) are **code behind a per-protocol interface**, tested in
isolation. This is the choice most testable for the ordering property: a
generic validator over declarative facts makes "bad command streams impossible
to write" testable; fully-codified ordering lives per-protocol where nothing
enforces it generically; fully-declarative mechanics cannot express the
executor's imperative wiring.

**What "derived" means here (clarified by `6ZIE5X`, mechanism chosen `BP7KIR`):**
the deliverable is a **`Plan` tree** — the family's ledger decisions authored
as an execution-ordered, callback-nested tree of declarative facts. Two
consumers derive from the **same** Plan: the **encoder** produces bytes
(`Plan → Vec<u8>`, callback subtrees nested inside their `FlashSwap`/
`V4Unlock` steps — the nesting IS execution order); the **validator** walks
the Plan depth-first (`Plan → Vec<LedgerOp>`, depth-first = execution order)
and enforces the invariants. One representation, no drift, no reordering, no
per-family trace duplication. Byte emission is still code (per-protocol
mechanics behind the encoder); the **decisions/facts are data** (the Plan).

This supersedes two mechanisms the `6SRC23` POC ruled out: `enc_*`-instrumentation
(produces emission-order traces, wrong for callback-nested flashes like
`v3_v4_v3`'s 3-level V3c→V3a→`V4_UNLOCK` nesting) and byte→`LedgerOp` decoding
(couples the gate to byte layout and fights additivity). The `6YUNQN` spike's
`ShapeClass`+`HopFacts` byte-derivation of the V2/V3 2-hop slice proved
feasibility of data-driven derivation; a future generic `ShapeClass`→Plan walker
(the `6ZIE5X` (a)-branch, deferred) may build on Plan-authored families, but
per-family Plan authoring is sufficient to deliver D4/D5. What is mandated:
(1) each family's stream is produced by building a Plan then encoding it;
(2) the validator gates the Plan (credit-before-debit, terminal-V2
pre-fund-then-`V2_SWAP_CALC`, flash-debt-net-zero) for every
(protocol × funding × capture × bribe) combination. Language describing the
emitters as "derived" is to be read as "Plan-derived bytes; ordering proven
by the validator over the Plan," not as data-driven byte synthesis from
`ShapeClass`.

### D5 — The runtime matrix is the source of truth; byte-parity is a cross-check.

Correctness is judged by **actual execution through the on-chain contract**:
the matrix runs the production encoder methods, executes the stream in revm,
and asserts `actual_delta == predicted` exactly. Byte-encoding is an
implementation detail **handled by the encoder methods the matrix calls**, so a
future executor revision (new commands, new byte layout) is absorbed by those
methods without re-validating bytes. Byte-parity is demoted to a weak "matches
the current reference" check that the matrix may override (the reference corpus
may itself contain latent bugs). **The matrix also validates every produced
`Plan` through the ledger validator** (D4: `Plan → LedgerOp` depth-first walk,
then `LedgerValidator::validate_full`) and asserts the ordering invariants hold
— this is the gate that makes credit-before-debit and terminal-V2 pre-fund
unrepresentable, not merely spot-tested.

### D6 — Scope and additive-proof boundary.

This epic proves **Uniswap-only correctness** (matrix bounded at 3-hop + all-V2
any-N) and builds grammar machinery that is **hop-count-agnostic** by design
(arbitrary N given a valid token path) and admits external ledgers/lenders. The
additive-extensibility proof is **modeled via stub external contracts** in the
harness — it demonstrates that an external-Vault ledger (Balancer) and an
external lender (Aave) compose as axis values — **not** a real Balancer/Aave
integration, which is a separate epic including `cmd_executor` primitive work.
`2PT5HH` (the 1-wei terminal-V2 fix) is **subsumed**: the "terminal V2 is
pre-funded-then-`V2_SWAP_CALC`" rule becomes an encoded ledger invariant fixed
once for all affected families.

**Additive, not grafted (clarified by `6ZIE5X`):** a new protocol composes as
**one or more axis values** — a new `Ledger`/`FundingSource`/`ProfitCapture`
value + a per-protocol mechanics impl + its own declarative facts (+ new
`cmd_executor` primitives / `LedgerOp` variants only where the math is
genuinely different). Some per-protocol complexity is expected and bounded to
**that protocol's layer**. The additive proof is demonstrated on a
**representative row** (one new value × one existing protocol) that executes
against a stub and passes the validator — **never** by grafting the new pool
into every slot of the existing 2-or-3-hop matrix. The combinatorial explosion
of (new protocol × every position × every neighbor × funding × capture) cells
is the bespoke-adapter disease this epic kills; the additive proof must not
rebuild it. Bounded per-protocol complexity in exchange for zero cross-matrix
fan-out is the intended trade.

## Considered options

- **Fully-codified ordering** (rules inside per-protocol impls): better
  locality, but ordering validity becomes generically untestable and
  re-introduces the combinatorial failure mode in trait clothing — rejected.
- **Fully-declarative mechanics** (express Solidity wiring as data): ordering
  facts are enumerable, but imperative callback/Vault/lender wiring cannot be
  data; a data-DSL to fake it is its own bug farm — rejected.
- **Fixed-at-deploy axes:** defeats the runtime economic funding choice —
  rejected.

## Consequences

- A new DEX family is one interface impl + one declarative descriptor +
  (for genuinely different math) new `cmd_executor` primitives; funding and
  capture values compose additively instead of multiplying adapters.
- The ledger validator and the expanded matrix
  (family × funding × capture × bribe) become the correctness gate for every
  future composer/encoder change.
- The `erc6909_profit`/`use_v4_batch` opts, today honored only by `v4_v4_v4`,
  become general axis values/ledger strategies honored across families.

Implementation, terminology, and the matrix live under ergo epic `463V2C`;
vocabulary is recorded in `CONTEXT.md` ("Executor command layer"). The runtime
harness is `rust/crates/degenbot-simulation/tests/harness_declarative.rs`;
grammar is `rust/crates/degenbot-executor/src/grammar.rs`.
