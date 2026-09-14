//! V2-style constant-product (`x·y=k`) swap math — pure-Rust, EVM-exact.
//!
//! This crate owns the constant-product AMM swap primitive that the V2 pool
//! family (Uniswap V2, Sushiswap, `PancakeSwap` V2, Swapbased, Camelot-v2
//! volatile, Aerodrome-v2 volatile) and the Solidly/Camelot/Aerodrome
//! volatile "V2-equivalent" hop representation all share: the EVM-exact
//! single-hop `getAmountOut`
//!
//! ```text
//! y = gamma_numer * reserve_out * x / (fee_denom * reserve_in + gamma_numer * x)
//! ```
//!
//! with floor division (EVM `DIV` semantics), computed in `U512` and narrowed
//! to `U256`.
//!
//! It is the V2-family sibling of the other pure-math leaf crates:
//! `degenbot-concentrated-liquidity-math` (concentrated liquidity), `degenbot-curve-math`
//! (stableswap), `degenbot-balancer-math` (weighted/stable),
//! `degenbot-solidly-math` (Solidly stable), and `degenbot-core::eip_1559`
//! (EIP-1559 base fee).
//!
//! ## Why this is a standalone crate (ADR-005 "standalone constraint")
//!
//! A single-hop swap amount calc is a pool-specific, value-only concern — it
//! is *not* an arbitrage/path-optimization (solver) concern. Previously the
//! primitive lived in `degenbot-bot/src/solvers/mobius_int.rs`, which is
//! documented as the "Arbitrage solvers using Möbius transformation
//! composition" module. That placement inverted the dependency direction:
//! pool-state code in `bot_core` reached *up* into `solvers` for a primitive
//! the solvers themselves sit on top of, and it stranded standalone-usable
//! V2 swap math inside the `degenbot-bot` engine crate — so a `cargo add
//! degenbot` consumer wanting just V2 swap math pulled the engine/pump/
//! registry surface.
//!
//! This crate is `pyo3`-free under its default features (enforced by
//! `just check-no-pyo3-in-cores`); it depends only on `alloy`. It is consumed
//! by `degenbot-bot` (the engine + the Möbius solvers, which compose
//! `IntHopState` hops into multi-hop arbitrage paths) and re-exported by the
//! `degenbot` umbrella for standalone Rust consumers.
//!
//! ## Contents
//!
//! The primitive surface lives in the [`hop_state`] submodule: [`IntHopState`]
//! (+ `new` / `swap`), [`SimulationResult`], and [`int_simulate_path`].

use alloy::primitives::U256;

pub mod hop_state;
pub use hop_state::{int_simulate_path, HopSwapError, IntHopState, SimulationResult, StepOutcome};

/// V2 (pure constant-product) exact-in — the on-chain `getAmountOut`:
/// `y = (fee_denom - fee_numer) * reserve_out * amount_in / (fee_denom * reserve_in + (fee_denom - fee_numer) * amount_in)`,
/// floor-divided. `fee_numer` / `fee_denom` are the raw fee rate (e.g. 3 / 1000).
///
/// # Errors
///
/// [`HopSwapError::InvalidFee`] when `fee_numer >= fee_denom`.
pub fn v2_swap_exact_in(
    reserve_in: U256,
    reserve_out: U256,
    amount_in: U256,
    fee_numer: u64,
    fee_denom: u64,
) -> Result<U256, HopSwapError> {
    if fee_numer > fee_denom {
        return Err(HopSwapError::InvalidFee);
    }
    let gamma = fee_denom - fee_numer;
    IntHopState::new(reserve_in, reserve_out, gamma, fee_denom).swap(amount_in)
}

/// V2 (pure constant-product) exact-out — the `getAmountOut` inverse with
/// `+1` floor-division compensation (router-side math, `U512` intermediates):
/// `x = 1 + reserve_in * amount_out * fee_denom / ((reserve_out - amount_out) * (fee_denom - fee_numer))`.
///
/// # Errors
///
/// [`HopSwapError::InvalidFee`] when `fee_numer >= fee_denom`.
pub fn v2_swap_exact_out(
    reserve_in: U256,
    reserve_out: U256,
    amount_out: U256,
    fee_numer: u64,
    fee_denom: u64,
) -> Result<U256, HopSwapError> {
    if fee_numer > fee_denom {
        return Err(HopSwapError::InvalidFee);
    }
    let gamma = fee_denom - fee_numer;
    IntHopState::new(reserve_in, reserve_out, gamma, fee_denom).swap_exact_out(amount_out)
}

#[cfg(test)]
// The 50 transcribed oracle vectors below are ground-truth literal data, not
// hand-written code: digit-group underscores buy nothing and would only invite
// transcription drift, so `unreadable_literal` is expected off for this module.
#[expect(clippy::unwrap_used, clippy::unreadable_literal)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    fn u(n: u128) -> U256 {
        U256::from(n)
    }

    /// Ground-truth exact-in vectors, transcribed from the retired Python
    /// reference `constant_product_calc_exact_in` (deleted in this change).
    ///
    /// Provenance: random.Random(seed) for seed in 0..25, reserves in
    /// [10^6, 2*10^16), `amount_in` in [1, 10^14), fee rate drawn from
    /// [(3,1000),(5,10000),(25,10000),(30,10000),(0,1000)] - the exact seed
    /// space the former Python-vs-Rust parity test exercised. The outputs were
    /// emitted by the Python oracle before its deletion and are now literal
    /// truth pinning, so the Rust primitive no longer needs a Python shadow to
    /// stay honest.
    #[rustfmt::skip]
    const EXACT_IN_VECTORS: &[(u128, u128, u128, u64, u64, u128)] = &[
        (13879924884195853u128, 1458602765833877u128, 68386665195107u128, 30, 10000, 7129980893919u128),
        (2273664478608900u128, 4248466781599356u128, 66462254487716u128, 30, 10000, 120309544149375u128),
        (3299686178449631u128, 13008464167661085u128, 23797707260197u128, 25, 10000, 92915516268865u128),
        (4698975733144970u128, 17079579997487933u128, 81748799992999u128, 3, 1000, 291193911776450u128),
        (10927480147601431u128, 17253413611066601u128, 12679409058651u128, 3, 1000, 19936414709729u128),
        (9203378857295493u128, 12917602660975321u128, 97172755435064u128, 0, 1000, 134963935092206u128),
        (2902825897744086u128, 1326677867684473u128, 20486995942025u128, 0, 1000, 9297548733264u128),
        (14225013937622903u128, 1739700774823568u128, 13247980736556u128, 25, 10000, 1614660353605u128),
        (13345490860550003u128, 13524113119787495u128, 27179095636178u128, 3, 1000, 27404600583357u128),
        (9624529393459504u128, 6706579143824362u128, 95231736741718u128, 3, 1000, 65514146949884u128),
        (1174001700746003u128, 17386115356618384u128, 2089836989113u128, 5, 10000, 30878559190448u128),
        (16278976024812033u128, 6841941260901490u128, 66956443808195u128, 0, 1000, 28026058253573u128),
        (9691565677918494u128, 19063945312559212u128, 49227482390650u128, 5, 10000, 96296384897137u128),
        (10475649686676411u128, 6750867109198541u128, 9964883387421u128, 0, 1000, 6415609028697u128),
        (18991210018077697u128, 8896783980310906u128, 40957906682118u128, 3, 1000, 19088882257168u128),
        (7529313744805436u128, 18782420317441935u128, 5092697305702u128, 5, 10000, 12689173035745u128),
        (16905525406729195u128, 10265807125744180u128, 31896217938011u128, 30, 10000, 19274477055082u128),
        (14921972759658241u128, 10931983724219296u128, 51457833499736u128, 25, 10000, 37475358456948u128),
        (4424890836230663u128, 16170644905575778u128, 33704045294694u128, 5, 10000, 122178783608397u128),
        (1558384547946647u128, 4348261486593903u128, 28078398054081u128, 30, 10000, 76731955228605u128),
        (5447744699510868u128, 3657520727574454u128, 46084449890227u128, 0, 1000, 30680755827790u128),
        (15059805316670279u128, 15061121830736632u128, 39585145903928u128, 30, 10000, 39366674536372u128),
        (8741199649076350u128, 16109683588039801u128, 98789334138669u128, 3, 1000, 179496119284956u128),
        (10445351654200911u128, 615245835064856u128, 43171259029672u128, 30, 10000, 2524815144896u128),
        (13794012086099560u128, 7863939875314132u128, 23553593275229u128, 5, 10000, 13398278063440u128),
    ];

    /// Ground-truth exact-out vectors - same provenance as `EXACT_IN_VECTORS`,
    /// seeded with random.Random(1000 + seed) and `amount_out` in
    /// [1, max(2, `reserve_out` / 2)).
    #[rustfmt::skip]
    const EXACT_OUT_VECTORS: &[(u128, u128, u128, u64, u64, u128)] = &[
        (15455541938625164u128, 14181252293753048u128, 566911429265034u128, 30, 10000, 645516982619471u128),
        (7135000506815803u128, 13593940441235676u128, 1443163926461421u128, 30, 10000, 849983473223776u128),
        (5180942689705763u128, 9996030136092578u128, 1143953732970504u128, 0, 1000, 669533164908611u128),
        (7979140250048506u128, 16686599587253339u128, 4857189023634233u128, 30, 10000, 3286115623778003u128),
        (3831211421997508u128, 19444526469819825u128, 6152674695852505u128, 3, 1000, 1778768478124741u128),
        (14380516455528787u128, 14791727942864302u128, 5744117493164501u128, 25, 10000, 9152737355202582u128),
        (2012004689336357u128, 15987101914331993u128, 1847965566904128u128, 0, 1000, 262966230396358u128),
        (1160916157471297u128, 6270319796248683u128, 1401936969665344u128, 5, 10000, 334473584056641u128),
        (16036454222848825u128, 2915068780797866u128, 230712437057031u128, 3, 1000, 1382432802167539u128),
        (428823194655085u128, 17238411833651469u128, 7775161770950363u128, 5, 10000, 352504436244504u128),
        (19251036619893703u128, 14797532787525214u128, 3712597365549517u128, 5, 10000, 6450836082278651u128),
        (9137359815559009u128, 14923061571596070u128, 5944012161626446u128, 30, 10000, 6067012597793837u128),
        (5708992243670948u128, 14535153500156478u128, 5454506281054995u128, 3, 1000, 3439560375053523u128),
        (17721468450236433u128, 9190846755530765u128, 1653702844215566u128, 0, 1000, 3888215897780123u128),
        (205421911064038u128, 9152129659820106u128, 3803185100525055u128, 30, 10000, 146497756318723u128),
        (18861269366964427u128, 10065579706041777u128, 4306487714859920u128, 0, 1000, 14103929046430672u128),
        (19659338302080110u128, 6158637617463144u128, 2969826618904821u128, 3, 1000, 18364370842052518u128),
        (15382629963251576u128, 12724386480363245u128, 917075643737591u128, 25, 10000, 1197765642807700u128),
        (14217629257572718u128, 3614900925790807u128, 496767413725168u128, 0, 1000, 2265090602521642u128),
        (11964516314426033u128, 3647312918469242u128, 541018894483433u128, 30, 10000, 2090113447103616u128),
        (887697210680345u128, 10654066582288299u128, 1615147278217399u128, 0, 1000, 158620924192321u128),
        (18298018957012623u128, 19265175674061941u128, 6024586742482455u128, 3, 1000, 8350815218880996u128),
        (14848449836331666u128, 3111937286126262u128, 880716611218717u128, 0, 1000, 5861041253683714u128),
        (17221081336788251u128, 4531534077088634u128, 1403876297758315u128, 3, 1000, 7753090864225963u128),
        (689375740089520u128, 11645079647210707u128, 4681780084128098u128, 3, 1000, 464897026602392u128),
    ];

    #[test]
    fn v2_swap_exact_in_matches_python_reference_vectors() {
        for &(reserve_in, reserve_out, amount_in, fee_numer, fee_denom, expected) in
            EXACT_IN_VECTORS
        {
            assert_eq!(
                v2_swap_exact_in(u(reserve_in), u(reserve_out), u(amount_in), fee_numer, fee_denom)
                    .unwrap(),
                u(expected),
                "exact_in(r_in={reserve_in}, r_out={reserve_out}, amount_in={amount_in}, fee={fee_numer}/{fee_denom})",
            );
        }
    }

    #[test]
    fn v2_swap_exact_out_matches_python_reference_vectors() {
        for &(reserve_in, reserve_out, amount_out, fee_numer, fee_denom, expected) in
            EXACT_OUT_VECTORS
        {
            assert_eq!(
                v2_swap_exact_out(u(reserve_in), u(reserve_out), u(amount_out), fee_numer, fee_denom)
                    .unwrap(),
                u(expected),
                "exact_out(r_in={reserve_in}, r_out={reserve_out}, amount_out={amount_out}, fee={fee_numer}/{fee_denom})",
            );
        }
    }

    /// Hand-computed canonical vector: 100 * 997 * 2000 / (1000 * 1000 + 100 * 997)
    /// = `199_400_000` / `1_099_700` = 181 (floor).
    #[test]
    fn v2_swap_exact_in_hand_checked_canonical() {
        assert_eq!(
            v2_swap_exact_in(u(1000), u(2000), u(100), 3, 1000).unwrap(),
            u(181),
        );
    }

    /// Hand-computed canonical vector: 1 + 1000 * 50 * 1000 / ((2000 - 50) * 997)
    /// = 1 + `50_000_000` / `1_944_150` = 26.
    #[test]
    fn v2_swap_exact_out_hand_checked_canonical() {
        assert_eq!(
            v2_swap_exact_out(u(1000), u(2000), u(50), 3, 1000).unwrap(),
            u(26),
        );
    }

    /// `amount_out` >= `reserve_out` is undefined - the pool cannot hand out
    /// more than it holds.
    #[test]
    fn v2_swap_exact_out_rejects_overdraw() {
        assert_eq!(
            v2_swap_exact_out(
                u(10u128.pow(9)),
                u(10u128.pow(9)),
                u(10u128.pow(9)),
                3,
                1000
            ),
            Err(HopSwapError::InsufficientReserves),
        );
        assert_eq!(
            v2_swap_exact_out(
                u(10u128.pow(9)),
                u(10u128.pow(9)),
                u(10u128.pow(9)) + U256::ONE,
                3,
                1000
            ),
            Err(HopSwapError::InsufficientReserves),
        );
    }

    /// `fee_numer` > `fee_denom` (>100% fee) is invalid.
    #[test]
    fn v2_swap_invalid_fee_raises() {
        assert_eq!(
            v2_swap_exact_in(u(10u128.pow(9)), u(10u128.pow(9)), u(100), 5, 3),
            Err(HopSwapError::InvalidFee),
        );
        assert_eq!(
            v2_swap_exact_out(u(10u128.pow(9)), u(10u128.pow(9)), u(100), 5, 3),
            Err(HopSwapError::InvalidFee),
        );
    }

    /// A 100% fee (gamma = 0) exact-in yields 0 output - not an error.
    #[test]
    fn v2_swap_exact_in_full_fee_degenerate_is_zero() {
        assert_eq!(
            v2_swap_exact_in(u(10u128.pow(9)), u(10u128.pow(9)), u(100), 1000, 1000).unwrap(),
            U256::ZERO,
        );
    }

    /// exact-out(exact-in(x)) <= x and re-applying exact-in recovers >= the
    /// output (the exact-out +1 floor-division compensation).
    #[test]
    fn v2_swap_exact_out_is_inverse_of_exact_in() {
        let (r_in, r_out, amount_in) = (u(10u128.pow(9)), u(2 * 10u128.pow(9)), u(7_123_456));
        let out = v2_swap_exact_in(r_in, r_out, amount_in, 3, 1000).unwrap();
        assert!(!out.is_zero());
        let back_in = v2_swap_exact_out(r_in, r_out, out, 3, 1000).unwrap();
        assert!(back_in <= amount_in);
        assert!(v2_swap_exact_in(r_in, r_out, back_in, 3, 1000).unwrap() >= out);
    }
}
