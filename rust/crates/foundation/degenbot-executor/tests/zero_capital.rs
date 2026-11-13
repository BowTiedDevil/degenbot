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

#![expect(clippy::cast_possible_truncation, clippy::panic, clippy::similar_names)]

use alloy::primitives::{address, Address};
use degenbot_execution::{HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
use degenbot_executor::composers::{
    encode_cmd_stream, ComposerInputs, EncodeContext, EncodeOptions, EncodeRequest,
};
use degenbot_executor::grammar_ledger::{FundingSource, InitialCapital, InsolventZeroCapital};
use degenbot_executor::grammar_shape::{derive_shape, derive_shape_detailed, Derive};

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
