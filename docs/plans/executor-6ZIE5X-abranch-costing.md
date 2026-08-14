# `CM5V3X` / 6ZIE5X (a)-branch costing — generic `ShapeClass → Plan` walker vs the per-family builder status quo

**Status:** decision record — costing + recommendation for how the next DEX
(Balancer/Aave) should land. **Decision:** per-family builders (6ZIE5X(c)
status quo) first; do NOT fund the (a)-branch generic walker before Balancer.
Revisit (a) after (1) the exhaustive validator gate lands and (2) ≥2 real
non-Uni DEX axes have shipped.

**Read against (frame is settled — not re-litigated here):**
- [`executor-6ZIE5X-decision.md`](./executor-6ZIE5X-decision.md) — adopted (c)
  (validation-from-data gate, emission-as-code), deferred (a).
- [`ADR-029`](../adr/ADR-029-executor-command-grammar-axes.md) — D1–D6.
- [`executor-v4-ledger-rules.md`](./executor-v4-ledger-rules.md) — the
  boundary classifier + delta rules.
- ergo `6ZIE5X` (done: option (c)), `GCC6I6` (done: axis types + `LedgerOp` IR +
  `LedgerValidator`), `VIXQYH` (done: additive ExternalLedger proof).
- **Post-T1/T2 ground truth** (this record's contribution): pointer to the
  numbers the original 6ZIE5X review did not have.

---

## 1. The status quo cost (post-T1/T2)

What the per-family authoring model (6ZIE5X(c)) actually costs *now*, after the
`ERP6ES` depth-split and the `GLOPCN` scaffold extraction:

| measure | value |
|---|---|
| `build_*_plan` public builders | **35** |
| family→author dispatch (`derive_shape` match, PPPHES) | **35 rows / 36 keys** (the shared any-N all-V2 arm covers `(V2,V2,V2)` and `(V2,V2,None)`) |
| `grammar_shape.rs` (builders + dispatch) | **6,848 lines** |
| `grammar_plan.rs` (deep stable walker) | **925 lines** — 18 `PlanStep` variants, `Plan` type, `plan_to_ledger_ops` + `plan_to_bytes`, forward primitives `v2_forward`/`v3_forward`/`v3_input` |
| `grammar_ledger.rs` (axes + `LedgerOp` + validator) | **1,903 lines** |
| shared scaffold primitives | **7 V2/V3** (T2: `seed_address_table`, `guard_arity`, `guard_forward_out`, `guard_no_zeroed_output`, `checked_swap_input`, `funding_branch`, `finish_plan`) + **5 V4** (`v4_scaffold_table`, `v4_hop_currencies`, `v4_terminal_capture_steps`, `v4_bridge_steps`, `native_capture_declines`) + forward primitives |
| pure V2/V3 builders | **1,151 → 1,055 lines (−96)** after GLOPCN |
| a new 3-hop V2/V3 fold | **≈ 86 lines** ("scaffold + a thin authored `PlanStep` sequence") |
| byte-identity guard | **444-cell FNV-1a golden** (`tests/glopcn_bytepin.rs`): every 2/3-hop family + any-N all-V2 (N=4) × 4 amount sets × 3 opts, pinning bytes **and** the None-decline partition |
| runtime matrix | **36 families** (9×2-hop, 27×3-hop) `harness_declarative` full_matrix — exact WETH-delta through real `cmd_executor` bytecode |
| executor/simulation tests | 117 / full crate green (after GLOPCN) |

**What the marginal cost of a new family converged to.** Under the additive
scope of D6 (a new protocol composes as **axis values + a mechanics impl + its
own facts**, never grafted into every matrix slot), a new *family* is now
"SCOPE_SLOT + thin authored Plan": the guard ladder and AddressTable sentinel
seed are shared, the exit is shared, and the family author writes only the
`PlanStep` sequence + the per-family wiring (AddressTable insertion order,
closing-currency rule, guard predicate set). That is the D6 trade made literal:
**bounded per-protocol complexity** (the authored Plan + wiring) in exchange for
**zero cross-matrix fan-out** (the validator + bytepin prove the new row from
shared primitives, not from re-deriving siblings).

The cost that did **not** collapse is the per-family **quirks** the GLOPCN
extraction had to *leave* authored (because unifying them would change bytes):
(1) the **guard ladder** genuinely diverges — `v3_v2` checks no `fits_int128`,
`v3_v3` checks no swap-in, all-V2 checks no `fits_int128` at all, 2-hop uses
`forward_out == 0` vs 3-hop `contains(&0)`; (2) the **AddressTable insertion
order** is family-specific and must mirror each emitter's `SET_ADDRESS`
preamble (`v3_v2_v2` is reverse-hop order; `all_V2` is hop-order + forward +
closing; `v2_v3_v2` discards `forward_a` first); (3) the **closing-currency
rule** is authored (`v2_v3` repays its flash in `weth`, all-V2 in
`v2_forward(last)`). These three are precisely the kinds of facts a generic
deriver would have to model per family anyway.

---

## 2. The (a)-branch cost — a generic `ShapeClass → Plan` walker

**(a)** is NOT "generalize `plan_to_bytes`" — that walker is done and stable
(`grammar_plan.rs`). **(a)** is *deriving the family's decisions* — the `Plan`
tree itself — from declarative `ShapeClass`/`HopFacts` data, replacing the 35
authored builders. The `6YUNQN` spike proved byte-derivation is **feasible** on
the 2-hop V2/V3 slice; the question is whether it is **cheaper** than per-family
authoring at the post-T2 margin. Costing the pieces:

**The engine (what the walker must model to derive a Plan):**
1. **The boundary classifier** (executor-v4-ledger-rules): V4→V4 internal
   ledger move (no transfer), V4→outside `TAKE`, outside→V4 `SYNC+TRANSFER+
   SETTLE`, capture `TAKE`/`MINT`, native↔WETH `WETH_DEPOSIT`/`WETH_WITHDRAW`
   + the `CurrencyBridge` wrap/unwrap layer, trailing `SETTLE_ALL`.
2. **V3 flash-callback credit** — a V3 flash's callback must credit its input
   before the downstream consumer debits it; the callback-nesting is itself the
   ordering.
3. **`ERC20_TRANSFER` wiring + the repayment pivot** — which `FlashSwap` repays
   which flash (in-path vs off-path), explicit-vs-auto-pay composition, and the
   flash-debt saturation semantics.
4. **Native ledger** — `NativeTransfer`, `WethDeposit`/`Withdraw` D0.
5. **`V4_BATCH`, capture-vs-credit `MINT`, `V4_SYNC`/`SETTLE`/`SETTLE_ALL`
   netting** — including the `use_v4_batch` / `erc6909_profit` axis values only
   today honored by `v4_v4_v4`.

**The hardest sub-problem — D3 enclosure derivation.** Which hop wraps which
`unlock`/callback is a *consequence* of ordering requirements, not a chosen
axis. Today the builders encode the nesting **structurally**. A generic walker
must **derive** it (a scheduler/solver over the credit-before-debit +
flash-repayment + net-zero constraints) and produce a tree byte-identical to
the 35 transcriptions. This is where the per-family quirks (§1) bite: the walker
must reproduce, from data, each family's exact AddressTable insertion order,
guard-decline set, and closing-currency wiring — the very three things the
GLOPCN extraction could *not* factor into shared code without changing bytes.

**The regression surface it replaces.** The walker does not delete the
correctness problem; it moves it behind one deriver that must pass the same
**444-cell bytepin** (bytes + decline partition) and the **36-family runtime
matrix** (exact deltas). A single derivation bug becomes a 36-family bug (the
RFPI6H class generalized), and the bytepin/matrix remain the only proof — so
(a) buys zero correctness, only authoring-volume.

**IR note (the honest overlap with (c)).** (a) and (c) share one no-regret
precondition: the `LedgerOp` IR is today only ~"two bug-class" wide
(credit-before-debit across Erc20/Pm/Native/External, terminal-V2
PairHandoff-seed, flash-debt-net-zero) — NOT the full stream the 6ZIE5X doc's
"what (c) requires that is NOT yet built" list describes (V3 flash-callback
credit as a full trace, `V4_SYNC`/`SETTLE`/`SETTLE_ALL` netting, `V4_BATCH`,
capture-vs-credit `MINT`, native ledger). **Widening the IR + wiring the
validator as the exhaustive matrix gate is required under BOTH (a) and (c)** and
is the prerequisite that makes per-family authoring safe enough to keep paying
for. So the IR-widening is not counted as (a)-only cost — but the **deriver +
HopFacts data model + byte-exact regression harness** are (a)-only, and they are
the multi-week tranche.

**Rough (a) tranche:** new `HopFacts` data model + boundary/ledger-fact
descriptors per protocol; the enclosure/nesting scheduler; porting all 35
families' quirks into data; a derivation-bytepin so the deriver matches the
transcriptions; and keeping both the golden and the runtime matrix green across
the swap. Against a status quo where a new family is ~86 lines + shared guards,
the deriver pays for itself only if it eliminates far more authoring than it
creates data/scheduler complexity — which the existing 35 families do NOT
demonstrate (their byte-exact quirks transfer into data either way).

---

## 3. The Balancer/Aave decision

**How many per-family builders does the next DEX actually add?** Under D6's
additive scope the answer is deliberately *not* "protocol × position ×
neighbor × funding × capture". `VIXQYH` already proved the additive shape: a
new external ledger composes as **one `BalanceLedger` impl + one set of facts +
new `LedgerOp` variants only where the math differs** (it quantified: 1 impl vs
**54 would-be adapters** if grafted). So:

- **Balancer** = one new `Ledger::External` (Vault) value, one per-protocol
  mechanics impl, its own boundary facts (the Vault holds all tokens; a swap is
  a single external ledger move, no per-pool `TAKE`/`SYNC`), new `cmd_executor`
  primitives only for genuinely-new math. Per-family builders added: the
  **representative rows** (VIXQYH's acceptance: one new value × one existing
  protocol) + whatever reachable Balancer-involving families the product
  actually exercises — **single-digit, not +35**.
- **Aave** = one new `FundingSource`/ledger value (external lender flash), one
  mechanics impl, representative rows — again single-digit.

So the "builder count" case for (a) is **weak at Balancer/Aave**: the additive
model keeps the count linear in axis values, not combinatorial. The 35 grows to
~38–42, each new family ~86 scaffold-backed lines, gated by the shared
validator + bytepin — which is exactly the bounded-per-protocol-complexity
trade D6 names.

**When the count DOES grow enough to matter:** if the product lands multiple
independent protocols (Balancer **and** Aave **and** Curve) and each exercises
several representative rows in heterogeneous topologies, the authoring volume
starts to be worth a deriver — but only *after* the exhaustive validator gate
exists, otherwise the deriver re-introduces the D0-class risk at 36-family
scale. That is the sequence: **validator-exhaustive first, then (a) is safe to
revisit; never (a) before the validator.**

---

## 4. Recommendation

**Keep per-family builders (6ZIE5X(c)) for the next DEX. Do not build the (a)
generic `ShapeClass → Plan` walker before Balancer/Aave.**

Reasoning, tied to D6 and the post-T2 builder count:

1. **The status quo is already the cheap half of (a).** Post-GLOPCN, a new
   family is "scaffold + thin authored Plan" (~86 lines) with the guard ladder,
   sentinel AddressTable, funding dispatch, and exit all shared. The marginal
   authoring cost of family #N is low and falling; D6's "bounded per-protocol
   complexity, zero cross-matrix fan-out" is realized: bounded in the authored
   Plan, zero in the validator + bytepin proving the row from shared
   primitives.
2. **(a) costs a multi-week deriver + data model + scheduler and buys zero
   correctness** — it must byte-match the same 444-cell golden + 36-family
   runtime matrix, and a single derivation bug generalizes to all families. Its
   hardest pieces (D3 enclosure derivation; the AddressTable-order / guard-set /
   closing-currency quirks that GLOPCN could not factor without changing bytes)
   transfer per-family complexity into data rather than deleting it.
3. **The builder-count case is weak at Balancer/Aave** precisely because D6's
   additive scope governs: the new DEX adds axis values + mechanics + a
   representative-row handful, not a combinatorial multiply. ~35 → ~38–42
   scaffold-backed builders is not the "post-(b) bespoke-adapter explosion" (a)
   exists to prevent.
4. **The no-regret work is the validator, not the deriver.** Widening `LedgerOp`
   to the full stream and wiring `LedgerValidator` as the exhaustive matrix
   gate is required under (a) *and* (c) (the 6ZIE5X doc names it as the "NOT yet
   built" foundational item). Until it exists, a generic deriver is the wrong
   place to spend: it would replace per-family authoring that is already gated,
   with a single point of failure that is *not yet* gated exhaustively.

**Revisit (a) when:** (1) the exhaustive validator gate ships (the
IR-widening); (2) ≥2 real non-UniDEX axes (Balancer + Aave + Curve) have
actually landed, so the deriver has a real heterogeneous dataset to amortize
against; and (3) the per-family count crosses a threshold where authored-Plan +
data volume exceeds deriver + data-model upkeep. Until then the walker stays
the deferred long-run collapse — explicitly unfunded before Balancer/Aave.

**No code changes in this task** (guardrail). The only surfacing code need is
the already-known, no-regret `LedgerOp`-widening + validator-as-matrix-gate
(pre-existing 6ZIE5X "what (c) requires that is NOT yet built") — tracked
separately, not implemented here.
