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
use degenbot_bot::sidecar_engine::{LaneFamily, SidecarSolver, SidecarV2Pool};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::slot_layout;
use degenbot_simulation::sim::evm::frame_replay::{BaseFeeSource, ReplayOutcome, ReplayStatus};
use degenbot_submission::frame_pipeline::{
    admit_extracted, build_descriptors, solve_pairs, state_digest, DiscoveredConnector,
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
    let stats = solve_pairs(
        &mut solver,
        &affected[0],
        &connectors,
        affected[0].token0,
        affected[0].token1,
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
