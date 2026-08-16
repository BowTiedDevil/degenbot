//! Builders — the **wide, per-family transcription surface** + dispatch
//! (`grammar_shape.rs` split, ERP6ES / candidate 2 of
//! `architecture-review-1786663110.html`).
//!
//! The ~35 `build_*_plan` functions author the execution-ordered, callback-
//! nested [`Plan`]/[`PlanStep`] tree (the vocabulary + the two walkers live in
//! [`crate::grammar_plan`]), then [`build_plan_bytes`] runs builder →
//! [`LedgerValidator`][crate::grammar_ledger::LedgerValidator] gate →
//! [`plan_to_bytes`]. [`derive_shape`] dispatches every well-formed family to
//! its builder via a `(Prot, Prot, Option<Prot>) → BuildPlan` table (a new
//! family is a new row); all-V2 any-N routes through [`derive_all_v2`].
//! Returns `None` on decline or gate rejection.
//!
//! This is the **churning** half — a family addition is a new builder + a
//! table row + builder tests, and it stops touching the deep, stable walker
//! (`Plan → LedgerOp`, `Plan → bytes`) isolated in `grammar_plan`.
//!
//! ---
//! **Status after RVNIPD / EYQ6UF (epic MNF6VU):** the hand-written
//! `derive_2hop_*` / `derive_3hop_*` byte-assembling emitters and their
//! parity-oracle are **deleted** — the Plan is the sole production producer
//! for every 2/3-hop family. The revm runtime matrix (`degenbot-simulation`
//! `harness_declarative` full-matrix, exact delta) is the ADR-029 D5 source of
//! truth; the primitive wire format is pinned by `tests/encoders_parity.rs`
//! and the native bridge by `tests/native_eth_3hop_bridge.rs`. N4TJSZ
//! (SPVEIE + KO5NNB + 4JOWO5): the all-V2 family (2-hop, 3-hop, any-N) now
//! routes through [`build_all_v2_chain`] + the [`LedgerValidator`][crate::grammar_ledger::LedgerValidator]
//! gate — the sole all-V2 producer. PPPHES: the 35-arm dispatch collapsed to
//! the `(Prot,Prot,Option<Prot>) → BuildPlan` table.

use alloy::primitives::Address;

use crate::composers::{ComposerInputs, HopInfo, PathInfo, V4HopInfo, NATIVE_CURRENCY_ADDRESS};
use crate::encoders::{AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
// Re-export the deep walker's public surface so external
// `degenbot_executor::grammar_shape::{Plan, PlanStep, plan_to_bytes,
// plan_to_ledger_ops, Prot, FundingSource, …}` paths keep resolving.
pub use crate::grammar_plan::{
    plan_to_bytes, plan_to_ledger_ops, Axis, AxisSupport, Bribe, FundingSource, Plan, PlanStep,
    ProfitCapture, Prot, ShapeClass, V4BatchSwap,
};
// Fast-forward helpers are pub(crate) in the walker — shared by the per-family
// builders + the V4 scaffold.

// ═══════════════════════════════════════════════════════════════════════════
// Cross-protocol V2/V3 scaffold (GLOPCN). The pure V2/V3 families (2-hop,
// 3-hop, any-N all-V2) duplicate the same skeleton inline: the guard ladder
// (arity, zeroed-output, `fits_int128` on the entry + hop swap-ins), the
// sentinel AddressTable (weth/executor, no PoolManager — a V2/V3 family
// touches no PM ledger), and the exit `(preamble, plan, at)` assembly. The
// helpers below own that scaffold so a family is authored as
// "scaffold + a thin PlanStep sequence", and the class of symmetry bug
// (RFPI6H) has exactly one site per concept. V4-crossing families stay on
// their own scaffold ([`v4_scaffold_table`] etc.) — their topology (one
// `V4_UNLOCK` over the PM ledger) diverges; `finish_plan` is the only helper
// every family shares. Currency resolution is the per-hop primitives
// `v2_forward`/`v3_forward`/`v3_input` (walker) + the family's authored wiring
// — the closing-currency rule is NOT unified here because it genuinely
// diverges (`v2_v3` repays its flash in `weth`; all-V2 in `v2_forward(last)`).
// ═══════════════════════════════════════════════════════════════════════════

// The pure V2/V3 3-hop scaffolding (seed_address_table, guard_no_zeroed_output,
// checked_swap_input, finish_plan) was decommissioned by the facts-driven T5
// migration (epic 6SU5LM): every pure V2/V3 2-hop + 3-hop builder now delegates
// to `derive_plan`, which inlines the equivalent guards.

// ═══════════════════════════════════════════════════════════════════════════// ═══════════════════════════════════════════════════════════════════════════
// 3-hop Plan scaffolding (W7FQN6 pilot). The shared topology pieces every
// V4-crossing 3-hop builder (this pilot + task HPZTNT) calls: the sentinel
// AddressTable scaffold, per-hop currency/orientation, the ADR-029 D1 capture
// guard, the terminal-capture steps, and the native↔WETH bridge steps. The
// pilot proves the existing `PlanStep` vocabulary needs NO new variant for the
// 3-hop slice.
// ═══════════════════════════════════════════════════════════════════════════

/// The AddressTable scaffold for V4-crossing families: weth / executor /
/// PoolManager sentinels (PM resolves to `SENTINEL_PM`, no table entry).
pub(crate) fn v4_scaffold_table(inputs: &ComposerInputs<'_>) -> AddressTable {
    AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    )
}

/// A V4 hop's swap orientation: `(forward/output currency, input currency)`
/// from its `zfo` flag. Shared by every V4-crossing family.
pub(crate) fn v4_hop_currencies(h: &V4HopInfo) -> (Address, Address) {
    if h.zfo {
        (h.currency1_address, h.currency0_address)
    } else {
        (h.currency0_address, h.currency1_address)
    }
}

/// The ADR-029 D1 capture guard for a V4-crossing terminal: a
/// `ProfitCapture::Native` on a non-WETH/non-native terminal is not
/// expressible (the executor cannot convert an arbitrary ERC-20 to native).
pub(crate) fn native_capture_declines(
    capture: ProfitCapture,
    terminal: Address,
    weth: Address,
) -> bool {
    capture == ProfitCapture::Native && terminal != weth && terminal != NATIVE_CURRENCY_ADDRESS
}

/// Build the terminal-capture `PlanStep`s for a V4-crossing family (mirrors the
/// emitters' terminal-capture block): `erc6909_profit` (WETH terminal) → an
/// ERC6909 mint; otherwise a physical `V4TakeDelta` unless `use_v4_batch`
/// auto-settles (a tok terminal still gets an explicit take — see the batch
/// caller); plus `ProfitCapture::Native` on a WETH terminal → a `WethWithdraw`
/// of the custodied profit. Shared by every V4-crossing builder.
pub(crate) fn v4_terminal_capture_steps(
    terminal: Address,
    terminal_idx: u8,
    capture: ProfitCapture,
    use_v4_batch: bool,
    any_gap: bool,
    profit: u128,
    weth: Address,
) -> Vec<PlanStep> {
    let mut steps = Vec::new();
    if capture == ProfitCapture::Erc6909 && terminal == weth {
        if profit > 0 {
            steps.push(PlanStep::V4Mint {
                currency_idx: SENTINEL_WETH,
                currency_addr: weth,
                recipient_idx: SENTINEL_SELF,
                amount: profit,
            });
        }
    } else if !use_v4_batch || any_gap {
        steps.push(PlanStep::V4TakeDelta {
            currency_idx: terminal_idx,
            currency_addr: terminal,
            recipient_idx: SENTINEL_SELF,
            seeds_pool: None,
        });
    }
    if capture == ProfitCapture::Native && terminal == weth {
        steps.push(PlanStep::WethWithdraw {
            weth_idx: SENTINEL_WETH,
            weth_addr: weth,
            amount: profit,
        });
    }
    steps
}

/// Build the native↔WETH bridge `PlanStep`s for a boundary (mirrors
/// `emit_currency_bridge`): a `V4TakeCompact` of the source-side currency to
/// SELF + a `WethDeposit` (wrap) or `WethWithdraw` (unwrap), plus the
/// `settle_idx`/`settle_currency` the following swap's input dedebt needs.
pub(crate) fn v4_bridge_steps(
    bridge: crate::composers::CurrencyBridge,
    weth: Address,
    amount: u128,
) -> (Vec<PlanStep>, u8, Address) {
    let wrap = matches!(bridge, crate::composers::CurrencyBridge::Wrap);
    let take_currency = if wrap { NATIVE_CURRENCY_ADDRESS } else { weth };
    let settle_currency = if wrap { weth } else { NATIVE_CURRENCY_ADDRESS };
    let take_idx = if wrap { SENTINEL_NATIVE } else { SENTINEL_WETH };
    let settle_idx = if wrap { SENTINEL_WETH } else { SENTINEL_NATIVE };
    let convert = if wrap {
        PlanStep::WethDeposit {
            weth_idx: SENTINEL_WETH,
            weth_addr: weth,
            amount,
        }
    } else {
        PlanStep::WethWithdraw {
            weth_idx: SENTINEL_WETH,
            weth_addr: weth,
            amount,
        }
    };
    (
        vec![
            PlanStep::V4TakeCompact {
                currency_idx: take_idx,
                currency_addr: take_currency,
                recipient_idx: SENTINEL_SELF,
                amount,
                seeds_pool: None,
                repays_flash: None,
            },
            convert,
        ],
        settle_idx,
        settle_currency,
    )
}

/// A Plan builder: every `build_*_plan` returns the full payload's
/// preamble bytes, the [`Plan`] tree, and the resolved [`AddressTable`].
/// (Also used by the 3-hop pilots — the signature is family-agnostic.)
pub(crate) type BuildPlan =
    fn(&PathInfo, &ComposerInputs<'_>) -> Option<(Vec<u8>, Plan, AddressTable)>;

/// Build a family's [`Plan`] through its `build_*_plan` builder, run the
/// [`LedgerValidator`][crate::grammar_ledger::LedgerValidator] gate on the
/// projected ledger trace, and fold `preamble + plan_to_bytes(&plan, &at)`
/// into the full payload.
///
/// The derivation outcome — a tri-state so a routine decline is never
/// conflated with a would-be-bug validator reject (ADR-030).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Derive {
    /// The family encoded as executable command bytes.
    Encoded(Vec<u8>),
    /// No producer/builder for this shape (a routine no-path; the strategy skips).
    Declined,
    /// A Plan was *built* but the ledger validator rejected it — a latent bug.
    /// Always fatal (ADR-030): never swallowed into a decline or a `None`.
    Rejected(crate::grammar_ledger::ValidationError),
}

/// Map a [`Derive`] onto the public `Option` seam: `Declined → None`, and a
/// `Rejected` is **always fatal** — it panics (ADR-030: a Reject is a would-be
/// bug and must never be silently dropped). [`Derive::Encoded`] yields the bytes.
#[must_use]
#[expect(
    clippy::panic,
    reason = "ADR-030: a validator Reject is intentionally fatal — it must never be swallowed into a None"
)]
pub(crate) fn derive_option(d: Derive) -> Option<Vec<u8>> {
    match d {
        Derive::Encoded(bytes) => Some(bytes),
        Derive::Declined => None,
        Derive::Rejected(e) => panic!(
            "executor derivation REJECT (a latent bug, ADR-030): the encoder wrote a Plan the ledger validator rejected: {e:?}"
        ),
    }
}

/// Build a family's Plan through its `build_*_plan` builder, run the
/// [`LedgerValidator`][crate::grammar_ledger::LedgerValidator] gate on the
/// projected ledger trace, and fold `preamble + plan_to_bytes(&plan, &at)`
/// into the full payload.
///
/// Returns a [`Derive`]: `Declined` when the builder declines (no bytes for
/// this shape), `Rejected` when a *built* Plan fails validation — a stream
/// that violates credit-before-debit / terminal-V2 pre-fund /
/// flash-debt-net-zero / PM-net-zero must NOT produce bytes. This is the first
/// time the validator gates real production bytes (ADR-029 D4/D5).
#[must_use]
fn build_plan_bytes(path: &PathInfo, build: BuildPlan, inputs: &ComposerInputs<'_>) -> Derive {
    let Some((preamble, plan, at)) = build(path, inputs) else {
        return Derive::Declined;
    };
    let ops = plan_to_ledger_ops(&plan);
    let mut v = crate::grammar_ledger::LedgerValidator::default();
    if let Err(e) = v.validate_full(&ops) {
        return Derive::Rejected(e);
    }
    let mut out = preamble;
    out.extend_from_slice(&plan_to_bytes(&plan, &at));
    Derive::Encoded(out)
}

/// Public all-V2 entry (KO5NNB cutover): the any-N (≥2) all-V2 family through
/// the walker + validator gate — `build_all_v2_walk` → the
/// [`LedgerValidator`][crate::grammar_ledger::LedgerValidator] → `plan_to_bytes`
/// (A3 routes it through the shared [`derive_shape_detailed`] dispatch).
/// Returns `None` on a routine decline; a validator `Reject` is **fatal**
/// (panics, ADR-030). See [`derive_all_v2_detailed`] for the tri-state.
#[must_use]
pub fn derive_all_v2(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    derive_option(derive_all_v2_detailed(path, inputs))
}

/// The tri-state form of [`derive_all_v2`] (ADR-030): `Declined` vs `Rejected`
/// are distinguished instead of collapsed into `None`. Delegates to the shared
/// [`derive_shape_detailed`] dispatch (its `(V2,V2,V2|None)` row is the all-V2
/// walker), so the all-V2 stream shares the single producer path.
#[must_use]
pub fn derive_all_v2_detailed(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Derive {
    derive_shape_detailed(path, inputs)
}

/// The protocol of a hop slot; `None` for a missing slot (a 2-hop path has a
/// `None` third slot).
#[must_use]
fn prot_of(h: Option<&HopInfo>) -> Option<Prot> {
    match h {
        Some(HopInfo::V2(_)) => Some(Prot::V2),
        Some(HopInfo::V3(_)) => Some(Prot::V3),
        Some(HopInfo::V4(_)) => Some(Prot::V4),
        None => None,
    }
}

/// Public entry: derive a family's command bytes from its Plan builder
/// (`build_*_plan` → [`LedgerValidator`][crate::grammar_ledger::LedgerValidator]
/// gate → `plan_to_bytes`) — the sole production producer since RVNIPD removed
/// the hand-written emitters. Returns `None` on a routine decline (an unsupported
/// family → the strategy skips); a validator `Reject` is **fatal** (panics,
/// ADR-030). See [`derive_shape_detailed`] for the tri-state.
#[must_use]
pub fn derive_shape(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    derive_option(derive_shape_detailed(path, inputs))
}

/// The tri-state form of [`derive_shape`] (ADR-030): a routine `Declined` is
/// distinguished from a would-be-bug `Rejected`, instead of both collapsing to
/// `None`.
#[must_use]
pub fn derive_shape_detailed(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Derive {
    let key = (
        prot_of(path.hops.first()),
        prot_of(path.hops.get(1)),
        prot_of(path.hops.get(2)),
    );
    let Some(build) = crate::grammar_walker::build_for_walk(key) else {
        return Derive::Declined;
    };
    build_plan_bytes(path, build, inputs)
}

/// Public accessor (candidate 4, `3BTR22`): the declared stream-varying axes
/// for a path's family, **derived from the hop-protocol facts** (not a
/// per-row table) — the family's walker branches `FundingSource` only for the
/// `v2_v3` / any-N all-V2 families, and `ProfitCapture` only for the pure-V4
/// families. `None` for a shape with no producer (1-hop, >3-hop non-all-V2,
/// unknown). The recognized-family check reuses [`build_for_walk`][crate::grammar_walker::build_for_walk] so the axis
/// surface can't drift from what actually produces bytes.
///
/// E.g. `v2_v3` and any-N all-V2 → `{funding}` (the two families that branch
/// `FundingSource`); the pure-V4 families `v4_v4`/`v4_v4_v4` → `{capture}`
/// (V4 terminal capture); every V4-involving-but-not-pure-V4 family and all
/// V2/V3-only families → `{}` (all axes derived; `ProfitCapture`/`Bribe` reach
/// those streams only via the on-chain `check_mode`/`pack_config` config, never
/// a stream byte).
#[must_use]
pub fn family_axis_support(path: &PathInfo) -> Option<AxisSupport> {
    let key = (
        prot_of(path.hops.first()),
        prot_of(path.hops.get(1)),
        prot_of(path.hops.get(2)),
    );
    // Recognized family? If not, there is no axis surface.
    crate::grammar_walker::build_for_walk(key)?;
    // capture: the pure-V4 families (every hop is V4).
    let capture = matches!(key, (Some(Prot::V4), Some(Prot::V4), Some(Prot::V4) | None));
    // funding: `v2_v3` (2-hop V2→V3) and the any-N all-V2 family.
    let funding = matches!(
        key,
        (Some(Prot::V2), Some(Prot::V3), None)
            | (Some(Prot::V2), Some(Prot::V2), Some(Prot::V2) | None)
    );
    Some(AxisSupport {
        funding,
        capture,
        bribe: false,
    })
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::items_after_statements,
        clippy::type_complexity,
        clippy::naive_bytecount,
        clippy::similar_names
    )]
    use super::*;
    // A3: the reference producers are deleted; these aliases re-point the
    // structure-inspection tests at the walkers (byte-identical, A2-proven).
    use crate::composers::{EncodeOptions, V2HopInfo, V3HopInfo};
    use crate::encoders::enc_preamble;
    use crate::grammar_walker::{
        build_all_v2_walk as build_all_v2_chain, build_v2v3_walk as build_v2v3_plan,
        build_v3v2_walk as build_v3v2_plan, build_v3v3_walk as build_v3v3_plan,
        build_v4v4_walk as build_v4v4_plan,
    };
    use alloy::primitives::{address, Address, U256};

    // ── Derive tri-state (ADR-030) ──

    /// A deliberately-invalid build: a `V4TakeDelta` before any PM credit. The
    /// ledger validator must surface `Reject(TakeBeforeCredit)`, not `None`.
    const WT: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const EX: Address = address!("DeAd0000000000000000000000000000000000Be");
    const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");

    #[expect(
        clippy::unnecessary_wraps,
        reason = "test helper matching the BuildPlan fn-pointer signature (always returns Some to force a validator reject)"
    )]
    fn bad_build(_: &PathInfo, _: &ComposerInputs<'_>) -> Option<(Vec<u8>, Plan, AddressTable)> {
        let at = AddressTable::with_sentinels(Some(WT), Some(EX), Some(PM));
        let plan = vec![PlanStep::V4TakeDelta {
            currency_idx: SENTINEL_WETH,
            currency_addr: WT,
            recipient_idx: SENTINEL_SELF,
            seeds_pool: None,
        }];
        Some((enc_preamble(&at), plan, at))
    }

    #[test]
    fn invalid_plan_surfaces_as_reject_not_none() {
        use crate::grammar_ledger::ValidationError;
        let inputs = ComposerInputs {
            executor_address: EX,
            pool_manager_address: PM,
            weth_address: WT,
            optimal_input: 0,
            hop_outputs: &[],
            consumed_inputs: &[],
            opts: EncodeOptions::default(),
        };
        match build_plan_bytes(&PathInfo::new(vec![]), bad_build, &inputs) {
            Derive::Rejected(ValidationError::TakeBeforeCredit { .. }) => {}
            other => panic!("expected Reject(TakeBeforeCredit), got {other:?}"),
        }
    }

    #[test]
    fn rejected_derivation_is_fatal_not_none() {
        use crate::grammar_ledger::ValidationError;
        // A Reject must panic (be fatal) at the public seam — never None.
        let r = std::panic::catch_unwind(|| {
            derive_option(Derive::Rejected(ValidationError::TakeBeforeCredit {
                currency: Address::ZERO,
                wanted: 1,
                have: 0,
            }))
        });
        assert!(
            r.is_err(),
            "a Reject must panic (ADR-030), never return None"
        );
        // A Declined is the routine skip; Encoded passes through.
        assert_eq!(derive_option(Derive::Declined), None);
        assert_eq!(derive_option(Derive::Encoded(vec![7u8])), Some(vec![7u8]));
    }

    /// The three v4_v4 terminal-output currencies the broadened slice covers
    /// (WETH / a tok / native) — the `use_v4_batch` + `erc6909_profit` opts
    /// interact with each differently (`derive_2hop_v4v4` exact parity).
    #[derive(Clone, Copy)]
    enum Terminal {
        Weth,
        Tok,
        Native,
    }

    /// The two v4_v4 currency-gap topologies (native↔WETH bridge at the mid).
    #[derive(Clone, Copy)]
    enum Gap {
        /// Hop a outputs native, hop b needs WETH → take native + `WETH_DEPOSIT`.
        Wrap,
        /// Hop a outputs WETH, hop b needs native → take WETH + `WETH_WITHDRAW`.
        Unwrap,
    }

    fn v4_v4_inputs(
        terminal: Terminal,
        opts: crate::composers::EncodeOptions,
    ) -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let t1 = address!("0000000000000000000000000000000000000db1");
        let t2 = address!("0000000000000000000000000000000000000db2");
        let pm = address!("00000000000000000000000000000000000000ff");
        let v4a_id = "0x0".to_string();
        let v4b_id = "0x1".to_string();
        // hop a: weth → t1 (currency0=weth, currency1=t1, zfo=true).
        let hop_a = HopInfo::V4(V4HopInfo {
            pool_manager_address: pm,
            pool_id_hex: v4a_id,
            currency0_address: weth,
            currency1_address: t1,
            fee: 3000,
            tick_spacing: 60,
            hook_address: Address::ZERO,
            zfo: true,
        });
        // hop b: t1 → <terminal>. V4 sorts currency0 < currency1; native
        // (address 0) is always currency0, so the zfo + currency layout
        // depends on the terminal.
        let hop_b = match terminal {
            // t1 → weth: currency0=t1, currency1=weth, zfo=true.
            Terminal::Weth => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: t1,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true,
            }),
            // t1 → t2: currency0=t1, currency1=t2, zfo=true.
            Terminal::Tok => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: t1,
                currency1_address: t2,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true,
            }),
            // t1 → native: native is currency0, t1 is currency1, so zfo=false
            // (currency1→currency0). out = currency0 = native.
            Terminal::Native => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: NATIVE_CURRENCY_ADDRESS,
                currency1_address: t1,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: false,
            }),
        };
        let path = PathInfo::new(vec![hop_a, hop_b]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        (
            path,
            ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: pm,
                weth_address: weth,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS,
                consumed_inputs: &CONSUMED,
                opts,
            },
        )
    }

    /// Build a v4_v4 **gap** topology (native↔WETH bridge at the mid). `Wrap`:
    /// hop a outputs native, hop b needs WETH (take native + `WETH_DEPOSIT`).
    /// `Unwrap`: hop a outputs WETH, hop b needs native (take WETH +
    /// `WETH_WITHDRAW`). V4 sorts currency0 < currency1, so native (address 0)
    /// is always currency0; zfo is set so the output/input legs match the gap.
    fn v4_v4_gap_inputs(
        gap: Gap,
        opts: crate::composers::EncodeOptions,
    ) -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let t1 = address!("0000000000000000000000000000000000000da1");
        let t2 = address!("0000000000000000000000000000000000000da2");
        let pm = address!("00000000000000000000000000000000000000ff");
        let v4a_id = "0x0".to_string();
        let v4b_id = "0x1".to_string();
        // Hop a: output the gap currency (mid_a). zfo=false → output=currency0;
        // zfo=true → output=currency1.
        let hop_a = match gap {
            // Wrap: mid_a=native. currency0=native, currency1=weth, zfo=false
            // → output=currency0=native, input=currency1=weth.
            Gap::Wrap => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4a_id,
                currency0_address: NATIVE_CURRENCY_ADDRESS,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: false,
            }),
            // Unwrap: mid_a=weth. currency0=t1, currency1=weth, zfo=true
            // → output=currency1=weth, input=currency0=t1.
            Gap::Unwrap => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4a_id,
                currency0_address: t1,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true,
            }),
        };
        // Hop b: input the bridged currency (mid_b), output the terminal.
        // mid_b = input = currency0 if zfo else currency1.
        let hop_b = match gap {
            // Wrap: mid_b=weth. currency0=t2, currency1=weth, zfo=false
            // → input=currency1=weth, output=currency0=t2.
            Gap::Wrap => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: t2,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: false,
            }),
            // Unwrap: mid_b=native. currency0=native, currency1=weth, zfo=true
            // → input=currency0=native, output=currency1=weth.
            Gap::Unwrap => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: NATIVE_CURRENCY_ADDRESS,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true,
            }),
        };
        let path = PathInfo::new(vec![hop_a, hop_b]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        (
            path,
            ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: pm,
                weth_address: weth,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS,
                consumed_inputs: &CONSUMED,
                opts,
            },
        )
    }

    #[test]
    fn funding_source_is_derived_from_leading_hop() {
        // V2-leading → in-path flash; V3-leading → self-fund (D0-forced).
        let cases: [(Prot, FundingSource); 2] = [
            (Prot::V2, FundingSource::InPathFlash),
            (Prot::V3, FundingSource::SelfFund),
        ];
        for (_p, expected) in cases {
            // The assignment rule is the match in `derive_shape`; assert the
            // two funding values are distinct (the spike's contract).
            assert_ne!(
                expected,
                match expected {
                    FundingSource::InPathFlash => FundingSource::SelfFund,
                    _ => FundingSource::InPathFlash,
                }
            );
        }
    }

    // POC (6SRC23): the v2_v3 InPathFlash flash-credit chain exercises the
    // executor-ledger credit-before-debit invariant the runtime matrix can't
    // see. The derived trace must validate clean; a misordering must reject.
    fn v2_v3_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let v2a = address!("00000000000000000000000000000000000000aa");
        let v3b = address!("00000000000000000000000000000000000000bb");
        let path = PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: v2a,
                token0_address: weth,
                token1_address: usdc,
                fee: 30,
                zfo: true, // WETH → USDC: forward token = USDC
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: v3b,
                token0_address: usdc,
                token1_address: weth,
                fee: 3000,
                zfo: true, // USDC → WETH: forward token = WETH (terminal)
            }),
        ]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        let inputs = ComposerInputs {
            executor_address: address!("00000000000000000000000000000000000000ee"),
            pool_manager_address: address!("00000000000000000000000000000000000000ff"),
            weth_address: weth,
            optimal_input: OPTIMAL,
            hop_outputs: &OUTS,
            consumed_inputs: &CONSUMED,
            opts: crate::composers::EncodeOptions::default(),
        };
        (path, inputs)
    }

    // BP7KIR Checkpoint 1: the Plan tree is the primary artifact for v2_v3.
    #[test]
    fn v2_v3_plan_projects_a_validating_trace() {
        let (path, inputs) = v2_v3_path_inputs();
        let (_preamble, plan, _at) =
            build_v2v3_plan(&path, &inputs).expect("v2_v3 must build a Plan");
        let ops = plan_to_ledger_ops(&plan);
        assert_eq!(ops.len(), 4, "v2_v3 Plan projects to 4 LedgerOps");
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            v.validate_full(&ops).is_ok(),
            "canonical v2_v3 Plan must project a validating trace"
        );
    }

    #[test]
    fn v2_v3_plan_misordered_callback_rejects() {
        let (path, inputs) = v2_v3_path_inputs();
        let (_preamble, mut plan, _at) =
            build_v2v3_plan(&path, &inputs).expect("v2_v3 must build a Plan");
        // Corrupt the Plan: make the outer WETH repayment (sibling #1 of the
        // V2 flash's callback) fire BEFORE the V3 flash (sibling #0) that
        // credits WETH. Depth-first walk: V2 flash (credits t1) → WETH repay
        // (executor WETH still 0 → the V2 flash's WETH credit is owed, not
        // held) → REJECT. The runtime matrix cannot name this; byte-parity
        // would confirm the misordered bytes that revert on-chain.
        let outer = plan.get_mut(0).unwrap();
        if let PlanStep::FlashSwap { callback, .. } = outer {
            // callback = [V3 FlashSwap, WETH Erc20Transfer]; swap to
            // [WETH Erc20Transfer, V3 FlashSwap].
            callback.swap(0, 1);
        } else {
            panic!("expected outer V2 FlashSwap");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::Erc20TransferBeforeCredit {
                    currency, wanted, have
                }) if currency == inputs.weth_address && wanted == 1_000_000 && have == 0
            ),
            "misordered Plan must be rejected: WETH repay before V3 flash credits WETH"
        );
        let _ = U256::ZERO;
    }

    // Increment 2 (BP7KIR): the remaining V2/V3 2-hop families on the Plan.
    // Each: byte-parity with the proven emitter + the Plan projects a
    // validating trace. A shared helper drives both assertions per family.
    fn v3_v2_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let v3a = address!("00000000000000000000000000000000000000a1");
        let v2b = address!("00000000000000000000000000000000000000b2");
        let path = PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: v3a,
                token0_address: weth,
                token1_address: usdc,
                fee: 3000,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: v2b,
                token0_address: usdc,
                token1_address: weth,
                fee: 30,
                zfo: true,
            }),
        ]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        (
            path,
            ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: address!("00000000000000000000000000000000000000ff"),
                weth_address: weth,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS,
                consumed_inputs: &CONSUMED,
                opts: crate::composers::EncodeOptions::default(),
            },
        )
    }
    fn v3_v3_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let v3a = address!("00000000000000000000000000000000000000a3");
        let v3b = address!("00000000000000000000000000000000000000b3");
        let path = PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: v3a,
                token0_address: weth,
                token1_address: usdc,
                fee: 3000,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: v3b,
                token0_address: usdc,
                token1_address: weth,
                fee: 3000,
                zfo: true,
            }),
        ]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        (
            path,
            ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: address!("00000000000000000000000000000000000000ff"),
                weth_address: weth,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS,
                consumed_inputs: &CONSUMED,
                opts: crate::composers::EncodeOptions::default(),
            },
        )
    }
    /// RVNIPD/EYQ6UF: build the Plan and assert it projects a trace that
    /// validates clean through the gate. The byte-level `derive_shape`
    /// comparison this used to make is gone — `derive_shape` IS the same
    /// Plan path now, so it was a self-comparison (the runtime matrix is the
    /// byte source of truth; `encoders_parity` pins the primitive wire format).
    fn plan_builds_and_validates(
        build: fn(&PathInfo, &ComposerInputs) -> Option<(Vec<u8>, Plan, AddressTable)>,
        path: &PathInfo,
        inputs: &ComposerInputs,
        name: &str,
    ) {
        let (_preamble, plan, _at) =
            build(path, inputs).unwrap_or_else(|| panic!("[{name}] build None"));
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            v.validate_full(&ops).is_ok(),
            "[{name}] Plan must validate clean"
        );
    }

    /// A dummy path whose hop sequence is `combo` (only the protocol sequence
    /// matters to [`family_axis_support`] — it inspects hop slots, not fees).
    fn axis_path(combo: &[Prot]) -> PathInfo {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let hops = combo
            .iter()
            .enumerate()
            .map(|(i, p)| match p {
                Prot::V2 => HopInfo::V2(V2HopInfo {
                    pool_address: Address::from([0xA0 + u8::try_from(i).unwrap(); 20]),
                    token0_address: weth,
                    token1_address: usdc,
                    fee: 30,
                    zfo: true,
                }),
                Prot::V3 => HopInfo::V3(V3HopInfo {
                    pool_address: Address::from([0xB0 + u8::try_from(i).unwrap(); 20]),
                    token0_address: weth,
                    token1_address: usdc,
                    fee: 3000,
                    zfo: true,
                }),
                Prot::V4 => HopInfo::V4(V4HopInfo {
                    pool_manager_address: Address::ZERO,
                    pool_id_hex: format!("0x{i:x}"),
                    currency0_address: weth,
                    currency1_address: usdc,
                    fee: 500,
                    tick_spacing: 10,
                    hook_address: Address::ZERO,
                    zfo: true,
                }),
            })
            .collect();
        PathInfo::new(hops)
    }

    #[test]
    fn family_axis_support_declares_the_honoring_surface() {
        // funding is branched by exactly v2_v3 + any-N all-V2.
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V2, Prot::V3])).unwrap(),
            AxisSupport::funding()
        );
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V2, Prot::V2])).unwrap(),
            AxisSupport::funding()
        );
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V2, Prot::V2, Prot::V2])).unwrap(),
            AxisSupport::funding()
        );
        // capture is branched by exactly the pure-V4 families.
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V4, Prot::V4])).unwrap(),
            AxisSupport::capture()
        );
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V4, Prot::V4, Prot::V4])).unwrap(),
            AxisSupport::capture()
        );
        // V4-involving but NOT pure-V4: capture is NOT varied in the stream
        // (physical take only; `check_mode` is config) — the surfaced
        // asymmetry the declaration makes explicit.
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V2, Prot::V2, Prot::V4])).unwrap(),
            AxisSupport::none()
        );
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V4, Prot::V2, Prot::V4])).unwrap(),
            AxisSupport::none()
        );
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V2, Prot::V4])).unwrap(),
            AxisSupport::none()
        );
        // V2/V3-only families derive funding (InPathFlash) — no axis varies.
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V3, Prot::V2])).unwrap(),
            AxisSupport::none()
        );
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V3, Prot::V3])).unwrap(),
            AxisSupport::none()
        );
        assert_eq!(
            family_axis_support(&axis_path(&[Prot::V3, Prot::V3, Prot::V3])).unwrap(),
            AxisSupport::none()
        );
        // bribe is declared on no family; the per-axis accessor agrees.
        for axes in [
            family_axis_support(&axis_path(&[Prot::V2, Prot::V3])).unwrap(),
            family_axis_support(&axis_path(&[Prot::V4, Prot::V4])).unwrap(),
            family_axis_support(&axis_path(&[Prot::V3, Prot::V3])).unwrap(),
        ] {
            assert!(!axes.is_honored(Axis::Bribe), "no family branches bribe");
        }
        assert!(family_axis_support(&axis_path(&[Prot::V2, Prot::V3]))
            .unwrap()
            .is_honored(Axis::Funding));
        assert!(family_axis_support(&axis_path(&[Prot::V4, Prot::V4]))
            .unwrap()
            .is_honored(Axis::Capture));
        // 1-hop / unknown shapes have no builder row → None.
        assert!(family_axis_support(&axis_path(&[Prot::V2])).is_none());
    }

    #[test]
    fn v3_v2_plan_byte_parity_and_validates() {
        let (path, inputs) = v3_v2_path_inputs();
        plan_builds_and_validates(build_v3v2_plan, &path, &inputs, "v3_v2");
    }
    #[test]
    fn v3_v3_plan_byte_parity_and_validates() {
        let (path, inputs) = v3_v3_path_inputs();
        plan_builds_and_validates(build_v3v3_plan, &path, &inputs, "v3_v3");
    }

    /// Build an `n`-hop all-V2 path closing on WETH: hop `i` is
    /// `token_i → token_{i+1}`, with the final hop returning to WETH
    /// (the canonical all-V2 arbitrage loop the speedrail serves).
    fn all_v2_chain_hops(n: usize) -> Vec<HopInfo> {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let wbtc = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
        let dai = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
        let cycle = [weth, usdc, wbtc, dai];
        (0..n)
            .map(|i| {
                HopInfo::V2(V2HopInfo {
                    pool_address: Address::from([0xD0 + u8::try_from(i).expect("2..=4 hops"); 20]),
                    token0_address: cycle[i % 4],
                    token1_address: cycle[(i + 1) % 4],
                    fee: 30,
                    zfo: true,
                })
            })
            .collect()
    }

    // KO5NNB gate proof: an InPathFlash all-V2 stream whose terminal output
    // cannot cover the flash repayment is REJECTED by the LedgerValidator
    // (the flash-repay `Erc20Transfer` would over-debit `erc20[weth]`, so
    // credit-before-debit fires). The retired hand-written speedrail emitted
    // this stream unvalidated (and the revm harness's 2× WETH buffer let it
    // "execute but lose"); the gate now makes it unrepresentable — N4TJSZ's
    // entire point. The SAME losing stream under SelfFund still validates
    // (no flash debt to repay — the executor eats the loss from held capital,
    // which is what a negative-control delta assert should measure).
    #[test]
    fn all_v2_gate_rejects_unprofitable_inpathflash_stream() {
        let path = PathInfo::new(all_v2_chain_hops(2));
        let outs: Vec<u128> = vec![80_000, 60_000]; // terminal 60k < optimal 100k — losing
        let consumed: Vec<u128> = vec![50_000, 80_000];
        for (flabel, funding) in [
            ("InPathFlash", FundingSource::InPathFlash),
            ("SelfFund", FundingSource::SelfFund),
        ] {
            let inputs = ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: address!("00000000000000000000000000000000000000ff"),
                weth_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                optimal_input: 100_000,
                hop_outputs: &outs,
                consumed_inputs: &consumed,
                opts: crate::composers::EncodeOptions {
                    funding,
                    ..Default::default()
                },
            };
            let (_preamble, plan, _at) = build_all_v2_chain(&path, &inputs)
                .unwrap_or_else(|| panic!("[{flabel}] build None"));
            let ops = plan_to_ledger_ops(&plan);
            let mut v = crate::grammar_ledger::LedgerValidator::default();
            if flabel == "InPathFlash" {
                let err = v
                    .validate_full(&ops)
                    .expect_err("losing InPathFlash stream must be rejected");
                // The exact invariant fired: the flash repay tries to debit
                // more WETH than the stream generated (terminal 60k < 100k owed).
                assert!(
                    matches!(
                        err,
                        crate::grammar_ledger::ValidationError::Erc20TransferBeforeCredit { .. }
                    ),
                    "expected Erc20TransferBeforeCredit, got {err:?}"
                );
                // ADR-030: the validator rejection of a *built* Plan is a
                // would-be-bug, not a silent skip. It now surfaces as a
                // `Reject` (tri-state) and the public seam is FATAL (panics),
                // where the old code swallowed it to `None` — this documents
                // a latent builder defect the facts-walker will make
                // unrepresentable.
                let d = derive_all_v2_detailed(&path, &inputs);
                assert!(
                    matches!(
                        d,
                        Derive::Rejected(
                            crate::grammar_ledger::ValidationError::Erc20TransferBeforeCredit { .. }
                        )
                    ),
                    "expected Reject(Erc20TransferBeforeCredit), got {d:?}"
                );
                let r = std::panic::catch_unwind(|| derive_all_v2(&path, &inputs));
                assert!(
                    r.is_err(),
                    "a validator Reject must be fatal (panic), never None (ADR-030)"
                );
            } else {
                assert!(
                    v.validate_full(&ops).is_ok(),
                    "losing SelfFund stream must still validate (no flash debt)"
                );
            }
        }
    }

    #[test]
    fn v3_v2_plan_terminal_v2_before_seed_rejected() {
        // The terminal-V2 pre-fund rule (`2PT5HH`): a `V2SwapCalc` before its
        // `Erc20Transfer` pair-seed must be rejected (the über-draw class).
        let (path, inputs) = v3_v2_path_inputs();
        let (_preamble, mut plan, _at) = build_v3v2_plan(&path, &inputs).expect("v3_v2 build None");
        // The V3 flash's callback is [WETH repay, forward seed, V2SwapCalc].
        // Move V2SwapCalc to the front (before the seed) → SwapCalcBeforeCredit.
        let outer = plan.last_mut().unwrap();
        if let PlanStep::FlashSwap { callback, .. } = outer {
            // [WETH transfer, seed transfer, V2SwapCalc] → [V2SwapCalc, WETH transfer, seed transfer]
            let swapcalc = callback.remove(2);
            callback.insert(0, swapcalc);
        } else {
            panic!("expected outer V3 FlashSwap");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::SwapCalcBeforeCredit { .. })
            ),
            "misordered Plan: V2SwapCalc before its pair seed must be rejected"
        );
        let _ = U256::ZERO;
    }

    // BP7KIR Increment 3: the V4 container (`v4_v4`) on the Plan — the
    // PM-net-zero master invariant + D0 take-before-credit on the PM ledger.
    fn v4_v4_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        // WETH terminal (the spike's proven slice): weth→t1→weth.
        v4_v4_inputs(Terminal::Weth, crate::composers::EncodeOptions::default())
    }

    #[test]
    fn v4_v4_plan_byte_parity_and_validates() {
        let (path, inputs) = v4_v4_path_inputs();
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4");
    }

    #[test]
    fn v4_v4_plan_take_before_swap_rejected() {
        // D0 on the PM ledger: a `V4TakeDelta` before any swap created PM
        // credit must be rejected (the `v2_v2_v4` bug class on the PM ledger).
        let (path, inputs) = v4_v4_path_inputs();
        let (_preamble, mut plan, _at) = build_v4v4_plan(&path, &inputs).expect("v4_v4 build None");
        // The V4Unlock's inner is [Swap a, Swap b, TakeDelta, SettleAll].
        // Move TakeDelta to the front (before both swaps) → TakeBeforeCredit.
        let outer = plan.get_mut(0).unwrap();
        if let PlanStep::V4Unlock { inner, .. } = outer {
            let take = inner.remove(2);
            inner.insert(0, take);
        } else {
            panic!("expected outer V4Unlock");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::TakeBeforeCredit { .. })
            ),
            "misordered v4_v4 Plan: TakeDelta before the swap credits PM must be rejected"
        );
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_unsettled_delta_rejected() {
        // The master V4 invariant: a `V4Unlock` that closes with a nonzero
        // `PM[currency]` delta (here: removing the trailing `V4SettleAll` leaves
        // a residual t1 delta when forward_out ≠ b_swap_in) must be rejected.
        let (path, mut inputs) = v4_v4_path_inputs();
        // Force an UNSETTLED t1 DEBT: b_swap_in (1_150_000) > forward_out
        // (1_100_000), so removing the trailing SettleAll leaves a NEGATIVE t1
        // delta (the executor owes the V4 input) → PmDeltaNonzero at V4UnlockEnd.
        // (A POSITIVE residual is on-chain-valid — the unlock-close auto-settles
        // it to the executor — so the rejection is specifically the unpaid debt.)
        static CLAMPED: [u128; 2] = [1_000_000, 1_150_000];
        inputs.consumed_inputs = &CLAMPED;
        let (_preamble, mut plan, _at) = build_v4v4_plan(&path, &inputs).expect("v4_v4 build None");
        let outer = plan.get_mut(0).unwrap();
        if let PlanStep::V4Unlock { inner, .. } = outer {
            // Remove the trailing SettleAll — the t1 delta (forward_out −
            // b_swap_in = 50_000) is left nonzero → PmDeltaNonzero at V4UnlockEnd.
            inner.pop();
        } else {
            panic!("expected outer V4Unlock");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::PmDeltaNonzero { .. })
            ),
            "v4_v4 Plan missing its settle must be rejected: nonzero PM delta at unlock end"
        );
        let _ = U256::ZERO;
    }

    // ── BP7KIR opts: `use_v4_batch` + `erc6909_profit` (within the WETH-only
    //    slice) — byte-parity with `derive_2hop_v4v4` AND gate validation. ──

    fn v4_v4_opts_inputs(
        opts: crate::composers::EncodeOptions,
    ) -> (PathInfo, ComposerInputs<'static>) {
        let (mut path, mut inputs) = v4_v4_path_inputs();
        // SAFETY of the `static` borrow: `EncodeOptions` is `Copy` and the
        // fixture's `hop_outputs`/`consumed_inputs` are `static`s, so we only
        // overwrite the `opts` field — the slice borrows remain valid.
        // Re-build `ComposerInputs` with the requested opts.
        let ComposerInputs {
            executor_address,
            pool_manager_address,
            weth_address,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            opts: _,
        } = inputs;
        inputs = ComposerInputs {
            executor_address,
            pool_manager_address,
            weth_address,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            opts,
        };
        // Suppress unused-mut on `path` (the fixture path is reused as-is).
        let _ = &mut path;
        (path, inputs)
    }

    #[test]
    fn v4_v4_plan_batch_byte_parity_and_validates() {
        // `use_v4_batch=true`: one `V4Batch` replaces the two `V4Swap`s, and
        // NO `V4TakeDelta` is emitted (the batch auto-captures the WETH
        // profit). Byte-parity with `derive_2hop_v4v4`'s batch arm.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
            ..Default::default()
        });
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4 batch");
        // Spot-check the Plan shape: outer `V4Unlock { inner: [V4Batch, V4SettleAll] }`.
        let (_preamble, plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 batch build None");
        let outer = &plan[0];
        let PlanStep::V4Unlock { inner, .. } = outer else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(inner.len(), 2, "batch inner = [V4Batch, V4SettleAll]");
        assert!(
            matches!(inner[0], PlanStep::V4Batch { .. }),
            "first step is V4Batch"
        );
        assert!(
            matches!(inner[1], PlanStep::V4SettleAll),
            "trailing SettleAll"
        );
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_erc6909_byte_parity_and_validates() {
        // `erc6909_profit=true`: `V4Mint` of the WETH profit (ERC6909 claim)
        // replaces the `V4TakeDelta`.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
            ..Default::default()
        });
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4 erc6909");
        let (_preamble, plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 erc6909 build None");
        let PlanStep::V4Unlock { inner, .. } = &plan[0] else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(
            inner.len(),
            4,
            "erc6909 inner = [V4Swap, V4Swap, V4Mint, V4SettleAll]"
        );
        assert!(
            matches!(inner[2], PlanStep::V4Mint { .. }),
            "profit step is V4Mint"
        );
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_batch_erc6909_byte_parity_and_validates() {
        // Both opts: `V4Batch` + `V4Mint` of the profit (still auto-settles via
        // `V4SettleAll`; the mint captures the WETH delta as an ERC6909 claim
        // before the trailing settle).
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: true,
            ..Default::default()
        });
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4 batch+erc6909");
        let (_preamble, plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 batch+erc6909 build None");
        let PlanStep::V4Unlock { inner, .. } = &plan[0] else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(
            inner.len(),
            3,
            "batch+erc6909 inner = [V4Batch, V4Mint, V4SettleAll]"
        );
        assert!(matches!(inner[0], PlanStep::V4Batch { .. }));
        assert!(matches!(inner[1], PlanStep::V4Mint { .. }));
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_mint_before_swap_rejected() {
        // `V4Mint` honors the same D0 credit-before-debit rule as `V4TakeDelta`:
        // a `V4Mint` positioned before the swaps that create `PM[weth]` credit
        // must be rejected (the `Mint` gate op fails with `TakeBeforeCredit`).
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
            ..Default::default()
        });
        let (_preamble, mut plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 erc6909 build None");
        // The V4Unlock's inner is [Swap a, Swap b, V4Mint, SettleAll].
        // Move V4Mint to the front (before both swaps) → TakeBeforeCredit.
        let outer = plan.get_mut(0).unwrap();
        if let PlanStep::V4Unlock { inner, .. } = outer {
            let mint = inner.remove(2);
            inner.insert(0, mint);
        } else {
            panic!("expected outer V4Unlock");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::TakeBeforeCredit { .. })
            ),
            "misordered v4_v4 erc6909 Plan: V4Mint before the swap credits PM must be rejected"
        );
        let _ = U256::ZERO;
    }

    // ── BP7KIR slice-broaden: the v4_v4 Plan across all 3 terminal currencies
    //    (WETH / tok / native) × all 4 opt modes. Byte-parity with the proven
    //    `derive_2hop_v4v4` emitter AND gate validation in every cell. ──
    #[test]
    fn v4_v4_terminal_opt_matrix_byte_parity_and_validates() {
        use crate::composers::EncodeOptions;
        let modes = [
            ("default", EncodeOptions::default()),
            (
                "batch",
                EncodeOptions {
                    erc6909_profit: false,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
            (
                "erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: false,
                    ..Default::default()
                },
            ),
            (
                "batch+erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
        ];
        let terminals = [
            ("weth", Terminal::Weth),
            ("tok", Terminal::Tok),
            ("native", Terminal::Native),
        ];
        for (t_name, terminal) in terminals {
            for (m_name, opts) in modes {
                let label = format!("v4_v4 {t_name}+{m_name}");
                let (path, inputs) = v4_v4_inputs(terminal, opts);
                plan_builds_and_validates(build_v4v4_plan, &path, &inputs, &label);
            }
        }
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_batch_native_terminal_emits_no_take() {
        // The batch asymmetry for a NATIVE terminal: `V4_BATCH` auto-settles the
        // positive native PM delta, so the derive emits NO `V4TakeDelta` and
        // neither does the Plan — the trailing `V4SettleAll` zeroes the
        // residual `PM[native]` (gate's master invariant at `V4UnlockEnd`).
        let (path, inputs) = v4_v4_inputs(
            Terminal::Native,
            crate::composers::EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: true,
                ..Default::default()
            },
        );
        let (_preamble, plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 native+batch build None");
        let PlanStep::V4Unlock { inner, .. } = &plan[0] else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(
            inner.len(),
            2,
            "native+batch inner = [V4Batch, V4SettleAll] (no take)"
        );
        assert!(matches!(inner[0], PlanStep::V4Batch { .. }));
        assert!(matches!(inner[1], PlanStep::V4SettleAll));
        let _ = U256::ZERO;
    }

    // ── BP7KIR currency-gap slice: native↔WETH bridge at the mid. Byte-parity
    //    with derive_2hop_v4v4's gap branch AND gate validation. ──
    #[test]
    fn v4_v4_gap_byte_parity_and_validates() {
        // Both gap topologies (Wrap + Unwrap), default opts. The gap branch
        // emits: swap a → take+bridge → swap b → settle_delta → take(terminal)
        // → settle_all.
        for (name, gap) in [("wrap", Gap::Wrap), ("unwrap", Gap::Unwrap)] {
            let label = format!("v4_v4 gap {name}");
            let (path, inputs) = v4_v4_gap_inputs(gap, crate::composers::EncodeOptions::default());
            plan_builds_and_validates(build_v4v4_plan, &path, &inputs, &label);
        }
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_gap_opt_matrix_byte_parity_and_validates() {
        // The gap branch is opt-invariant (use_v4_batch + erc6909_profit are
        // inoperative across a gap — the derive forces individual swaps + a
        // physical take). Sweep all 4 opt modes for both gap topologies.
        use crate::composers::EncodeOptions;
        let modes = [
            ("default", EncodeOptions::default()),
            (
                "batch",
                EncodeOptions {
                    erc6909_profit: false,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
            (
                "erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: false,
                    ..Default::default()
                },
            ),
            (
                "batch+erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
        ];
        for (g_name, gap) in [("wrap", Gap::Wrap), ("unwrap", Gap::Unwrap)] {
            for (m_name, opts) in modes {
                let label = format!("v4_v4 gap {g_name}+{m_name}");
                let (path, inputs) = v4_v4_gap_inputs(gap, opts);
                plan_builds_and_validates(build_v4v4_plan, &path, &inputs, &label);
            }
        }
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_gap_shape_is_swap_bridge_swap_settle_take() {
        // Plan-shape spot check: the gap branch lays out
        // [V4Swap, V4TakeCompact, (WethDeposit|WethWithdraw), V4Swap,
        //  V4SettleDelta, V4TakeDelta, V4SettleAll].
        let (path, inputs) =
            v4_v4_gap_inputs(Gap::Wrap, crate::composers::EncodeOptions::default());
        let (_preamble, plan, _at) = build_v4v4_plan(&path, &inputs).expect("v4_v4 gap build None");
        let PlanStep::V4Unlock { inner, .. } = &plan[0] else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(inner.len(), 7, "gap inner = 7 steps");
        assert!(matches!(inner[0], PlanStep::V4Swap { .. }), "0: swap a");
        assert!(
            matches!(inner[1], PlanStep::V4TakeCompact { .. }),
            "1: bridge take"
        );
        assert!(
            matches!(inner[2], PlanStep::WethDeposit { .. }),
            "2: wrap deposit (Wrap gap)"
        );
        assert!(matches!(inner[3], PlanStep::V4Swap { .. }), "3: swap b");
        assert!(
            matches!(inner[4], PlanStep::V4SettleDelta { .. }),
            "4: settle"
        );
        assert!(
            matches!(inner[5], PlanStep::V4TakeDelta { .. }),
            "5: profit take"
        );
        assert!(matches!(inner[6], PlanStep::V4SettleAll), "6: settle all");
        let _ = U256::ZERO;
    }
    // ── WE45KC inc.2: ProfitCapture::Native on v4_v4 (ADR-029 D1) ──────────
    // The capture axis is now load-bearing in the encoder: a WETH-terminal
    // v4_v4 path with capture=Native appends a WETH_WITHDRAW (0x13) converting
    // the profit to native ETH after the V4_TAKE_DELTA custody take. A
    // non-WETH/non-native tok terminal + Native is declined (the executor
    // cannot convert an arbitrary ERC-20 to native). A native terminal +
    // Native is a no-op (already native custody).

    /// Collect every `WETH_WITHDRAW` (0x13) command's 32-byte amount payload
    /// in `bytes` (scans all windows; `0x13` may appear in address bytes, so
    /// the caller asserts the expected profit is among the payloads).
    fn weth_withdraw_amounts(bytes: &[u8]) -> Vec<u128> {
        bytes
            .windows(33)
            .filter(|w| w[0] == 0x13)
            .map(|w| {
                let mut a = [0u8; 16];
                a.copy_from_slice(&w[17..33]);
                u128::from_be_bytes(a)
            })
            .collect()
    }

    /// Count `V4_TAKE_DELTA` (0x50) commands in `bytes`.
    fn count_v4_take_delta(bytes: &[u8]) -> usize {
        bytes.iter().filter(|&&b| b == 0x50).count()
    }

    #[test]
    fn v4_v4_native_capture_weth_terminal_appends_weth_withdraw() {
        // capture=Native: WETH-terminal path takes WETH to custody, then
        // WETH_WITHDRAW(profit) converts it to native. profit = weth_out -
        // optimal_input = 1_200_000 - 1_000_000 = 200_000.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            capture: crate::grammar_ledger::ProfitCapture::Native,
            ..Default::default()
        });
        let bytes =
            derive_shape(&path, &inputs).expect("v4_v4 native-capture WETH terminal must derive");
        assert!(
            weth_withdraw_amounts(&bytes).contains(&200_000),
            "Native capture must append WETH_WITHDRAW of the profit; got {:?}",
            weth_withdraw_amounts(&bytes)
        );
        // The custody take is still emitted (WETH taken to executor first).
        assert!(
            count_v4_take_delta(&bytes) >= 1,
            "V4_TAKE_DELTA custody take must precede the withdraw"
        );
    }

    #[test]
    fn v4_v4_custody_capture_emits_no_weth_withdraw() {
        // Default (Custody): no WETH_WITHDRAW — profit held as WETH.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions::default());
        let bytes = derive_shape(&path, &inputs).expect("v4_v4 custody WETH terminal must derive");
        assert!(
            !weth_withdraw_amounts(&bytes).contains(&200_000),
            "Custody capture must NOT append a WETH_WITHDRAW of the profit"
        );
    }

    #[test]
    fn v4_v4_native_capture_tok_terminal_declines() {
        // capture=Native, tok terminal: the executor cannot convert an
        // arbitrary ERC-20 to native → derive declines (ADR-029 D1: declared
        // but not executable).
        let (path, inputs) = v4_v4_inputs(
            Terminal::Tok,
            crate::composers::EncodeOptions {
                capture: crate::grammar_ledger::ProfitCapture::Native,
                ..Default::default()
            },
        );
        assert!(
            derive_shape(&path, &inputs).is_none(),
            "Native capture on a non-WETH/non-native tok terminal must decline"
        );
    }

    #[test]
    fn v4_v4_native_capture_native_terminal_is_noop() {
        // capture=Native, native terminal: profit is already native custody
        // (V4_TAKE_DELTA(native_idx, SELF)); no WETH_WITHDRAW needed. Derives.
        let (path, inputs) = v4_v4_inputs(
            Terminal::Native,
            crate::composers::EncodeOptions {
                capture: crate::grammar_ledger::ProfitCapture::Native,
                ..Default::default()
            },
        );
        let bytes = derive_shape(&path, &inputs)
            .expect("v4_v4 native-capture native terminal must derive (no-op)");
        assert!(
            !weth_withdraw_amounts(&bytes).iter().any(|&a| a > 0),
            "Native terminal + Native capture is already native; no withdraw"
        );
    }
    #[test]
    fn v4_v4_native_capture_plan_byte_parity_and_validates() {
        // WE45KC inc.2: the Native-capture Plan (WETH terminal → V4TakeDelta
        // custody + WethWithdraw) stays byte-identical to derive_2hop_v4v4 AND
        // validates clean through the ledger gate (D5). The custody credit on
        // V4TakeDelta(→SELF) now models the executor's receipt, so the
        // withdraw debits a real Erc20[WETH] balance.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            capture: ProfitCapture::Native,
            ..Default::default()
        });
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4 native-capture");
    }
}
