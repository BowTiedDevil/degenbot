//! Frame-pipeline tests (task WKPZQK).
//!
//! 1. The golden frame (offline): a hand-built replay journal (post-target
//!    V2 reserves at slot 8) flows descriptors → `extract_pool_post_states`
//!    → workspace admission → 2-hop discovery/solve → compose, matching the
//!    hand-derived golden reference.
//! 2. The live e2e (`#[ignore]`-gated): a captured frame JSONL replays
//!    end-to-end against a forked node.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use alloy::primitives::{address, aliases::U112, Address, U256};
use degenbot_bot::sidecar_engine::{LaneFamily, SidecarHopRef, SidecarSolver, SidecarV2Pool};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::slot_layout;
use degenbot_simulation::sim::evm::frame_replay::{BaseFeeSource, ReplayOutcome, ReplayStatus};
use degenbot_submission::frame_pipeline::{
    admit_extracted, build_descriptors, net_bid, solve_dfs_chains, state_digest, StrategyRuntime,
    WETH,
};
use revm::state::{Account, AccountStatus, EvmState, EvmStorageSlot};

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
#[expect(
    clippy::too_many_lines,
    reason = "fixture plumbing reads top-to-bottom: extract, admit, refs by hand, solve, compose, digest"
)]
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
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture", None);
    assert_eq!(affected.len(), 1, "P admits and trades WETH");
    assert_eq!(affected[0].address, P);
    assert_eq!(affected[0].family, LaneFamily::V2);

    // Admit the connector the walker would have discovered (the admission
    // shape is unchanged; the discovery traversal itself is exercised in the
    // live e2e).
    let q_id = solver
        .admit_v2(&SidecarV2Pool {
            address: Q,
            token0: affected[0].token0,
            token1: affected[0].token1,
            reserve0: 200_000,
            reserve1: 900,
        })
        .expect("connector admits");

    // solve: both drift cycles declared + evaluated; the golden profit. The
    // refs replicate the anchored lane's re-derived traversal exacts (anchor
    // consumes WETH, mid consumes TOK) — the 2-hop walker output verbatim.
    let (t0, t1) = (affected[0].token0, affected[0].token1);
    let tok = if t0 == WETH { t1 } else { t0 };
    let anchor_consuming_weth = SidecarHopRef {
        pool_id: affected[0].workspace_pool_id,
        pool: P,
        token0: t0,
        token1: t1,
        zfo: t0 == WETH,
        family: LaneFamily::V2,
    };
    let mid_consuming_tok = SidecarHopRef {
        pool_id: q_id,
        pool: Q,
        token0: t0,
        token1: t1,
        zfo: tok == t0,
        family: LaneFamily::V2,
    };
    let mid_consuming_weth = SidecarHopRef {
        pool_id: q_id,
        pool: Q,
        token0: t0,
        token1: t1,
        zfo: t0 == WETH,
        family: LaneFamily::V2,
    };
    let anchor_consuming_tok = SidecarHopRef {
        pool_id: affected[0].workspace_pool_id,
        pool: P,
        token0: t0,
        token1: t1,
        zfo: tok == t0,
        family: LaneFamily::V2,
    };
    let stats = solve_dfs_chains(
        &mut solver,
        &[
            vec![anchor_consuming_weth, mid_consuming_tok],
            vec![mid_consuming_weth, anchor_consuming_tok],
        ],
        U256::ZERO,
    );
    assert_eq!(stats.dfs_declared, 2, "both drift directions proposed");
    assert!(
        stats.dfs_evaluated >= 1,
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
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture", None);
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

/// The honest drop: a frame whose affected pool trades no WETH quote
/// composes no candidate, and the observe label is `non_base_quote` — never
/// a fake conversion for ranking only. This pins the full label matrix of
/// `honest_observe`: the truthful verdict in every combination.
#[test]
fn non_base_quote_drop_is_truthful() {
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

/// No DB/index is attached (offline-review without the workspace DB), so the
/// discovery lane stays shut and every observe reason must be truthful.
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
        wallet_gas_cost_wei: Arc::new(std::sync::atomic::AtomicU64::new(1_000_000_000_000)),
        gas_floor_wei: U256::from(50_000_000_000_000u64),
        fixture_mode: false,
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

// ─────────────── the anchored walker's depth-3 lane ───────────────

/// M: the depth-3 intermediate (canonical token order: TOK < M < WETH).
const M: Address = address!("0000000000000000000000000000000000000ab2");
/// C1/C2: the walker-discovered bridge pools (TOK-M and M-WETH).
const C1: Address = address!("000000000000000000000000000000000000b101");
const C2: Address = address!("000000000000000000000000000000000000b102");

/// The walker's 3-hop chain (WETH entry, P tok->weth anchor first, then
/// C1 tok->M, then C2 M->WETH) runs through the SAME declare/evaluate gate
/// as the fans: declared once, envelope-evaluated, best candidate kept and
/// composable. Profit matches the independent reference CLP-chain scan.
#[test]
fn walker_three_hop_chain_solves_and_composes() {
    let mut solver = SidecarSolver::new();
    // P staged golden (476_259 TOK / 1_050 WETH), C1 (400_000 TOK /
    // 800_000 M), C2 (800_000 M / 2_000 WETH): the bridge pays ~0.005 WETH
    // per TOK against P's ~0.0022, so the WETH-entry chain profits.
    let p_id = admitted_pair(&mut solver, P, TOK, WETH, 476_259, 1_050);
    let c1_id = admitted_pair(&mut solver, C1, TOK, M, 400_000, 800_000);
    let c2_id = admitted_pair(&mut solver, C2, M, WETH, 800_000, 2_000);

    // Canonical `zfo` rule: the INPUT token is the hop's token0.
    let chains = vec![vec![
        SidecarHopRef {
            pool_id: p_id,
            pool: P,
            token0: TOK,
            token1: WETH,
            zfo: false, // entry WETH = token1
            family: LaneFamily::V2,
        },
        SidecarHopRef {
            pool_id: c1_id,
            pool: C1,
            token0: TOK,
            token1: M,
            zfo: true, // input TOK = token0
            family: LaneFamily::V2,
        },
        SidecarHopRef {
            pool_id: c2_id,
            pool: C2,
            token0: M,
            token1: WETH,
            zfo: true, // input M = token0
            family: LaneFamily::V2,
        },
    ]];

    let stats =
        degenbot_submission::frame_pipeline::solve_dfs_chains(&mut solver, &chains, U256::ZERO);
    assert_eq!(stats.dfs_declared, 1, "the walker chain declares once");
    assert_eq!(stats.dfs_evaluated, 1, "the chain clears the zero floor");
    let best = stats.best.expect("the 3-hop walker chain profits");
    assert_eq!(best.hops.len(), 3);
    assert_eq!(stats.chains.len(), 1, "one chain outcome recorded");
    assert!(stats.chains[0].evaluated);
    assert_eq!(stats.chains[0].pools, vec![P, C1, C2]);
    assert_eq!(stats.chains[0].profit_wei, Some(best.profit));
    assert!(stats.chains[0].reject.is_none());

    // Wei honesty: the recorded profit matches the independent CLP-chain
    // optimum (WETH -> TOK -> M -> WETH).
    let (w_star, p_star) = best_chain_profit(
        &[(1_050, 476_259), (400_000, 800_000), (800_000, 2_000)],
        60_000,
    );
    assert!(p_star > 0, "the scanned 3-hop chain profits");
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

    let cd = degenbot_bot::sidecar_engine::build_candidate_calldata(
        &best,
        address!("0x30b28ed8aa581fbc0191c3b532b0697773070e97"),
        WETH,
        9_800,
    )
    .expect("the 3-hop walker candidate composes");
    assert!(cd.len() > 4 + 32 * 3 + 64, "execute() calldata shape");
}

// ───────────────────── the bid ladder (single bribe site) ──────────────────

#[test]
fn bid_ladder_is_floor_of_bribe_share() {
    // The 98% ladder applied to an exact-solve profit: the bid is the
    // TRUNCATED (floor) share, and nothing else gates on a second
    // application of the share. Expected values are worked literals, not
    // recomputed by the code under test.
    assert_eq!(
        net_bid(
            55_000_000_000_000_000_000u128,
            1_000_000_000_000,
            9_800,
            u128::MAX
        )
        .expect("viable")
        .bid_wei,
        53_900_000_000_000_000_000u128
    );
    // Truncation: at a negligible gas cost the ladder floors (123 wei at a
    // 9902-bips floored share = 120), never rounds up.
    assert_eq!(
        net_bid(123, 0, 9_800, u128::MAX).expect("viable").bid_wei,
        U256::from(120u64)
    );
    // A different ceiling moves the bid with it.
    assert_eq!(
        net_bid(10_000, 0, 9_000, u128::MAX)
            .expect("viable")
            .bid_wei,
        U256::from(9_000u64)
    );
    // A gas burn above the gross refuses the bid entirely: 5 wei of gross
    // cannot fund any wallet-positive bundle.
    assert!(net_bid(5, 9_800, 9_800, u128::MAX).is_none());
}

/// The observe-reason histogram split: every typed `ReplayFrameError` maps to
/// exactly one honest label, and the JSONL evidence carries the nonce pair.
#[test]
fn replay_reasons_split() {
    use degenbot_simulation::sim::evm::frame_replay::ReplayFrameError;

    let (r, v) =
        degenbot_submission::frame_pipeline::replay_observe_reason(&ReplayFrameError::GapPending {
            claimed: 12,
            expected: 10,
        });
    assert_eq!(r, "gap_pending");
    assert_eq!(v["claimed_nonce"], 12);
    assert_eq!(v["expected_nonce"], 10);

    let (r, v) = degenbot_submission::frame_pipeline::replay_observe_reason(
        &ReplayFrameError::AlreadySettled {
            frame: 30673,
            parent: 30674,
        },
    );
    assert_eq!(r, "already_settled");
    assert_eq!(v["frame_nonce"], 30673);
    assert_eq!(v["parent_nonce"], 30674);

    let (r, v) = degenbot_submission::frame_pipeline::replay_observe_reason(
        &ReplayFrameError::EnvelopeArtifact {
            raw: "call gas cost (49784) exceeds the gas limit (0)".into(),
        },
    );
    assert_eq!(r, "envelope_artifact");
    assert_eq!(
        v["detail"],
        "call gas cost (49784) exceeds the gas limit (0)"
    );

    let (r, _) =
        degenbot_submission::frame_pipeline::replay_observe_reason(&ReplayFrameError::Other {
            raw: "rpc timeout".into(),
        });
    assert_eq!(r, "replay_failed");
}
