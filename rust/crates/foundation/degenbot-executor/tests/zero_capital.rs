//! Zero-capital solvency: the `InitialCapital` policy's derivation outcomes.
//!
//! The zero-capital stance (`EncodeOptions::initial_capital`) asks one question —
//! *can this built Plan run with no pre-existing token balance?* — and the
//! ledger answers it in ONE `LedgerValidator::validate_capital` pass over the
//! projected `LedgerOp` trace. There is no family-, currency-, or
//! closure-specific pre-check left: the ledger already distinguishes a
//! `SelfFund` seed (an external capital assumption) from path-generated credit
//! (`Erc20Credit`, V2/V3 flash output, `SwapCalc` output).
//!
//! This test pins the 27 three-hop `{V2,V3,V4}³` permutations in both amount
//! arms:
//!
//! * **profitable** — every permutation still ENCODES, and its bytes are
//!   byte-identical to the production (`InitialCapital::Held`) encode, so the
//!   zero-capital stance can never change a valid stream;
//! * **losing** — EVERY one reports the typed
//!   `InsolventZeroCapital { asset, required, available }` deficit, and the
//!   public `Option` seam declines it (never panics). The four terminal-V4
//!   captures that used to decline one step earlier, at Plan BUILD — their
//!   builder modelled the capture as `output − entry`, so a negative capture
//!   had no `PlanStep` at all — now record it as the ledger-only
//!   `PlanStep::CaptureShortfall` declared at the head of the Plan, and the one
//!   capital-policy evaluation answers them like the other 23.
//!   `BUILD_DECLINING_LOSING_PERMUTATIONS` is empty and must stay empty.
//!
//! Production classification is pinned beside them: the same losing streams
//! under `InitialCapital::Held` keep today's outcomes — a validator `Rejected`
//! is still fatal (ADR-030), a self-funded losing stream still encodes so it
//! executes to its economic receipt (the negative-control delta assert), and
//! the four declared-capture families still DECLINE (the stream would spend
//! entry capital no `SelfFund` seed declares), never panic.
//!
//! The self-funded arm has a third obligation the zero-capital arm does not: a
//! `SelfFund` stream *spends* the entry capital a flash stream borrows, so its
//! Plan must DECLARE it — otherwise the capital override installs nothing and the
//! payment reverts inside a WETH `transfer`. The section below pins that
//! declaration against the ledger for all 27 losing permutations, pins the
//! V4-first amount to the zero-capital deficit's `required - available`, pins
//! that a solvent Plan gains nothing, and pins that the declaration adds no
//! command byte.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::panic,
    clippy::similar_names,
    clippy::unwrap_used
)]

use alloy::primitives::{address, Address};
use degenbot_execution::{CapitalRequirement, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
use degenbot_executor::composers::{
    encode_cmd_stream, encode_cmd_stream_capital, ComposerInputs, EncodeContext, EncodeOptions,
    EncodeRequest,
};
use degenbot_executor::grammar_ledger::{
    CapitalError, FundingSource, InitialCapital, InsolventZeroCapital, LedgerValidator,
    ValidationError,
};
use degenbot_executor::grammar_plan::{
    plan_external_capital, plan_to_bytes, plan_to_ledger_ops, PlanStep,
};
use degenbot_executor::grammar_shape::{
    derive_shape, derive_shape_detailed, family_axis_support, Derive,
};
use degenbot_executor::grammar_walker::build_walk;

/// The `{V2,V3,V4}³` base of the permutation index (three protocols per hop).
const PROTOCOLS: usize = 3;

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");

/// The entry (flash/self-fund) amount every permutation runs at.
const OPTIMAL: u128 = 1_000_000;
/// A profitable three-hop amount chain (each hop returns more than it takes).
const PROFITABLE_OUTS: [u128; 3] = [1_100_000, 1_200_000, 1_300_000];
const PROFITABLE_CONSUMED: [u128; 3] = [1_000_000, 1_100_000, 1_200_000];
/// The losing counterpart: the terminal hop returns less than the entry.
const LOSING_OUTS: [u128; 3] = [900_000, 800_000, 700_000];
const LOSING_CONSUMED: [u128; 3] = [1_000_000, 900_000, 800_000];

fn v2(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
    HopInfo::V2(V2HopInfo {
        pool_address: Address::from([0xA0 + idx; 20]),
        token0_address: in_t,
        token1_address: out_t,
        fee: 30,
        zfo: true,
    })
}

fn v3(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
    HopInfo::V3(V3HopInfo {
        pool_address: Address::from([0xB0 + idx; 20]),
        token0_address: in_t,
        token1_address: out_t,
        fee: 3000,
        zfo: true,
    })
}

fn v4(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
    HopInfo::V4(V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: format!("0x{idx:02x}"),
        currency0_address: in_t,
        currency1_address: out_t,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true,
    })
}

/// All `{V2,V3,V4}³` hop-protocol permutations over a connected
/// `WETH → USDC → WBTC → WETH` cycle, named `v2_v3_v4` style.
fn permutations() -> Vec<(String, PathInfo)> {
    const FAMS: [&str; PROTOCOLS] = ["V2", "V3", "V4"];
    let cycle = [WETH, USDC, WBTC];
    let mut out = Vec::with_capacity(PROTOCOLS * PROTOCOLS * PROTOCOLS);
    for fidx in 0..PROTOCOLS * PROTOCOLS * PROTOCOLS {
        // `PLACES[i]` is `PROTOCOLS^i` (the mixed-radix place value of hop `i`).
        const PLACES: [usize; PROTOCOLS] = [1, PROTOCOLS, PROTOCOLS * PROTOCOLS];
        let combo: Vec<&str> = (0..PROTOCOLS)
            .map(|i| FAMS[(fidx / PLACES[i]) % PROTOCOLS])
            .collect();
        let hops: Vec<HopInfo> = combo
            .iter()
            .enumerate()
            .map(|(i, fam)| {
                let in_t = cycle[i % 3];
                let out_t = cycle[(i + 1) % 3];
                match *fam {
                    "V2" => v2(in_t, out_t, i as u8),
                    "V3" => v3(in_t, out_t, i as u8),
                    _ => v4(in_t, out_t, i as u8),
                }
            })
            .collect();
        out.push((combo.join("_").to_lowercase(), PathInfo::new(hops)));
    }
    out
}

fn inputs<'a>(
    outs: &'a [u128; 3],
    consumed: &'a [u128; 3],
    opts: EncodeOptions,
) -> ComposerInputs<'a> {
    ComposerInputs {
        executor_address: EXECUTOR,
        pool_manager_address: PM,
        weth_address: WETH,
        optimal_input: OPTIMAL,
        hop_outputs: outs,
        consumed_inputs: consumed,
        opts,
    }
}

fn zero_capital_opts() -> EncodeOptions {
    EncodeOptions {
        funding: FundingSource::InPathFlash,
        initial_capital: InitialCapital::Zero,
        ..Default::default()
    }
}

fn production_opts() -> EncodeOptions {
    EncodeOptions {
        funding: FundingSource::InPathFlash,
        initial_capital: InitialCapital::Held,
        ..Default::default()
    }
}

fn self_fund_opts() -> EncodeOptions {
    EncodeOptions {
        funding: FundingSource::SelfFund,
        initial_capital: InitialCapital::Held,
        ..Default::default()
    }
}

/// The losing permutations that fall back to a Plan-BUILD decline instead of a
/// typed deficit — **empty**, and the pin that keeps it that way.
///
/// It used to hold the four terminal-V4 captures whose builder modelled the
/// capture as `terminal_output − entry` (`checked_sub`), so a losing amount
/// chain had no Plan representation and the builder declined before the ledger
/// ran. That was a *Plan-vocabulary* gap, not a capital-policy one: a capture is
/// signed, but only its positive half is a take. `PlanStep::CaptureShortfall`
/// now carries the negative half, so the policy reaches all 27 and any
/// regression that reintroduces a build-time decline shows up here by name.
const BUILD_DECLINING_LOSING_PERMUTATIONS: [&str; 0] = [];

/// Every profitable zero-capital permutation encodes, and the bytes are the
/// production bytes (the stance never changes a valid stream).
#[test]
fn profitable_zero_capital_permutations_encode_production_bytes() {
    let mut encoded = 0;
    for (name, path) in permutations() {
        let zero = derive_shape_detailed(
            &path,
            &inputs(&PROFITABLE_OUTS, &PROFITABLE_CONSUMED, zero_capital_opts()),
        );
        let production = derive_shape_detailed(
            &path,
            &inputs(&PROFITABLE_OUTS, &PROFITABLE_CONSUMED, production_opts()),
        );
        let (Derive::Encoded(zero_bytes), Derive::Encoded(production_bytes)) = (zero, production)
        else {
            panic!("[{name}] a profitable zero-capital permutation must encode in both stances");
        };
        assert_eq!(
            zero_bytes, production_bytes,
            "[{name}] the zero-capital stance must not change a valid stream's bytes"
        );
        // The public intake seam agrees.
        let req = EncodeRequest::new(
            path.clone(),
            OPTIMAL,
            PROFITABLE_OUTS.to_vec(),
            PROFITABLE_CONSUMED.to_vec(),
            zero_capital_opts(),
        );
        let public = encode_cmd_stream(&EncodeContext::new(EXECUTOR, PM, WETH), &req)
            .unwrap_or_else(|| panic!("[{name}] profitable zero-capital intake declined"));
        assert_eq!(public, zero_bytes, "[{name}] intake bytes differ");
        encoded += 1;
    }
    assert_eq!(encoded, 27, "all 27 profitable permutations must encode");
}

/// Every losing zero-capital permutation the Plan can represent reports a typed
/// deficit — never a silent approval, never a panic.
#[test]
fn losing_zero_capital_permutations_report_typed_deficits() {
    let mut typed = 0;
    let mut declined = Vec::new();
    for (name, path) in permutations() {
        let detail = derive_shape_detailed(
            &path,
            &inputs(&LOSING_OUTS, &LOSING_CONSUMED, zero_capital_opts()),
        );
        match detail {
            Derive::InsolventZeroCapital(InsolventZeroCapital {
                asset,
                required,
                available,
            }) => {
                assert_eq!(
                    asset, WETH,
                    "[{name}] a losing WETH cycle's deficit is denominated in WETH"
                );
                assert!(
                    required > available,
                    "[{name}] a typed deficit must be a real shortfall: required {required} > available {available}"
                );
                assert!(
                    available <= LOSING_OUTS[2],
                    "[{name}] `available` counts path-generated credit only (never the withheld seed)"
                );
                assert!(
                    derive_shape(
                        &path,
                        &inputs(&LOSING_OUTS, &LOSING_CONSUMED, zero_capital_opts()),
                    )
                    .is_none(),
                    "[{name}] the public seam declines an insolvent zero-capital stream (never panics)"
                );
                typed += 1;
            }
            Derive::Declined => declined.push(name),
            other => panic!("[{name}] unexpected zero-capital outcome: {other:?}"),
        }
    }
    assert_eq!(
        declined, BUILD_DECLINING_LOSING_PERMUTATIONS,
        "the Plan-build declines for losing zero-capital permutations changed"
    );
    assert_eq!(
        typed + declined.len(),
        27,
        "every one of the 27 losing permutations must be accounted for"
    );
    assert_eq!(
        typed, 27,
        "all 27 losing permutations are typed deficits — none declines at Plan build"
    );
}

/// The capital a self-funded override installs is exactly what the Plan the
/// adapter builds declares — and the funding axis is a PER-FAMILY fact, so the
/// answer is a real set, not a path-level rule.
///
/// This is the seam a path-aware override reads: `encode_cmd_stream_capital` is
/// the requirement half of the very build `encode_cmd_stream` performs, so the
/// two can never fund one asset and declare another. The expectation is derived
/// independently here — from the family's declared axis support — so a family
/// that ignores the `funding` axis (and therefore needs no entry capital at all)
/// is pinned as loudly as one that seeds the WETH entry.
#[test]
fn self_funded_requirements_come_from_the_built_plan() {
    let mut seeding = 0;
    let mut self_funding = 0;
    for (name, path) in permutations() {
        let request = EncodeRequest::new(
            path.clone(),
            OPTIMAL,
            PROFITABLE_OUTS.to_vec(),
            PROFITABLE_CONSUMED.to_vec(),
            self_fund_opts(),
        );
        let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
        let installed = encode_cmd_stream_capital(&ctx, &request);

        // The same Plan, built directly: the intake is a read of this build,
        // not a second derivation of it.
        let (_, plan, _) = build_walk(
            &path,
            &inputs(&PROFITABLE_OUTS, &PROFITABLE_CONSUMED, self_fund_opts()),
        )
        .unwrap_or_else(|| panic!("[{name}] the family builds a Plan"));
        assert_eq!(
            installed,
            plan_external_capital(&plan),
            "[{name}] the override's requirement set IS the built Plan's"
        );

        // One requirement per currency, whatever the family seeds: the Plan
        // states amounts per asset, and a duplicated entry would make the
        // override install the same asset twice.
        let mut currencies: Vec<Address> = installed
            .iter()
            .map(|requirement| requirement.currency)
            .collect();
        currencies.sort_unstable();
        currencies.dedup();
        assert_eq!(
            currencies.len(),
            installed.len(),
            "[{name}] the requirement set names each asset once"
        );
        for requirement in &installed {
            assert!(
                requirement.amount > 0,
                "[{name}] a declared requirement is capital, never a zero grant"
            );
        }
        if installed.is_empty() {
            self_funding += 1;
        } else {
            assert!(
                installed
                    .iter()
                    .any(|requirement| requirement.currency == WETH),
                "[{name}] a seeding family declares the cycle's WETH entry"
            );
            seeding += 1;
        }
    }
    assert_eq!(
        seeding + self_funding,
        27,
        "every permutation is accounted for"
    );
    assert!(
        seeding > 0 && self_funding > 0,
        "the matrix exercises both a funding-branched family and a self-funding one \
         (seeding {seeding}, self-funding {self_funding})"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// The declared held seed: a self-funded stream's entry capital
// ═══════════════════════════════════════════════════════════════════════════

/// The V4-led permutations, in fixture order. `v4_v4_v3` is the one shape whose
/// self-funded Plan already declares its held inputs, so it is the control that
/// the derivation adds nothing behind an explicit seed.
const V4_LED: [&str; 9] = [
    "v4_v4_v4", "v4_v2_v2", "v4_v2_v4", "v4_v3_v3", "v4_v3_v4", "v4_v4_v2", "v4_v4_v3", "v4_v2_v3",
    "v4_v3_v2",
];

fn fixture(name: &str) -> PathInfo {
    permutations()
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("fixture has {name}"))
        .1
}

/// The seed declaration a self-funded Plan of `path` carries at `outs`/`consumed`.
fn declared_seed(path: &PathInfo, outs: &[u128; 3], consumed: &[u128; 3]) -> Vec<(Address, u128)> {
    let (_, plan, _) = build_walk(path, &inputs(outs, consumed, self_fund_opts()))
        .unwrap_or_else(|| panic!("the self-funded family builds a Plan"));
    // A declaration emits no command bytes, so a Plan carrying one must still
    // encode: the residual is a seed, never a refused capture.
    assert!(
        !plan
            .iter()
            .any(|step| matches!(step, PlanStep::CaptureShortfall { .. })),
        "a self-funded Plan may not carry a capture shortfall — it is unencodable"
    );
    plan_external_capital(&plan)
        .into_iter()
        .map(|requirement| (requirement.currency, requirement.amount))
        .collect()
}

/// Every losing self-funded permutation pays out entry capital the executor must
/// already hold, so its Plan has to DECLARE that capital: the ledger derives the
/// payment its own credit graph leaves uncovered and the seed names it, which is
/// what makes the stream payable on-chain instead of reverting inside a WETH
/// `transfer` with empty return data.
#[test]
fn losing_self_funded_permutations_declare_every_uncovered_payment() {
    let mut declaring = 0;
    for (name, path) in permutations() {
        let declared = declared_seed(&path, &LOSING_OUTS, &LOSING_CONSUMED);
        // Whatever the builder seeds on its own, the derivation must leave the
        // evaluation with nothing uncovered: declared seeds credit the search
        // (they are authoritative), a shortfall behind them is declared.
        let (_, plan, _) = build_walk(
            &path,
            &inputs(&LOSING_OUTS, &LOSING_CONSUMED, self_fund_opts()),
        )
        .unwrap();
        let mut validator = LedgerValidator::default();
        assert_eq!(
            validator.undeclared_seed(&plan_to_ledger_ops(&plan)),
            Ok(None),
            "[{name}] the losing self-funded Plan must declare the capital its payments need, \
             got {declared:?}"
        );
        for (currency, amount) in &declared {
            assert!(
                *amount > 0,
                "[{name}] a declaration is capital, never a zero grant"
            );
            // The cycle's own two assets: the WETH entry and, for a shape that
            // pays a V4 debt in its intermediate, that intermediate.
            assert!(
                [WETH, USDC, WBTC].contains(currency),
                "[{name}] a declared asset is one the cycle names, got {currency}"
            );
        }
        if !declared.is_empty() {
            assert!(
                declared.iter().any(|(currency, _)| *currency == WETH),
                "[{name}] a losing WETH cycle holds its entry in WETH, got {declared:?}"
            );
            declaring += 1;
        }
        // …and the stream still encodes (a Reject here would be the encoder's
        // latent-bug panic, i.e. the failure this declaration removes).
        assert!(
            matches!(
                derive_shape_detailed(
                    &path,
                    &inputs(&LOSING_OUTS, &LOSING_CONSUMED, self_fund_opts())
                ),
                Derive::Encoded(_)
            ),
            "[{name}] a losing self-funded stream must encode to its economic receipt"
        );
    }
    assert_eq!(
        declaring, 27,
        "every losing permutation declares its held capital"
    );
}

/// The V4-first shapes that declared nothing: their declaration is EXACTLY the
/// shortfall the zero-capital policy already reports (`required - available`),
/// not the whole entry and not a per-family guess.
///
/// `v4_v2_v2` is pinned by name — the mainnet probe's shape — with the ledger's
/// own numbers: the settle wants the whole entry (`required`) and the path's
/// realized credit is the terminal output (`available`), so the declaration is
/// the difference between them.
#[test]
fn v4_first_losing_self_fund_declares_exactly_the_zero_capital_shortfall() {
    for name in V4_LED.into_iter().filter(|n| *n != "v4_v4_v3") {
        let path = fixture(name);
        let Derive::InsolventZeroCapital(InsolventZeroCapital {
            asset,
            required,
            available,
        }) = derive_shape_detailed(
            &path,
            &inputs(&LOSING_OUTS, &LOSING_CONSUMED, zero_capital_opts()),
        )
        else {
            panic!("[{name}] the zero-capital stance must report a typed deficit");
        };
        assert_eq!(
            declared_seed(&path, &LOSING_OUTS, &LOSING_CONSUMED),
            vec![(asset, required - available)],
            "[{name}] the held declaration is the deficit's own shortfall"
        );
    }

    // The pinned row: WETH (1e15 on mainnet) wanted by the settle, the terminal
    // output available, the shortfall declared.
    let path = fixture("v4_v2_v2");
    assert_eq!(
        derive_shape_detailed(
            &path,
            &inputs(&LOSING_OUTS, &LOSING_CONSUMED, zero_capital_opts())
        ),
        Derive::InsolventZeroCapital(InsolventZeroCapital {
            asset: WETH,
            required: OPTIMAL,
            available: LOSING_OUTS[2],
        }),
        "v4_v2_v2's deficit: the settle wants the entry, the path credits its output"
    );
    assert_eq!(
        declared_seed(&path, &LOSING_OUTS, &LOSING_CONSUMED),
        vec![(WETH, OPTIMAL - LOSING_OUTS[2])],
        "v4_v2_v2 declares the entry the settle pays, less the credit it earns first"
    );

    // The control: a shape whose self-funded Plan already declares both held
    // inputs gains no restatement behind them.
    assert_eq!(
        declared_seed(&fixture("v4_v4_v3"), &LOSING_OUTS, &LOSING_CONSUMED),
        vec![(WETH, OPTIMAL), (USDC, LOSING_CONSUMED[1])],
        "v4_v4_v3's two explicit seeds stay authoritative and unduplicated"
    );
}

/// A solvent self-funded Plan gains no seed — for any of the 27 permutations and
/// both amount arms that fund their own payments.
#[test]
fn solvent_self_funded_plans_declare_no_unnecessary_seed() {
    for (name, path) in permutations() {
        let (_, plan, _) = build_walk(
            &path,
            &inputs(&PROFITABLE_OUTS, &PROFITABLE_CONSUMED, self_fund_opts()),
        )
        .unwrap_or_else(|| panic!("[{name}] the self-funded family builds a Plan"));
        let mut validator = LedgerValidator::default();
        assert_eq!(
            validator.undeclared_seed(&plan_to_ledger_ops(&plan)),
            Ok(None),
            "[{name}] a profitable self-funded Plan needs no held capital"
        );
        if V4_LED.contains(&name.as_str()) && name != "v4_v4_v3" {
            assert!(
                plan_external_capital(&plan).is_empty(),
                "[{name}] a profitable V4-first stream that pays from its own credit declares \
                 no seed, got {:?}",
                plan_external_capital(&plan)
            );
        }
    }
}

/// The shortened V4-first shape that used to abort a worker thread: a 2-hop
/// `V4→V2` self-funded stream whose entry pays the PoolManager.
///
/// The held seed turns the encoder's latent-bug panic into a typed, intentional
/// outcome — and removing it puts the panic back, which is what the sensitivity
/// arm proves: the Plan without its declaration is the `Erc20TransferBeforeCredit`
/// the live ladder logged (`wanted: 1e15, have: 998142225664309`), i.e. the empty
/// WETH-transfer revert seen on-chain.
#[test]
fn shortened_v4_first_self_fund_declares_its_entry_and_no_longer_panics() {
    // `WETH → USDC` (V4) then `USDC → WETH` (V2): the V4 unlock pays its entry
    // WETH into the PoolManager after the tail swap has credited its output, so
    // the plan needs the difference held.
    let path = PathInfo::new(vec![v4(WETH, USDC, 0), v2(USDC, WETH, 1)]);
    let outs = [LOSING_OUTS[0], LOSING_OUTS[2], 0];
    let consumed = [OPTIMAL, LOSING_OUTS[0], 0];
    let two_hop_inputs = |opts: EncodeOptions| ComposerInputs {
        hop_outputs: &outs[..2],
        consumed_inputs: &consumed[..2],
        ..inputs(&LOSING_OUTS, &LOSING_CONSUMED, opts)
    };

    // The self-funded stream: the declaration is exactly the entry the transfer
    // pays, less the credit the tail swap earned before it.
    let (_, plan, _) = build_walk(&path, &two_hop_inputs(self_fund_opts())).unwrap();
    assert_eq!(
        plan_external_capital(&plan),
        vec![CapitalRequirement {
            currency: WETH,
            amount: OPTIMAL - LOSING_OUTS[2],
        }],
        "the shortened stream declares its uncovered entry"
    );
    assert!(
        derive_shape(&path, &two_hop_inputs(self_fund_opts())).is_some(),
        "the public seam encodes it — no panic, no decline"
    );

    // Sensitivity: strip the declaration and the ledger refuses the very
    // transfer the declaration funds — the live ladder's panic text.
    let stripped: Vec<_> = plan
        .iter()
        .filter(|step| !matches!(step, PlanStep::SelfFund { .. }))
        .cloned()
        .collect();
    let mut validator = LedgerValidator::default();
    assert_eq!(
        validator.validate_capital(&plan_to_ledger_ops(&stripped)),
        Err(CapitalError::Rejected(
            ValidationError::Erc20TransferBeforeCredit {
                currency: WETH,
                wanted: OPTIMAL,
                have: LOSING_OUTS[2] as i128,
            }
        )),
        "without the declaration the entry transfer is uncovered again"
    );

    // The flash stream is a different question: it borrows its entry in-path
    // and must not silently spend capital it never declared, so production
    // keeps ADR-030's fatal Reject for it.
    let production = two_hop_inputs(production_opts());
    assert!(
        matches!(
            derive_shape_detailed(&path, &production),
            Derive::Rejected(ValidationError::Erc20TransferBeforeCredit { .. })
        ),
        "an in-path-flash stream that pays undeclared capital still rejects"
    );
}

/// The held-seed declaration is **ledger-only**: it is a `SelfFund` step, and
/// `SelfFund` emits no command byte. This pins that on every reachable family
/// (2- and 3-hop, 36 cells) and several amount sets, in both directions:
///
/// * a cell that gained a declaration encodes the SAME bytes with the seed
///   present and stripped (the declaration cannot reach the wire);
/// * a family whose row does not branch the `funding` axis still emits the same
///   bytes under `SelfFund` as under the production flash stream — the
///   pre-existing partition, unchanged by the declaration.
#[test]
fn the_held_seed_declaration_changes_no_command_byte() {
    fn strip(plan: &[PlanStep]) -> Vec<PlanStep> {
        plan.iter()
            .filter(|step| !matches!(step, PlanStep::SelfFund { .. }))
            .map(|step| {
                let mut step = step.clone();
                match &mut step {
                    PlanStep::FlashSwap { callback, .. } => *callback = strip(callback),
                    PlanStep::V4Unlock { inner, .. } => *inner = strip(inner),
                    _ => {}
                }
                step
            })
            .collect()
    }

    // (optimal_input, hop_outputs, consumed_inputs) — a profitable, an exact, a
    // losing and a partially-consumed arm.
    let arms: [(u128, [u128; 3], [u128; 3]); 4] = [
        (OPTIMAL, PROFITABLE_OUTS, PROFITABLE_CONSUMED),
        (
            OPTIMAL,
            [OPTIMAL, OPTIMAL, OPTIMAL],
            [OPTIMAL - 1, OPTIMAL - 1, OPTIMAL - 1],
        ),
        (OPTIMAL, LOSING_OUTS, LOSING_CONSUMED),
        (
            OPTIMAL,
            [900_001, 800_003, 700_007],
            [OPTIMAL, 900_001, 800_003],
        ),
    ];

    let mut declared_cells = 0;
    let mut unchanged_cells = 0;
    for (name, path) in permutations() {
        let branches_funding = family_axis_support(&path)
            .unwrap_or_else(|| panic!("[{name}] the row exists"))
            .funding;
        for (optimal, outs, consumed) in &arms {
            let arm_inputs = |opts| ComposerInputs {
                optimal_input: *optimal,
                ..inputs(outs, consumed, opts)
            };
            let (preamble, plan, at) = build_walk(&path, &arm_inputs(self_fund_opts()))
                .unwrap_or_else(|| panic!("[{name}] the self-funded family builds a Plan"));
            let declared = plan_external_capital(&plan);
            let encoded = {
                let mut bytes = preamble.clone();
                bytes.extend_from_slice(&plan_to_bytes(&plan, &at).expect("the Plan encodes"));
                bytes
            };
            let mut stripped_bytes = preamble;
            stripped_bytes.extend_from_slice(
                &plan_to_bytes(&strip(&plan), &at).expect("the stripped Plan encodes"),
            );
            assert_eq!(
                encoded, stripped_bytes,
                "[{name}] a SelfFund declaration reached the command wire"
            );
            if !declared.is_empty() {
                declared_cells += 1;
            }

            if !branches_funding {
                let flash = derive_shape_detailed(&path, &arm_inputs(production_opts()));
                let self_fund = derive_shape_detailed(&path, &arm_inputs(self_fund_opts()));
                assert_eq!(
                    self_fund, flash,
                    "[{name}] at {optimal}: a family that funds itself in-path must emit the \
                     production bytes under a self-funded stream"
                );
                unchanged_cells += 1;
            }
        }
    }
    assert!(
        declared_cells > 0,
        "the sweep must exercise cells that gained a declaration"
    );
    assert!(
        unchanged_cells > 0,
        "the sweep must exercise cells whose bytes are the production stream's"
    );
}

/// The four terminal-V4 captures the builder used to decline: their deficit is
/// the unearned residual itself (`entry − terminal_output`), answered by the
/// Plan's declared shortfall, and the production stance declines the family
/// rather than panicking or emitting a stream.
#[test]
fn declared_capture_shortfall_names_the_unearned_residual() {
    for name in ["v2_v2_v4", "v2_v3_v4", "v2_v4_v4", "v3_v4_v4"] {
        let (_, path) = permutations()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("fixture has {name}"));
        assert_eq!(
            derive_shape_detailed(
                &path,
                &inputs(&LOSING_OUTS, &LOSING_CONSUMED, zero_capital_opts())
            ),
            Derive::InsolventZeroCapital(InsolventZeroCapital {
                asset: WETH,
                required: OPTIMAL - LOSING_OUTS[2],
                available: 0,
            }),
            "[{name}] the deficit is the residual the path never earned"
        );
        // Production: a routine DECLINE, never a fatal `Rejected` (ADR-030).
        // The residual would have to be spent out of the executor's balance,
        // and no `SelfFund` seed in this Plan declares that capital — that is
        // the self-funded shape's job, and it still encodes.
        assert_eq!(
            derive_shape_detailed(
                &path,
                &inputs(&LOSING_OUTS, &LOSING_CONSUMED, production_opts())
            ),
            Derive::Declined,
            "[{name}] the held stance has no stream for a declared capture"
        );
        assert!(
            derive_shape(
                &path,
                &inputs(&LOSING_OUTS, &LOSING_CONSUMED, production_opts())
            )
            .is_none(),
            "[{name}] the public seam declines it (never panics)"
        );
    }
}

/// Production (`InitialCapital::Held`) rejection classification is unchanged: a
/// validator `Reject` is still fatal, and a self-funded losing stream still
/// encodes so it executes to its economic receipt.
#[test]
fn held_capital_keeps_production_rejection_classification() {
    // Two losing InPathFlash streams the production model rejects today: an
    // all-V2 repayment shortfall and a PM take shortfall inside a V4 unlock.
    for name in ["v2_v2_v2", "v3_v3_v4"] {
        let (_, path) = permutations()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("fixture has {name}"));
        let inputs = inputs(&LOSING_OUTS, &LOSING_CONSUMED, production_opts());
        match derive_shape_detailed(&path, &inputs) {
            Derive::Rejected(_) => {}
            other => {
                panic!("[{name}] production must still REJECT a losing in-path flash: {other:?}")
            }
        }
        // …and the public seam is fatal (ADR-030), never a `None`.
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            derive_shape(&path, &inputs)
        }))
        .is_err();
        assert!(
            panicked,
            "[{name}] a production validator Reject must stay fatal at the public seam"
        );
    }

    // Every losing permutation under SelfFund still encodes: the executor funds
    // the entry, so the loss is measured on-chain rather than refused.
    for (name, path) in permutations() {
        match derive_shape_detailed(
            &path,
            &inputs(&LOSING_OUTS, &LOSING_CONSUMED, self_fund_opts()),
        ) {
            Derive::Encoded(_) => {}
            other => panic!("[{name}] a losing self-funded path must still encode: {other:?}"),
        }
    }
}

/// The stance is the only difference: the same Plan under `held` is solvent
/// exactly where it is, so a zero-capital deficit is the policy's answer, not a
/// second validator.
#[test]
fn zero_capital_deficit_is_absent_under_held_capital() {
    let (_, path) = permutations()
        .into_iter()
        .find(|(n, _)| n == "v4_v3_v4")
        .unwrap_or_else(|| panic!("fixture has v4_v3_v4"));
    // v4_v3_v4's losing plan settles a negative PM delta from the executor's
    // balance: `Held` grants that capital (it encodes), `Zero` reports the
    // deficit that capital would cover.
    let held = derive_shape_detailed(
        &path,
        &inputs(&LOSING_OUTS, &LOSING_CONSUMED, production_opts()),
    );
    assert!(
        matches!(held, Derive::Encoded(_)),
        "the held stance funds the settle: {held:?}"
    );
    let zero = derive_shape_detailed(
        &path,
        &inputs(&LOSING_OUTS, &LOSING_CONSUMED, zero_capital_opts()),
    );
    assert_eq!(
        zero,
        Derive::InsolventZeroCapital(InsolventZeroCapital {
            asset: WETH,
            required: 300_000,
            available: 0,
        }),
        "the zero stance reports the same 300_000 WETH shortfall"
    );
}
