# ADR-077: The refusal vocabulary — one `RefusalKind` owned by the registration outcome ledger, and the eligibility gate moves core-side

**Status: accepted (2026-10-16).** Drafted at the architecture-review grilling (candidates C1+C4; rounds 1–3, Q2/Q3/Q10/Q11/Q12/Q18/Q19/Q22). Refines ADR-022 (the lifecycle's home is unchanged; the vocabulary across the homes is what deepens).

## Context

"What can go wrong registering a pool" is told across seven `bot_core` files (6,612 lines) as ~21 refusal/outcome types: per-family register-phase enums (`V2RegistrationError`, `V3RegistrationError`, `AerodromeRegistrationError`, `ResolveV4IdentityError`), per-family build-phase twins (`BuildError`, `V2/V3/V4/AerodromeBuildError`), lifecycle/verify types (`RegistrationLifecycleError`, `LiquidityVerifyError`, `VerifyError`, `SnapshotLoadError`), and the ledger's own outcome types. The register path and the build path are parallel entries that converge on `BotState::register_*_pool`, never on each other — and address/identity mismatch has **three** vocabularies that never meet: `…::Create2(AddressMismatch)` (registration.rs:305/397/602), `parity::IdentityDivergence` (build_register.rs:201/528), and `PoolBuilderError::Create2` (builder.rs:110/473). The FFI pays the scatter directly: `errmap.rs` carries 18 `map_*` functions, one per enum (1,128 lines).

The ledger already owns the bounded cross-language outcome vocabulary (`RegistrationOutcome`, 13 variants; `RegistrationUnitOutcome`; `PipelineReport::absorb` as the sole counter-mutation point). Python is already a thin adapter over it (`_registration_ledger.py` forwards; tags minted from the core's exported lists) — with one exception: `_evaluate_registration_eligibility` (`build_paths.py:661`) makes real SKIP/REJECT/REGISTERED decisions Python-side.

Two scope facts: `CalcTokensOutError`/`CalcTokensInError` are swap-*calculation* refusals — they ride the read door's `SimulateSwapError` vocabulary and stay out of this ADR. `arb_engine/snapshot_verify.rs` is a 12-line re-export shim kept "so existing imports continue to resolve" — a compatibility layer the repo's hard-cutover policy forbids.

## Decision

**D1 — One refusal vocabulary, ledger-owned.** A single closed `RefusalKind` lands beside `RegistrationOutcome` in `registration_ledger.rs`. Family and phase ride as **data** (tags on variants), never as per-family enum types; load-bearing classifications (the stable-vs-transient build-refusal taxonomy, liquidity-verify's fatal-vs-transient) ride on the variant.

**D2 — The outcome type absorbs refusals; the enum trees die.** `RegistrationOutcome` gains the refusal arm carrying `RefusalKind` + tags. The per-family register/build/lifecycle/verify enums are deleted at cutover; `errmap.rs` collapses to one mapper over the one vocabulary.

**D3 — Identity parity gets one home.** The construction side owns identity parity (`parity::IdentityDivergence` subsumes the other two mismatch vocabularies); the register path consumes the verdict. Neither entry is upstream of the other, so the shared verify lives below both, at the construction/parity seam.

**D4 — The eligibility gate moves core-side.** The mechanical checks of `_evaluate_registration_eligibility` (direction mismatch, dup fast-path, rejected-path memo) land beside the ledger. `PathCompositionPredicate` stays a Python-side pre-filter expressing rejections through the shared vocabulary — no FFI callback per unit, no hot-path crossing.

**D5 — Landing: additive, phase-by-phase, hard cutover.** `RefusalKind` is introduced additively; migration proceeds identity-parity first, then registry, builder, lifecycle/verify; old enums are deleted per phase (no compat layer). The `arb_engine/snapshot_verify.rs` shim is deleted in the sweep and its imports re-pointed at `bot_core::snapshot_verify`.

## Consequences

The glossary's "Registration outcome ledger" is corrected: the ledger is **core-owned**; the cockpit consumes it. One refusal vocabulary gives the family write door (ADR-078) its refusal arm — this ADR lands first for that reason.
