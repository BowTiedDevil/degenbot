# Facet A (T2TCJM) — Decomposition Input

**Purpose of this file:** a detailed, code-verified summary of what Facet A asks for, so
a separate proposer session can workshop how to decompose it into discrete, reviewable,
byte-identical chunks. It is **not** a completed task; it is raw material for scoping.

Ergo task ref: `T2TCJM` · epic `PZGI6G` · state `doing` (claimed)
ADR: `docs/adr/ADR-025-execution-strategy-seam.md` (esp. D5: "collapse the 27+8
combinatorial fan-out behind `CmdExecutorComposer::compose` … as internals of the default
adapter behind the `PayloadComposer` seam … Red→Green against the existing golden-master
vectors, which now pin the default adapter's output").

---

## 1. Task statement (verbatim from ergo)

> Collapse the 27 `three_hop_*` + 8 `encode_cmd_*` permutation bodies behind
> `CmdExecutorComposer::compose` (the default adapter) using a generic per-family hop
> adapter (enum or match over `HopInfo`) driven by a generic 2/3-hop walk. Preserve the
> CL-clamp swap-in rule as ONE shared resolution point (`V2 → full output; CL →
> consumed_inputs[i]` + `fits_int128`) per ADR-025.
>
> **Guardrail:** byte-identical output vs the golden corpus (Red→Green); a 4th DEX family
> becomes a per-family adapter (one enum/route), NOT 35 copy-paste functions;
> `PayloadComposer` / `ExecutionStrategy` signatures unchanged — purely internal to the
> default adapter.
>
> **Speedrail:** the file's own `encode_cmd_v2_n_hop` already proves the generic-walk
> pattern for the all-V2 family — generalize that, don't invent a new pattern.

---

## 2. Current code layout (`rust/crates/degenbot-executor/src/composers.rs`, 4153 lines)

The file holds the whole `cmd_executor` command-stream encoder. After Facet B (HZI4L2, done)
the legacy `V4V4ArbitragePayload` / `V4V3ArbitragePayload` / `CmdExecutorComposer` / their
supporting types are **gone**. What remains:

### 2.1 Shared value types & helpers (already factored — these are the seam's knees)
- `ComposerInputs<'a>` — the bundled per-path context (executor / pool-manager / weth
  addresses, `optimal_input`, `hop_outputs`, `consumed_inputs`, `EncodeOptions`). Built once
  per path, passed by reference so no composer trips `too_many_arguments`.
- `EncodeOptions { erc6909_profit, use_v4_batch }`
- `PathInfo { hops: Vec<HopInfo> }`, `HopInfo::{V2,V3,V4}`, `V2HopInfo`, `V3HopInfo`,
  `V4HopInfo` descriptor structs.
- `fits_int128(u128) -> bool` — the int128 overflow guard.
- `v4_input_is_native(hop)` / `v4_output_is_native(hop)`.
- `enum CurrencyBridge { None, Wrap, Unwrap }` + `CurrencyBridge::at_boundary(a,b)` +
  `emit_currency_bridge(&mut inner, bridge, bridge_idx, forward_out)` — the native↔WETH
  gap helper.
- `AddressTable` (from `encoders`), the `enc_*` primitives, sentinel constants
  (`SENTINEL_WETH`, `SENTINEL_SELF`, `SENTINEL_NATIVE`).

### 2.2 Entry points
- `pub fn encode_cmd_stream(...) -> Option<Vec<u8>>` (line 321) — the public dispatcher.
  Routing: all-V2 → `encode_cmd_v2_n_hop`; 2-hop → match on `(hops[0], hops[1])` over the 8
  two-hop functions; 3-hop → `encode_cmd_3_hop`; otherwise `None`.
- `pub fn encode_cmd_3_hop(...) -> Option<Vec<u8>>` (line 1498) — match over all 27
  `(hops[0],hop[1],hops[2])` combos → the 27 `three_hop_*` functions.

### 2.3 The 8 two-hop composers (`encode_cmd_*`)
| fn | line | structural character |
|----|------|----------------------|
| `encode_cmd_v2_n_hop` | 395 | N≥2 all-V2 flash-borrow + chained `V2_SWAP_CALC`; uniform loop. THE SPEEDRAIL pattern. |
| `encode_cmd_v4_v4` | 507 | delta-netting vs `CurrencyBridge` gap; `use_v4_batch`; `erc6909_profit` (V4_MINT); V4_UNLOCK envelope; TAKE_DELTA/SETTLE_ALL. |
| `encode_cmd_v4_v3` | 717 | V4_UNLOCK; v4_out_native branch (V4_TAKE→WETH_DEPOSIT→V3 autopay→settle_delta) vs ERC-20 branch (take→V3 autopay→settle_delta / wrap-vs-withdraw depending on v4_input_is_native). |
| `encode_cmd_v3_v4` | 833 | **Nested**: V3 swap with `forward_data` callback that runs a `V4_UNLOCK` (sources WETH for V3's debt). v4_input_is_native branch inside the callback. |
| `encode_cmd_v4_v2` | 980 | V4_UNLOCK; v4_out_native → take→wrap→V2_SWAP_COMPACT w/ WETH-then-USDC callback; else take-direct-to-V2 + V2_SWAP_CALC; settle with v4_in_native wrap/unwrap variants. |
| `encode_cmd_v2_v4` | 1103 | **Nested**: outer V2_SWAP_COMPACT whose callback runs the V4_UNLOCK. v4_input_is_native inside callback (V4 settle_delta native) vs sync/transfer/swap; WETH_DEPOSIT in callback for native-out. |
| `encode_cmd_v3_v3` | 1247 | forward-order: V3_A callback pays WETH to V3_A then V3_B autopay; CL clamp on `consumed_inputs[1]`. |
| `encode_cmd_v2_v3` | 1309 | V2 flash → callback contains V3 swap with `forward_data` (ERC20_TRANSFER satisfying V3 IIA) then WETH repays V2. |
| `encode_cmd_v3_v2` | 1387 | V3 swap with `forward_data` running the V2 swap; explicit WETH payment ordering. |

### 2.4 The 27 three-hop composers (`three_hop_*`, lines 1568–3627+)
Every V2/V3/V4 × V2/V3/V4 × V2/V3/V4 combination (V4-V4-V4 … V2-V2-V2). Each is a bespoke
body with its own opcode ordering, callback nesting, native/WETH/ERC20 intermediate handling,
and CL-clamp resolution. Representative structural tolls (verified by reading the code):
- Purely-chained callback families (e.g. V2-V2-V3, V2-V2-V2, V2-V3-V2): a single top-level
  `V2_SWAP_COMPACT` or `V3_SWAP_COMPACT` whose `forward_data` nests the remaining swaps.
- V4-terminal families (e.g. V2-V2-V4, V3-V3-V4, V4-V4-V4): a `V4_UNLOCK` envelope
  containing `enc_v4_sync`/`settle`/`swap_compact`/`take_compact`/`settle_all`.
  `V4_V4_V4` exercises delta-netting + batch + ERC6909 just like the 2-hop V4-V4.
- V4-mid families (e.g. V2-V4-V2, V3-V4-V3): V3/V2 forward_data callback wraps a `V4_UNLOCK`,
  with settle/take ordering specific to the surrounding families.
- Native/WETH gap handling recurs per-position using `CurrencyBridge`.
- The CL-clamp `consumed_inputs[i]` rule is applied per-CL-hop at different indices;
  `fits_int128` guards are sprinkled per function.

The bodies are **hand-tuned for on-chain settlement correctness** — comments cite specific
revert behaviors (e.g. `CurrencyNotSettled` under-prediction for V4_TAKE amounts,
V3 IIA `balance_before` capture, over-fed CL clamps). The opcode *ordering* is load-bearing
and differs per family, not merely per hop.

---

## 3. The golden corpus (the byte-identity oracle) — `tests/`

| file | vectors |
|------|---------|
| `composers_parity.rs` | 44 (2-hop + helpers) |
| `composers_3hop_parity.rs` | 58 (all 27 3-hop + extras) |
| `native_eth_3hop_bridge.rs` | 5 |
| `native_v4_v2_mixed_path_ends.rs` | 7 |
| `native_v4_v2_v4_path_ends.rs` | 2 |
| `native_v4_v3_v4_path_ends.rs` | 3 |

**Total ≈ 119 vectors.** These pin exact `encode_cmd_stream` / `encode_cmd_3_hop` output
bytes (many assert `encode_cmd_stream(...) == Some(expected)` where `expected` is built from
`enc_*` primitives + `v4_envelope`). This is the ONLY oracle — the Python encoder was already
retired, so there is no live equivalence harness; the golden bytes ARE the spec.

**Pre-existing failing test (unrelated, must not be "fixed" by this task):**
`parity_v3_v4_v2` in `composers_3hop_parity.rs::parity_v3_v4_v2` fails on unmodified origin
(separate in-flight 3-hop V2-encoding fix). Do not attribute it to Facet A; note it.

---

## 4. What Facet A must produce (design contract)

1. **A generic per-family hop adapter** — an enum (or `match` over `HopInfo`) that, given a
   hop + role (first / mid / terminal) + the running `ComposerInputs`, knows how to emit ITS
   contribution to the subtree (swap opcode, forward/take/settle, callbacks) in the order the
   combination requires. Adding a 4th DEX family = one new adapter route, not 35 new copies.
2. **A generic 2/3-hop walk** that composes hop adapters into the full command stream,
   generalizing the existing `encode_cmd_v2_n_hop` pattern. Must reproduce the nested
   callback / `V4_UNLOCK` structure, not just a flat loop.
3. **One shared CL-clamp resolution point** (`V2 → full output; CL → consumed_inputs[i]` +
   `fits_int128`), replacing the current per-function scattering.
4. **`CmdExecutorComposer::compose`** as the **default adapter** implementing the seam
   (`PayloadComposer`/`ExecutionStrategy` in `degenbot-execution`). The public seam
   signatures in `degenbot-execution` must be **unchanged**; the collapse is purely internal
   to the default adapter. (`encode_cmd_stream` may remain as the adapter's internal engine,
   or be subsumed — see open questions.)

**Guardrail execution style:** Red→Green against the golden corpus at every chunk boundary —
each decomposition step must leave the full corpus byte-identical, so it is reviewable in
isolation and any divergence is localized.

---

## 5. Why this is hard (the decomposition obstacle — for the proposer session)

- The 35 bodies are **structurally divergent, not symmetric permutations**. The obvious
  "generic walk = loop over hops, per-family emit" collapses as soon as a family mix requires
  callback nesting or an inner `V4_UNLOCK` whose opcode order depends on *which* families
  surround it.
- The `encode_cmd_v2_n_hop` speedrail is uniform *only because* all hops share one family
  with one settlement; generalizing it to mixed families is the crux, not a given.
- Byte-identity is the contract: a 4th-family-friendly design that ALSO bakes in every
  existing ordering nuance is a genuine design problem, not mechanical refactoring.
- The corpus is the only oracle; there is no live Python/`enc_*`-cross-check to lean on
  during the migration (the golden vectors themselves are `enc_*`-derived, but the *composer*
  level has no second implementation).

---

## 6. Candidate decomposition axes (starter ideas for the proposer session — NOT a plan)

Suggestions that a proposer session could evaluate/refine; do not treat as authoritative:

1. **By "shape class"** rather than by family: classify the 35 functions into a small set of
   architectural "templates" (e.g. `FlatCallbackChain`, `V4UnlockTerminal`,
   `CallbackWrapsV4Unlock`, `ForwardOrderAutopay`, …) and build one generic skeleton per class,
   then verify each class's full vector slice stays green before moving on. This bounds the
   real structural diversity and makes the "walk" honest.
2. **Shared-resolution first:** extract the CL-clamp rule + `fits_int128` into ONE helper and
   rewire all 35 functions to call it (a pure refactor with no byte change on the happy path,
   easily Red→Green in place) — a low-risk first chunk that de-risks the rest.
3. **Introduce `CmdExecutorComposer::compose` as a thin default adapter wrapper** that
   internally delegates to the existing `encode_cmd_stream`, then progressively move the
   permutation logic *into* the adapter. Keeps the seam provable at every step while the walk
   is built underneath.
4. **Bottom-up adapter build:** implement the per-family hop adapter for ONE family (e.g.
   V4, the richest) with its full settlement/order vocabulary, then add V2, then V3, checking
   the relevant golden slice after each. Pack the 2/3-hop walk last once the per-hop
   vocabulary is locked.
5. **Characterization tests as scaffolding:** the corpus already pins everything; add explicit
   per-class test grouping (or a `proptest`-style cross-check across the corpus) so each chunk
   has a crisp, local green signal rather than "the whole 119-vector suite."

Recommended sequencing risks to weigh: do the seam-proving wrapper + shared-resolution first
(low risk); do the family-adapter + walk last (high risk, largest byte-diff surface); keep
each merged chunk byte-identical so a regression is attributable to exactly one commit.

---

## 7. Open questions for the proposer session

- Does `CmdExecutorComposer::compose` **replace** `encode_cmd_stream` as the public entry, or
  wrap it while `encode_cmd_stream` remains for existing callers? **Verified:** `encode_cmd_stream`
  has LIVE Rust callers — `degenbot-backrun-strategy/src/simulator.rs` (imports at line 40;
  called at lines 1004 and 2635) — so the public fn (or its exact behavior) must survive the
  collapse, at minimum as the default adapter's engine. `src/degenbot/_ffi/executor.pyi`
  references `composers::encode_cmd_stream` for the Python driver path too.
- What is the exact nesting vocabulary the per-family adapter must own — is a single
  "emit hop subtree (first|mid|terminal)" method enough, or does each family need
  first/mid/terminal *triplet* behavior (since e.g. a V3 in position 0 vs position 2 encodes
  differently)?
- Should `EncodeOptions` (erc6909_profit / use_v4_batch) remain a flat option struct the walk
  threads, or become per-family configuration?
- How do the 8 two-hop and 27 three-hop relate to the walk — one `encode_hops(&self, path,
  inputs, arity)` entry that dispatches on hop-count, or two walks sharing an adapter?
- Confirm the pre-existing `parity_v3_v4_v2` failure is tracked separately and excluded from
  Facet A's green signal.

---

## 8. Gate commands (the acceptance test for any decomposition)

- `just test-rust` (workspace `cargo test`) — full corpus must be green.
- `just lint-rust` / `lint-rust-check` — clippy `-D warnings`.
- `just check-no-pyo3-in-cores` — pyo3 stays out of the core crates.
- `just fmt-check`.
- `cargo build --workspace` — downstream crates (`degenbot-simulation`,
  `degenbot-backrun-strategy`, umbrella `degenbot`, `degenbot_rs`) still compile.
- `PayloadComposer` / `ExecutionStrategy` public signatures in `degenbot-execution` unchanged.

---

*Generated 2026 — working-tree snapshot at the time of writing. The composer file was 4153
lines; the 8 two-hop + 27 three-hop functions and the 119-vector corpus are as enumerated
above. Verify line numbers before editing; they will drift.*
