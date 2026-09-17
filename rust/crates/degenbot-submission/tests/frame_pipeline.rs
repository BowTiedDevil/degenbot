//! Frame-pipeline tests (task WKPZQK).
//!
//! 1. The classifier guard: with `DEGENBOT_CLASSIFIER_GUARD=1` armed, a
//!    classifier entry PANICS — the dry-run e2e runs with the guard armed so
//!    any hot-path classification aborts the run.
//! 2. The golden frame (offline): a hand-built replay journal (post-target
//!    V2 reserves at slot 8) flows descriptors → `extract_pool_post_states`
//!    → workspace admission → 2-hop discovery/solve → compose, matching the
//!    hand-derived golden reference.
//! 3. The live e2e (`#[ignore]`-gated): a captured frame JSONL replays
//!    end-to-end against a forked node with the guard armed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use alloy::primitives::{address, aliases::U112, Address, U256};
use degenbot_bot::sidecar_engine::{LaneFamily, SidecarHopRef, SidecarSolver, SidecarV2Pool};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::slot_layout;
use degenbot_simulation::sim::evm::frame_replay::{BaseFeeSource, ReplayOutcome, ReplayStatus};
use degenbot_submission::frame_pipeline::{
    admit_extracted, build_descriptors, solve_fans, state_digest, DiscoveredConnector,
    StrategyRuntime, WETH,
};
use revm::state::{Account, AccountStatus, EvmState, EvmStorageSlot};

// ─────────────────────────── the classifier guard ──────────────────────────

#[test]
fn classifier_guard_armed_aborts_classify() {
    // Arm the panic-guard shim (WKPZQK; deleted by the cutover task). The
    // pipeline never calls classify, so the arm is only observed here.
    std::env::set_var("DEGENBOT_CLASSIFIER_GUARD", "1");
    let result = std::panic::catch_unwind(|| {
        degenbot_decoders::target_classifier::classify(
            address!("0x7a250d5630b4cf539739df2c5dacb4c659f2488d"),
            &[0x38, 0xed, 0x17, 0x39],
            &degenbot_decoders::target_classifier::RouterRegistry::mainnet(),
        )
    });
    std::env::remove_var("DEGENBOT_CLASSIFIER_GUARD");
    let err = result.expect_err("the armed guard MUST abort the classifier entry");
    let msg = err
        .downcast_ref::<&'static str>()
        .copied()
        .unwrap_or("")
        .to_string();
    assert_eq!(
        msg,
        degenbot_decoders::target_classifier::CLASSIFIER_GUARD_MSG,
        "the panic carries the guard message"
    );
}

// ─────────────────── the golden frame (offline, end to end) ────────────────

const TOK: Address = address!("0000000000000000000000000000000000000aa1");
/// P: the affected pool (the target swapped 50 WETH in, TOK -> WETH cycle).
const P: Address = address!("000000000000000000000000000000000000b001");
/// Q: the discovery connector the fan admits (cheaper TOK than staged P).
const Q: Address = address!("000000000000000000000000000000000000b002");
const SEED: u64 = 7;

/// The replayed journal of ONE V2 swap frame: post-target reserves packed at
/// the pair's slot 8 (`476_259` TOK / `1_050` WETH — the golden target's 50-WETH
/// swap settled), touched + status flag set.
fn golden_replay_outcome() -> ReplayOutcome {
    let reserves = slot_layout::pack_v2_reserves_word(U112::from(476_259u64), U112::from(1_050u64));
    let slot8 = U256::from_be_bytes(reserves.0);
    let mut state = EvmState::default();
    let mut account = Account::default();
    account.status = AccountStatus::Touched;
    account.storage.insert(
        U256::from(slot_layout::V2_RESERVES_SLOT),
        EvmStorageSlot {
            original_value: U256::ZERO,
            present_value: slot8,
            transaction_id: revm::state::TransactionId::ZERO,
            is_cold: false,
        },
    );
    state.insert(P, account);
    ReplayOutcome {
        status: ReplayStatus::Success,
        state,
        touched: vec![(P, vec![U256::from(slot_layout::V2_RESERVES_SLOT)])],
        rpc_reads: 0,
        wall: std::time::Duration::from_micros(42),
        base_fee_source: BaseFeeSource::Projected,
    }
}

/// The in-memory runtime fixture: seeded DB (TOK/WETH ids) + index edges for
/// P and Q in canonical (token0 = TOK) order.
fn runtime_fixture() -> (StrategyRuntime, u64, u64) {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let mut index = degenbot_bot::sidecar_paths::V2ConnectorIndex::default();
    index.push_edge(degenbot_bot::sidecar_paths::V2Edge {
        pool_id: 101,
        token0_id: u64::try_from(tok_id).unwrap(),
        token1_id: u64::try_from(weth_id).unwrap(),
        address: P,
    });
    index.push_edge(degenbot_bot::sidecar_paths::V2Edge {
        pool_id: 102,
        token0_id: u64::try_from(tok_id).unwrap(),
        token1_id: u64::try_from(weth_id).unwrap(),
        address: Q,
    });
    // The pipeline's sidecars quote chains of 1 (mainnet).
    (
        StrategyRuntime::new(1, Some(index), Some(db), 8),
        u64::try_from(tok_id).unwrap(),
        u64::try_from(weth_id).unwrap(),
    )
}

#[test]
fn golden_frame_extract_admit_solve_compose_end_to_end() {
    let (rt, _tok_id, _weth_id) = runtime_fixture();
    let outcome = golden_replay_outcome();

    // descriptors: projected from the connector index (the tracked registry).
    let (descriptors, hit_v4) = build_descriptors(rt.index.as_ref(), &outcome.touched);
    assert!(!hit_v4, "an all-V2 frame touches no PoolManager");
    assert_eq!(
        descriptors.len(),
        1,
        "only the tracked pool gets a descriptor"
    );

    // extract: the journalled slot-8 word decodes to the post-target pair.
    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors,
    );
    assert_eq!(extracted.len(), 1, "one touched tracked pool extracted");
    let degenbot_simulation::sim::evm::journal_pools::PoolPostState {
        address,
        family,
        kind,
    } = &extracted[0];
    assert_eq!(*address, P);
    assert!(matches!(
        family,
        degenbot_simulation::sim::evm::journal_pools::PoolFamily::V2Pair
    ));
    let reserves = match kind {
        degenbot_simulation::sim::evm::journal_pools::PoolPostKind::Typed(
            degenbot_simulation::sim::evm::journal_pools::TypedPoolPost::V2 { reserves },
        ) => reserves,
        other => panic!("expected typed V2 post-state, got {other:?}"),
    };
    assert_eq!(
        reserves.reserve0,
        U112::from(476_259u64),
        "TOK side drained"
    );
    assert_eq!(reserves.reserve1, U112::from(1_050u64), "WETH side grew");

    // admit into THIS frame's fresh workspace scope.
    let mut solver = SidecarSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture");
    assert_eq!(affected.len(), 1, "P admits and trades WETH");
    assert_eq!(affected[0].address, P);
    assert_eq!(affected[0].family, LaneFamily::V2);

    // Admit the connector the fan discovered (the discovery lane's admission
    // shape; the discovery fan call itself is exercised in the live e2e).
    let q_id = solver
        .admit_v2(&SidecarV2Pool {
            address: Q,
            token0: affected[0].token0,
            token1: affected[0].token1,
            reserve0: 200_000,
            reserve1: 900,
        })
        .expect("connector admits");
    let connectors = vec![DiscoveredConnector {
        address: Q,
        workspace_pool_id: q_id,
        family: LaneFamily::V2,
    }];

    // solve: both drift cycles declared + evaluated; the golden profit.
    let quote_id = rt.token_id(WETH).unwrap();
    let stats = solve_fans(
        &mut solver,
        &affected[0],
        &[degenbot_submission::frame_pipeline::QuoteFan {
            quote: WETH,
            quote_id,
            connectors,
            normalization: None,
        }],
        U256::ZERO,
    );
    assert_eq!(stats.connectors, 1);
    assert_eq!(stats.cycles_declared, 2, "both drift directions proposed");
    assert!(
        stats.cycles_evaluated >= 1,
        "at least the profitable direction solves (fee-drain direction may not)"
    );
    let best = stats.best.expect("the golden cycle profits");
    assert!(
        U256::from(best.profit) >= U256::from(55u8) && U256::from(best.profit) <= U256::from(56u8),
        "profit {} outside the golden plateau [55,56]",
        best.profit
    );
    assert!(
        U256::from(best.optimal_input) > U256::from(100u8)
            && U256::from(best.optimal_input) < U256::from(150u8),
        "optimal input {} far from the golden 123",
        best.optimal_input
    );

    // compose: the executable artifact builds from the solved hops.
    let cd = degenbot_bot::sidecar_engine::build_candidate_calldata(
        &best,
        address!("0x30b28ed8aa581fbc0191c3b532b0697773070e97"),
        WETH,
        9_800,
    )
    .expect("the 2-hop V2 candidate composes");
    assert!(cd.len() > 4 + 32 * 3 + 64, "execute() calldata shape");

    // digest evidence for the JSONL extract trace.
    let digest = state_digest(&extracted[0]);
    assert!(digest.starts_with("0x"), "digest {digest}");
}

// ─────────────── per-frame quote selection (the USDC frame) ────────────────

/// Real mainnet USDC — the non-WETH settlement quote this section exercises.
const USDC: Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
/// P2: the USDC frame's affected pool (canonical token0 = TOK: 0x…0aa1).
const P2: Address = address!("000000000000000000000000000000000000b003");

/// The replayed journal of ONE USDC-frame V2 swap: post-target reserves
/// packed at the pair's slot 8 (`900_000` TOK / `1_100` USDC settled).
fn usdc_frame_replay_outcome() -> ReplayOutcome {
    let reserves = slot_layout::pack_v2_reserves_word(U112::from(900_000u64), U112::from(1_100u64));
    let slot8 = U256::from_be_bytes(reserves.0);
    let mut state = EvmState::default();
    let mut account = Account::default();
    account.status = AccountStatus::Touched;
    account.storage.insert(
        U256::from(slot_layout::V2_RESERVES_SLOT),
        EvmStorageSlot {
            original_value: U256::ZERO,
            present_value: slot8,
            transaction_id: revm::state::TransactionId::ZERO,
            is_cold: false,
        },
    );
    state.insert(P2, account);
    ReplayOutcome {
        status: ReplayStatus::Success,
        state,
        touched: vec![(P2, vec![U256::from(slot_layout::V2_RESERVES_SLOT)])],
        rpc_reads: 0,
        wall: std::time::Duration::from_micros(42),
        base_fee_source: BaseFeeSource::Projected,
    }
}

/// USDC-quoted affected pool admits with its quote orientation. The frame
/// moving a USDC-quoted pair previously produced NO admitted pool at all
/// (the WETH-only admission cut it) — the per-frame quote set is {WETH,
/// USDC, USDT}, so this frame's settlement quote is USDC.
#[test]
fn usdc_quoted_pair_admits_with_quote_orientation() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let usdc_id = db
        .get_or_create_erc20_token(1, &USDC.to_checksum(None), None, None, None)
        .unwrap();
    let (tok_id, weth_id, usdc_id) = (
        u64::try_from(tok_id).unwrap(),
        u64::try_from(weth_id).unwrap(),
        u64::try_from(usdc_id).unwrap(),
    );
    let mut index = degenbot_bot::sidecar_paths::V2ConnectorIndex::default();
    // P2 + Q2 trade (TOK, USDC); the normalizer edges trade (USDC, WETH) so
    // the quote actually connects back to the base quote in the index.
    for (pool_id, t0, t1, addr) in [
        (103u64, tok_id, usdc_id, P2),
        (104u64, tok_id, usdc_id, Q),
        (105u64, usdc_id, weth_id, P),
    ] {
        index.push_edge(degenbot_bot::sidecar_paths::V2Edge {
            pool_id,
            token0_id: t0,
            token1_id: t1,
            address: addr,
        });
    }
    let rt = StrategyRuntime::new(1, Some(index), Some(db), 8);
    let outcome = usdc_frame_replay_outcome();

    let (descriptors, _) = build_descriptors(rt.index.as_ref(), &outcome.touched);
    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors,
    );
    assert_eq!(extracted.len(), 1);
    let mut solver = SidecarSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture");
    // The WETH-only admission cut this frame short: `affected` was empty, so
    // no quote-land discovery could ever start for a USDC-quoted pair.
    assert_eq!(
        affected.len(),
        1,
        "the USDC-quoted pair admits into the frame scope"
    );
    let quotes = &affected[0].quotes;
    assert_eq!(quotes.len(), 1, "one supported quote: USDC");
    assert_eq!(quotes[0].quote, USDC);
    assert_eq!(quotes[0].quote_id, usdc_id);
    assert_eq!(quotes[0].tok_id, tok_id);
    assert!(
        quotes.iter().all(|q| q.quote != WETH),
        "WETH-only selection yields no quote for this frame"
    );
}

/// Q2: the USDC frame's discovery connector (cheap TOK against a drained
/// USDC side).
const Q2: Address = address!("000000000000000000000000000000000000b004");
/// N1/N2: the two DISTINCT WETH/USDC normalization pools (the quote lane's
/// priced back-to-WETH legs; one pool twice would double-count its
/// reserves in the chain solve).
const N1: Address = address!("000000000000000000000000000000000000b005");
const N2: Address = address!("000000000000000000000000000000000000b006");

/// Reference constant-product output (independent of the mixed solver's
/// closed form): `out = in*997*r_out / (r_in*1000 + in*997)`.
fn reference_out(r_in: u128, r_out: u128, amount_in: u128) -> u128 {
    let num = U256::from(amount_in) * U256::from(997u64) * U256::from(r_out);
    let den = U256::from(r_in) * U256::from(1000u64) + U256::from(amount_in) * U256::from(997u64);
    u128::try_from(num / den).unwrap()
}

/// Brute-force optimum of a CLP chain with fees — the independent truth the
/// normalized-candidate assertions compare against (the solver's closed form
/// is derived independently of this scan).
fn best_chain_profit(chains: &[(u128, u128)], max_in: u128) -> (u128, u128) {
    let mut best = (0u128, 0u128);
    for w in 1..=max_in {
        let mut amt = w;
        for (r_in, r_out) in chains {
            amt = reference_out(*r_in, *r_out, amt);
        }
        if amt > w {
            let p = amt - w;
            if p > best.1 {
                best = (w, p);
            }
        }
    }
    best
}

/// The in-memory runtime fixture for the USDC frame: seeded DB + index
/// edges for P2/Q2 (TOK, USDC) and the N1/N2 (USDC, WETH) normalizers.
fn usdc_runtime_fixture() -> (StrategyRuntime, u64, u64, u64) {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let usdc_id = db
        .get_or_create_erc20_token(1, &USDC.to_checksum(None), None, None, None)
        .unwrap();
    let (tok_id, weth_id, usdc_id) = (
        u64::try_from(tok_id).unwrap(),
        u64::try_from(weth_id).unwrap(),
        u64::try_from(usdc_id).unwrap(),
    );
    let mut index = degenbot_bot::sidecar_paths::V2ConnectorIndex::default();
    for (pool_id, t0, t1, addr) in [
        (103u64, tok_id, usdc_id, P2),
        (104u64, tok_id, usdc_id, Q2),
        (105u64, usdc_id, weth_id, N1),
        (106u64, usdc_id, weth_id, N2),
    ] {
        index.push_edge(degenbot_bot::sidecar_paths::V2Edge {
            pool_id,
            token0_id: t0,
            token1_id: t1,
            address: addr,
        });
    }
    (
        StrategyRuntime::new(1, Some(index), Some(db), 8),
        tok_id,
        weth_id,
        usdc_id,
    )
}

/// Admit an abstract-units V2 fixture pool (canonical token order by
/// address, which the fixture constants already are).
fn admitted_pair(
    solver: &mut SidecarSolver,
    addr: Address,
    token0: Address,
    token1: Address,
    r0: u128,
    r1: u128,
) -> u64 {
    solver
        .admit_v2(&SidecarV2Pool {
            address: addr,
            token0,
            token1,
            reserve0: r0,
            reserve1: r1,
        })
        .unwrap()
}

/// The wrapped USDC-frame candidate: a quote-land 2-hop cycle closed in
/// WETH through TWO priced normalization pools. Every non-base-quote
/// candidate carries these hops as part of the candidate itself, and the
/// profit is wei — compared against the reference CLP chain (not the USDC
/// unit profit).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "fixture plumbing reads top-to-bottom: admit, fan, solve, compose, verify"
)]
fn usdc_frame_produces_candidate_with_priced_normalization() {
    let (rt, _tok_id, _weth_id, usdc_id) = usdc_runtime_fixture();
    let outcome = usdc_frame_replay_outcome();
    let (descriptors, _) = build_descriptors(rt.index.as_ref(), &outcome.touched);
    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors,
    );
    let mut solver = SidecarSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture");
    assert_eq!(affected.len(), 1);

    // Canonical orders: TOK(0x…0aa1) < USDC; USDC < WETH.
    let q2_id = admitted_pair(&mut solver, Q2, TOK, USDC, 200_000, 150);
    let n1_id = admitted_pair(&mut solver, N1, USDC, WETH, 1_000_000, 1_000_000);
    let n2_id = admitted_pair(&mut solver, N2, USDC, WETH, 500_000, 1_500_000);
    let norm = degenbot_submission::frame_pipeline::Normalization {
        // N1: WETH -> USDC (in = WETH = token1, zfo = false).
        inbound: SidecarHopRef {
            pool_id: n1_id,
            pool: N1,
            token0: USDC,
            token1: WETH,
            zfo: false,
            family: LaneFamily::V2,
        },
        // N2: USDC -> WETH (in = USDC = token0, zfo = true).
        outbound: SidecarHopRef {
            pool_id: n2_id,
            pool: N2,
            token0: USDC,
            token1: WETH,
            zfo: true,
            family: LaneFamily::V2,
        },
    };

    let stats = solve_fans(
        &mut solver,
        &affected[0],
        &[degenbot_submission::frame_pipeline::QuoteFan {
            quote: USDC,
            quote_id: usdc_id,
            connectors: vec![DiscoveredConnector {
                address: Q2,
                workspace_pool_id: q2_id,
                family: LaneFamily::V2,
            }],
            normalization: Some(norm),
        }],
        U256::ZERO,
    );

    assert_eq!(
        stats.cycles_declared, 2,
        "both wrapped drift cycles declared"
    );
    let best = stats.best.expect("the USDC frame composes a candidate");
    assert_eq!(
        best.hops.len(),
        4,
        "priced normalization hops ride the path"
    );
    assert_eq!(best.hops[0].pool, N1, "inbound WETH -> quote leg");
    assert_eq!(best.hops[1].pool, Q2, "quote-land arbitrage hop");
    assert_eq!(best.hops[3].pool, N2, "outbound quote -> WETH leg");
    assert!(
        !best.hops[0].zfo && best.hops[3].zfo,
        "directions close the WETH cycle"
    );
    assert_eq!(
        best.hop_outputs.len(),
        4,
        "per-hop alignment for the composer"
    );

    // The normalization hops compose: the 4-hop all-V2 stream encodes to
    // execute() calldata (InPathFlash repays the WETH entry from the WETH
    // output; the bid stays WETH-denominated).
    let cd = degenbot_bot::sidecar_engine::build_candidate_calldata(
        &best,
        address!("0x30b28ed8aa581fbc0191c3b532b0697773070e97"),
        WETH,
        9_800,
    )
    .expect("the normalized 4-hop candidate composes");
    assert!(cd.len() > 4 + 32 * 3 + 64, "execute() calldata shape");

    // Wei honesty: the recorded profit matches the independent CLP-chain
    // optimum (WETH in -> USDC -> TOK -> USDC -> WETH out), NOT the USDC
    // unit profit; the envelope compares exactly this number.
    let (w_star, p_star) = best_chain_profit(
        &[
            (1_000_000, 1_000_000), // N1: WETH -> USDC
            (150, 200_000),         // Q2: USDC -> TOK
            (900_000, 1_100),       // P2: TOK -> USDC (staged)
            (500_000, 1_500_000),   // N2: USDC -> WETH
        ],
        60_000,
    );
    assert!(p_star > 0, "the scanned chain profits");
    assert!(
        (i128::try_from(best.profit).unwrap() - i128::try_from(p_star).unwrap()).abs() <= 3,
        "wei profit {} vs reference {}",
        best.profit,
        p_star
    );
    assert!(
        best.optimal_input.abs_diff(w_star) <= 100,
        "optimal WETH input {} vs reference {}",
        best.optimal_input,
        w_star
    );
    assert!(
        best.profit > 60,
        "the normalized candidate beats this frame's WETH-only golden scale (55..56)"
    );
}

/// The honest drop: with no priceable normalization lane, the USDC fan's
/// candidates are refused (`non_base_quote_dropped`) and the frame's
/// observe label is `non_base_quote` — never a fake conversion for
/// ranking only.
#[test]
fn non_base_quote_drop_is_truthful() {
    let (rt, _tok, _weth, usdc_id) = usdc_runtime_fixture();
    let outcome = usdc_frame_replay_outcome();
    let (descriptors, _) = build_descriptors(rt.index.as_ref(), &outcome.touched);
    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors,
    );
    let mut solver = SidecarSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture");
    let q2_id = admitted_pair(&mut solver, Q2, TOK, USDC, 200_000, 150);

    let stats = solve_fans(
        &mut solver,
        &affected[0],
        &[degenbot_submission::frame_pipeline::QuoteFan {
            quote: USDC,
            quote_id: usdc_id,
            connectors: vec![DiscoveredConnector {
                address: Q2,
                workspace_pool_id: q2_id,
                family: LaneFamily::V2,
            }],
            normalization: None,
        }],
        U256::ZERO,
    );
    assert!(stats.best.is_none(), "nothing composed without the lane");
    assert!(stats.non_base_quote_dropped);
    assert_eq!(stats.cycles_declared, 0);
    assert_eq!(
        degenbot_submission::frame_pipeline::honest_observe("no_candidate", true, false),
        "non_base_quote"
    );
    // A WETH-closing candidate that did solve keeps the existing truthful
    // verdict; an unrelated observe reason is never rewritten.
    assert_eq!(
        degenbot_submission::frame_pipeline::honest_observe("no_candidate", true, true),
        "no_candidate"
    );
    assert_eq!(
        degenbot_submission::frame_pipeline::honest_observe("budget_exhausted", true, false),
        "budget_exhausted"
    );
}

/// Cross-quote ranking: a WETH frame's golden candidate (55..56 wei) and
/// the USDC frame's normalized candidate meet in one aggregate — the
/// selector compares wei, never quote units; the recorded profit is the
/// WETH-closed chain output, not the USDC-unit delta.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "two-quote fixture plumbing reads top-to-bottom: extract, admit, fan both, aggregate"
)]
fn mixed_quotes_rank_in_wei_not_quote_units() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let usdc_id = db
        .get_or_create_erc20_token(1, &USDC.to_checksum(None), None, None, None)
        .unwrap();
    let (tok_id, weth_id, usdc_id) = (
        u64::try_from(tok_id).unwrap(),
        u64::try_from(weth_id).unwrap(),
        u64::try_from(usdc_id).unwrap(),
    );
    let mut index = degenbot_bot::sidecar_paths::V2ConnectorIndex::default();
    for (pool_id, t0, t1, addr) in [
        (101u64, tok_id, weth_id, P),
        (102u64, tok_id, weth_id, Q),
        (103u64, tok_id, usdc_id, P2),
        (104u64, tok_id, usdc_id, Q2),
        (105u64, usdc_id, weth_id, N1),
        (106u64, usdc_id, weth_id, N2),
    ] {
        index.push_edge(degenbot_bot::sidecar_paths::V2Edge {
            pool_id,
            token0_id: t0,
            token1_id: t1,
            address: addr,
        });
    }
    let rt = StrategyRuntime::new(1, Some(index), Some(db), 8);

    // One frame touching the WETH pair (slot 8 drained) AND the USDC pair.
    let mut state = EvmState::default();
    for (pool, r0, r1) in [(P, 476_259u64, 1_050u64), (P2, 900_000u64, 1_100u64)] {
        let reserves = slot_layout::pack_v2_reserves_word(U112::from(r0), U112::from(r1));
        let mut account = Account::default();
        account.status = AccountStatus::Touched;
        account.storage.insert(
            U256::from(slot_layout::V2_RESERVES_SLOT),
            EvmStorageSlot {
                original_value: U256::ZERO,
                present_value: U256::from_be_bytes(reserves.0),
                transaction_id: revm::state::TransactionId::ZERO,
                is_cold: false,
            },
        );
        state.insert(pool, account);
    }
    let outcome = ReplayOutcome {
        status: ReplayStatus::Success,
        state,
        touched: vec![
            (P, vec![U256::from(slot_layout::V2_RESERVES_SLOT)]),
            (P2, vec![U256::from(slot_layout::V2_RESERVES_SLOT)]),
        ],
        rpc_reads: 0,
        wall: std::time::Duration::from_micros(42),
        base_fee_source: BaseFeeSource::Projected,
    };
    let (descriptors, _) = build_descriptors(rt.index.as_ref(), &outcome.touched);
    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors,
    );
    assert_eq!(extracted.len(), 2, "both tracked pools extracted");
    let mut solver = SidecarSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture");
    assert_eq!(affected.len(), 2, "WETH-pair AND USDC-pair both admit");
    let weth_pool = affected
        .iter()
        .find(|a| a.address == P)
        .expect("WETH orientation present");
    let usdc_pool = affected
        .iter()
        .find(|a| a.address == P2)
        .expect("USDC orientation present");
    assert!(weth_pool.quotes.iter().any(|q| q.quote == WETH));
    assert!(!usdc_pool.quotes.iter().any(|q| q.quote == WETH));

    // WETH fan (golden): connector Q at (200_000, 900).
    let weth_conn_id = admitted_pair(&mut solver, Q, TOK, WETH, 200_000, 900);
    let weth_stats = solve_fans(
        &mut solver,
        weth_pool,
        &[degenbot_submission::frame_pipeline::QuoteFan {
            quote: WETH,
            quote_id: weth_id,
            connectors: vec![DiscoveredConnector {
                address: Q,
                workspace_pool_id: weth_conn_id,
                family: LaneFamily::V2,
            }],
            normalization: None,
        }],
        U256::ZERO,
    );

    // USDC fan (wrapped): connector Q2 + the priced normalization pair.
    let usdc_conn_id = admitted_pair(&mut solver, Q2, TOK, USDC, 200_000, 150);
    let n1_id = admitted_pair(&mut solver, N1, USDC, WETH, 1_000_000, 1_000_000);
    let n2_id = admitted_pair(&mut solver, N2, USDC, WETH, 500_000, 1_500_000);
    let usdc_stats = solve_fans(
        &mut solver,
        usdc_pool,
        &[degenbot_submission::frame_pipeline::QuoteFan {
            quote: USDC,
            quote_id: usdc_id,
            connectors: vec![DiscoveredConnector {
                address: Q2,
                workspace_pool_id: usdc_conn_id,
                family: LaneFamily::V2,
            }],
            normalization: Some(degenbot_submission::frame_pipeline::Normalization {
                inbound: SidecarHopRef {
                    pool_id: n1_id,
                    pool: N1,
                    token0: USDC,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2,
                },
                outbound: SidecarHopRef {
                    pool_id: n2_id,
                    pool: N2,
                    token0: USDC,
                    token1: WETH,
                    zfo: true,
                    family: LaneFamily::V2,
                },
            }),
        }],
        U256::ZERO,
    );

    // Cross-quote aggregation exactly as the pipeline's solve stage does it.
    let mut aggregate = weth_stats.clone();
    aggregate.connectors += usdc_stats.connectors;
    aggregate.cycles_declared += usdc_stats.cycles_declared;
    aggregate.cycles_evaluated += usdc_stats.cycles_evaluated;
    aggregate.non_base_quote_dropped |= usdc_stats.non_base_quote_dropped;
    if usdc_stats.best.as_ref().is_some_and(|b| {
        aggregate
            .best
            .as_ref()
            .is_none_or(|best| b.profit > best.profit)
    }) {
        aggregate.best = usdc_stats.best;
    }

    let golden = weth_stats.best.expect("golden WETH candidate");
    assert!(
        (55..=56).contains(&golden.profit),
        "WETH candidate unchanged: {}",
        golden.profit
    );
    let normalized = aggregate.best.expect("aggregate carries a candidate");
    assert_eq!(
        normalized.hops[0].pool, N1,
        "the wei ranking picked the normalized USDC candidate"
    );
    assert!(
        normalized.profit > golden.profit,
        "wei comparison crossed quotes: {} > {}",
        normalized.profit,
        golden.profit
    );
    let (_w_star, p_star) = best_chain_profit(
        &[
            (1_000_000, 1_000_000),
            (150, 200_000),
            (900_000, 1_100),
            (500_000, 1_500_000),
        ],
        60_000,
    );
    assert!(
        (i128::try_from(normalized.profit).unwrap() - i128::try_from(p_star).unwrap()).abs() <= 3,
        "the aggregate's number is the WETH-closed chain optimum ({p_star}), not a USDC-unit delta"
    );
    assert!(!aggregate.non_base_quote_dropped, "nothing was dropped");
}

// ─────────────────────── the live dry-run e2e ──────────────────────────────

/// Live dry-run: the captured frame JSONL (`tests/fixtures/`, the
/// `/tmp/mb_trace.jsonl` conventions) flows the FULL pipeline — frame-replay
/// seam → extract → admission → discovery → solve → compose → gate — with
/// the CLASSIFIER GUARD ARMED: any hot-path classification aborts the run.
/// No DB/index is attached (offline-review without the workspace DB), so the
/// discovery fan stays shut and every observe reason must be truthful.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
#[expect(
    clippy::too_many_lines,
    reason = "the live dry-run reads top-to-bottom"
)]
async fn dry_run_fixture_frames_replay_end_to_end_without_classifier() {
    use std::path::PathBuf;
    use std::sync::Arc;

    use alloy::primitives::address;
    use alloy::providers::ProviderBuilder;
    use degenbot_bot::bot_core::SimAnchorState;
    use degenbot_bot::sidecar::{Decision, SidecarConfig};
    use degenbot_submission::frame_pipeline::{
        build_block_handle, load_fixture_frames, process_frame, PipelineConfig, StrategyRuntime,
    };

    // Arm the guard for the WHOLE run (this target never calls classify).
    std::env::set_var("DEGENBOT_CLASSIFIER_GUARD", "1");

    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let provider = degenbot_rpc::provider::AlloyProvider::from_provider(Arc::new(
        ProviderBuilder::default().connect_http(rpc_url.parse().unwrap()),
    ));
    let head = provider.get_block_number().await.unwrap();
    let pin = head.saturating_sub(1);

    // The trace capture lands in a temp file for in-process assertions.
    let trace_path = std::env::temp_dir().join("wkpzqk-e2e-trace.jsonl");
    let _ = std::fs::remove_file(&trace_path);
    std::env::set_var("SIDECAR_TRACE_JSONL", &trace_path);

    // Live mode needs no feed/signer/dispatcher: process frames directly.
    let mut runtime = StrategyRuntime::new(1, None, None, 8);
    let mut handle = Option::from(
        build_block_handle(
            &provider,
            pin,
            &runtime.warm_cache,
            Box::leak(Box::new(SimAnchorState::default())),
        )
        .await
        .expect("live replay handle builds"),
    );

    let sidecar = SidecarConfig {
        stream_url: String::new(),
        rpc_url: String::new(),
        key_file: None,
        bid_mode: false,
        budget_wei: U256::ZERO,
        max_bundle_wei: U256::from(1_000_000_000_000_000u64),
        stop_file: PathBuf::from("/nonexistent-wkpzqk"),
        stale_ms: 1500,
    };
    let pl = PipelineConfig {
        exec: address!("0x30b28ed8aa581fbc0191c3b532b0697773070e97"),
        owner: address!("0x5c603b8a137a40426e0ddfa981ec10c245af080e"),
        bribe_bips: 9_800,
        gas_floor_wei: U256::from(50_000_000_000_000u64),
    };
    // The bundle-sim client points at the SAME node join (read/sim only, and
    // only reached if a candidate ever composes — offline-review without a
    // DB never gets there: connectors are never guessed).
    let sim_client = alloy::rpc::client::ClientBuilder::default().http(rpc_url.parse().unwrap());

    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frame_replay_capture.jsonl");
    let frames = load_fixture_frames(&fixture);
    assert!(
        !frames.is_empty(),
        "the committed capture fixture is non-empty"
    );

    for ev in &frames {
        // A capture replayed at a LATER head has a stale nonce (the tx has
        // long since landed). Re-arm the frame's nonce to the pinned view's
        // parent count so the REPLAY path is genuinely exercised end-to-end
        // (the signer reaches the seam as recovered data; the seam validates
        // the nonce faithfully against the layered view).
        let mut frame = ev.clone();
        frame.nonce = provider
            .get_transaction_count(&ev.from, Some(pin.saturating_sub(1)))
            .await
            .unwrap_or(ev.nonce);
        let artifacts = process_frame(
            &mut runtime,
            &provider,
            &sim_client,
            &sidecar,
            &pl,
            &mut handle,
            &frame,
            pin,
            0,
            U256::ZERO,
        )
        .await;
        // Every decision is observe-only and the reason is TRUTHFUL: the
        // un-composed frame never reads as "the sim rejected our work".
        match &artifacts.decision {
            Decision::Observe { reason } => {
                assert!(
                    matches!(
                        *reason,
                        "no_candidate"
                            | "v4_unsupported"
                            | "reverted"
                            | "replay_failed"
                            | "replay_unavailable"
                    ),
                    "truthful observe reason, got {reason}"
                );
            }
            Decision::Bid { .. } => panic!("dry-run without index/db cannot bid"),
            Decision::Drop { reason } => {
                assert_eq!(*reason, "stale_candidate");
            }
        }
    }

    // The JSONL trace got the per-frame replay events (wall/rpc_reads/touched).
    let traced = std::fs::read_to_string(&trace_path).unwrap();
    let mut replay_events = 0;
    for line in traced.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v["kind"] == "replay" {
            replay_events += 1;
            // A settled frame carries the seam telemetry (wall/rpc_reads/
            // touched); a rejected frame (stale-nonce capture replayed at a
            // later head) carries the typed error instead. Both are truthful.
            assert!(
                (v["rpc_reads"].is_u64() && v["touched"].is_u64() && v["wall_us"].is_u64())
                    || v["error"].is_string(),
                "replay event shape: {v}"
            );
        }
    }
    assert_eq!(
        replay_events,
        frames.len(),
        "one replay event per fixture frame"
    );
}
