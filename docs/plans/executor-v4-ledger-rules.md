# Executor V4 Ledger Rules — design note (epic 463V2C, `WAYDTL` step 2)

**Status:** doc-then-proof stage. Grounds the V4 derivation in the authoritative
v4-core delta-accounting, in plain language. Reference read:
`Uniswap/v4-core` `src/PoolManager.sol` + `src/libraries/Pool.sol` +
`src/libraries/NonzeroDeltaCount.sol`.

## The one master invariant

`PoolManager.unlock(data)` runs `msg.sender.unlockCallback(data)` and, **after**
the callback returns, checks `NonzeroDeltaCount.read() != 0` — if so it reverts
`CurrencyNotSettled`. The callback is the executor running its command stream
(which may itself invoke V2/V3 pools and nested `V4_UNLOCK`s).

> **Therefore the only V4-wide sequencing requirement is: every touched
> currency's delta must be zero when the callback returns.** There is no
> per-pool repayment-ordering rule (contrast V3, which demands the input be paid
> within its callback). This permissiveness is why the executor can open with
> `V4_UNLOCK` and run everything inside it.

## The delta operations (from `PoolManager`)

Each operation changes `PM[currency]` (executor's delta; positive = PM owes
executor, negative = executor owes PM) and the nonzero-count. Nonzero-count
increments when a delta leaves 0, decrements when it returns to 0.

| Op (contract fn) | Delta change | What the executor must hold first |
|---|---|---|
| `swap` | input `−in`, output `+out` | — |
| `take(cur, to, amt)` | `−amt` (consumes credit) + transfers `amt` tokens to `to` | a **positive** `PM[cur]` ≥ amt, else delta goes negative and the stream fails to net zero at callback end |
| `mint(to, id, amt)` | `−amt` (consumes credit), mints ERC6909 | a **positive** `PM[cur]` ≥ amt (same rule) |
| `settle()` (via `sync` first) | `+paid` (cancels debt) | the executor holds the debt token (or sends `msg.value` for native) |
| `burn(from, id, amt)` | `+amt` (creates credit) | an ERC6909 claim `≥ amt` to burn |

`sync(cur)` sets the reserve baseline the executor's transfered-in tokens are
measured against for the following `settle`; for native it resets so `settle`
uses `msg.value`.

## The V4 emitter's single task (ledger-resolution engine)

Build a stream whose net effect is **all deltas → 0 by callback end**, using
the rules:

- A V4 swap is the only "source" of credit/debt: it makes input negative and
  output positive.
- Every positive-credit consumer (`take`/`mint`) must be **dominated** by a
  prior swap/`burn` in the same currency (credit-before-debit). The
  `LedgerValidator` (GCC6I6) already rejects a consumer running before its
  credit.
- Every negative-debt must be cancelled by a `settle` backed by executor-held
  tokens (a `take` into the pair that then swaps, or an `mint`/`burn` balance).
- Nothing else has ordering constraints; set `V4_SETTLE_ALL` (or a trailing
  `settle_delta`) to flush any residual.

This is **one uniform rule underneath any topology** — V4 is the outer carrier
precisely because this rule is so permissive.

## Boundary model: V4 pools are containers within the PoolManager

A V4 pool is a *container inside* the PoolManager. Moving value between two
V4 pools (same or different pool ids) is pure **internal ledger movement** —
the manager adjusts its per-currency deltas to move credit between the
containers and presents the executor deltas to resolve. **No ERC-20 transfer, no
`TAKE`.** A `TAKE` (or `settle`/`sync`) is needed only when an asset **crosses
the PM boundary**.

So the per-step instruction is a **boundary classifier**, not a universal
"landing spot":

| Boundary | Rule | Field needed? |
|---|---|---|
| **V4 → V4** (both in PM) | internal ledger move; credit between containers nets in the manager | no |
| **V4 → outside** (V2/V3 pool, wallet, transfer) | `TAKE(cur, recipient, amt)` — one real ERC-20 transfer, delta resolved; `recipient` is the landing spot | yes — recipient |
| **outside → V4** (V2/V3/wallet into V4) | `V4_SYNC(cur)` + `ERC20_TRANSFER(cur, PM, amt)` + `V4_SETTLE` to seed input credit (or reuse credit the PM already holds) | yes — source |
| **capture** (profit out of path) | `TAKE(cur, SELF)` (physical) or `MINT` (ERC6909 credit) | yes — capture mode |
| **native cap / bridge** | `WETH_DEPOSIT`/`WETH_WITHDRAW` where the boundary crosses native↔WETH | yes |

This is why the ledger rule is permissive: internal boundaries impose no
ordering (the PM just nets deltas), and the only constraints are (1) every
touched currency nets to zero by callback end and (2) a `TAKE`/`MINT` that
consumes credit runs after the swap/`burn` that produced it.

## Decisions confirmed in conversation

- **Framing (1): ** a **small set of topology templates** (who is the outer
  carrier, where the V4 sits) **+ one uniform ledger-resolution layer**, not a
  single self-inventing rule. — *chosen A*.
- **Hand-off (2): ** the **boundary classifier above** — `TAKE`/`settle` only at
  PM boundaries; V4→V4 is internal ledger movement. — *resolved*.
- **Scope:** first proof is **WETH-only**.
  **Caveat (kept in the model):** most real V4 pools are **native (ETH) pairs**,
  so the native↔WETH wrapper/unwrapper layer (`WETH_DEPOSIT`/`WETH_WITHDRAW` +
  the `CurrencyBridge` classification) is part of the rules from the start — the
  first proof simply exercises the WETH-only slice. We do not lose the native
  profit currency or the wrapper/unwrapper layer.
- **Workflow:** doc → minimal proof → cutover — this is the doc; next is the
  minimal WETH-only pure-V4 proof (`v4_v4`), then cutover guarded by
  byte-parity + the runtime matrix.
