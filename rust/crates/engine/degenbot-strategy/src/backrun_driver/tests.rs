//! Driver-assembly tests: the boot registry parity across standalone and
//! hosted boot, the loop lifecycle table, the bid-economics policy pins, and
//! the quarantine rescue-routing folds.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use crate::backrun::{MevblockerBackrun, SubmissionSlot, TxpoolBackrun};
use crate::frame_pipeline::predecessor_observe_reason;
use crate::gap_quarantine::{ParkedFrame, Quarantine, QuarantineDecision};
use crate::gap_quarantine_journal::{ParkRecord, QuarantineJournal};
use alloy::primitives::{Address, B256};
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
use std::sync::Arc;

use super::driver_loop::{
    decode_predecessor, journal_reentry_outcome, nonce_lane_evidence, outcome_for,
    pool_known_gap_for_tick, predecessor_hash, quarantine_journal_path, record_gap_park,
    reentry_outcome, route_failed_hydration, route_unhydratable_hydration, run_frame, DriverHandle,
    FrameOutcome, GapParkMemo, LoopDecline, LoopPhase, LoopShared, ReentryOutcome, FEED_PREFIX,
};
use super::node_capability::NodeCapability;
use crate::frame_pipeline::{bid_submission_target, build_broadcast_relays};

/// The strategy boot product owns one DB-backed registry, and every concrete
/// ecosystem composition views those same facts. The fixture seeds the
/// canonical USDC/WETH V2 connector plus a V3 connector, pinning the order --
/// the V2 scan, then its V3 additions, then the ranker and frozen registry.
#[tokio::test]
async fn one_boot_product_shares_db_registry_graph_and_policy_facts() {
    use alloy::primitives::address;
    use degenbot_db::{V2PoolRowInput, V3PoolRowInput};
    use degenbot_substrate::pool_ingress::VerifyLevel;

    const USDC_WETH_V2: Address = address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");
    const USDC_WETH_V3: Address = address!("8ad599c3a0ff1de082011efddc58f1908eb6e6d8");
    const USDC: Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("connectors.db");
    {
        let (db, _) = degenbot_db::DegenbotDb::open_for_writes(&db_path).expect("seed db");
        db.lock()
            .execute(
                "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, \
                 factory, deployer) VALUES (1, 1, 'uniswap_v2', 1, NULL, \
                 '0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f', NULL)",
                (),
            )
            .expect("seed exchange");
        db.upsert_v2_pools(
            1,
            "uniswap_v2",
            1,
            10_000,
            &[V2PoolRowInput {
                address: USDC_WETH_V2,
                token0_address: USDC,
                token1_address: WETH,
                fee_token0: 300,
                fee_token1: 300,
                stable: None,
            }],
        )
        .expect("seed v2 connector");
        db.upsert_v3_pools(
            1,
            "uniswap_v3",
            1,
            1_000_000,
            &[V3PoolRowInput {
                address: USDC_WETH_V3,
                token0_address: USDC,
                token1_address: WETH,
                fee: 3_000,
                tick_spacing: 60,
            }],
        )
        .expect("seed v3 connector");
    }
    let provider = Arc::new(
        AlloyProvider::new("http://127.0.0.1:1", DEFAULT_MAX_RETRIES)
            .await
            .expect("provider builds without a node"),
    );
    let mut config = degenbot_config::BotConfig::default();
    config.strategy.mevblocker_backrun.verify_ticks = degenbot_config::VerifyTicks::Strict;
    config.strategy.txpool_backrun.verify_ticks = degenbot_config::VerifyTicks::Off;

    let resources = super::resolve_backrun_boot(
        Arc::new(config),
        db_path,
        super::BackrunNodeJoin {
            rpc_url: "http://127.0.0.1:1".to_string(),
            provider,
            chain_id: 1,
            capability: Arc::new(super::AnyRequestTransport),
        },
    )
    .await;
    let mevblocker = resources.strategy_boot(super::BackrunEcosystem::Mevblocker);
    let peer = resources.strategy_boot(super::BackrunEcosystem::Txpool);

    assert!(mevblocker.registry().is_registered_pool(&USDC_WETH_V2));
    assert!(mevblocker.registry().is_registered_pool(&USDC_WETH_V3));
    assert_eq!(mevblocker.registry().registered_pool_count(), 2);
    assert!(
        Arc::ptr_eq(mevblocker.registry(), peer.registry()),
        "both compositions consume the one frozen registry"
    );
    assert!(
        Arc::ptr_eq(
            mevblocker.connector_db().expect("held connector DB"),
            peer.connector_db().expect("held connector DB"),
        ),
        "the hosted facets never reopen the same DB"
    );
    assert!(mevblocker.dfs().is_some());
    assert!(peer.dfs().is_some());
    assert_eq!(mevblocker.verify_level(), VerifyLevel::Strict);
    assert_eq!(peer.verify_level(), VerifyLevel::Off);
}

/// The lifecycle FSM is total and closed: every state answers every verb
/// with a next state or a typed decline.
#[test]
fn lifecycle_transition_table_is_total_and_closed() {
    for state in [
        LoopPhase::Starting,
        LoopPhase::Running,
        LoopPhase::Stopping,
        LoopPhase::Stopped,
    ] {
        if state == LoopPhase::Starting {
            assert_eq!(state.on_running(), Ok(LoopPhase::Running));
        } else {
            assert_eq!(state.on_running(), Err(LoopDecline::StartRequiresStarting));
        }

        if matches!(state, LoopPhase::Starting | LoopPhase::Running) {
            assert_eq!(state.on_stop(), Ok(LoopPhase::Stopping));
        } else {
            assert_eq!(state.on_stop(), Err(LoopDecline::StopRequiresLive));
        }

        if state == LoopPhase::Stopped {
            assert_eq!(state.on_stopped(), Err(LoopDecline::StoppedRequiresLive));
        } else {
            assert_eq!(state.on_stopped(), Ok(LoopPhase::Stopped));
        }
    }
}

/// A stop request flips a live handle to `Stopping` and arms the loop's
/// flag; a second request declines, and the loop's return lands `Stopped`.
#[tokio::test]
async fn stop_walks_a_live_handle_to_stopping_and_is_idempotent() {
    let shared = Arc::new(LoopShared::new());
    let handle = DriverHandle {
        shared: Arc::clone(&shared),
        run: Some(Box::pin(async {})),
    };
    assert_eq!(handle.state(), LoopPhase::Starting);
    assert_eq!(handle.stop(), Ok(LoopPhase::Stopping));
    assert!(shared.stop_requested());
    assert_eq!(
        handle.stop(),
        Err(LoopDecline::StopRequiresLive),
        "a stopping driver refuses a second stop"
    );
    shared.mark_stopped();
    assert_eq!(handle.state(), LoopPhase::Stopped);
    assert!(handle.state().is_terminal());
}

/// A stop requested before the loop's first poll is not resurrected by
/// `begin_running`: the loop sees the armed flag and drains.
#[tokio::test]
async fn begin_running_does_not_resurrect_a_requested_stop() {
    let shared = LoopShared::new();
    assert_eq!(shared.request_stop(), Ok(LoopPhase::Stopping));
    shared.begin_running();
    assert_eq!(*shared.state.lock(), LoopPhase::Stopping);
    assert!(shared.stop_requested());
    shared.mark_stopped();
    assert_eq!(*shared.state.lock(), LoopPhase::Stopped);
}

#[tokio::test]
async fn broadcast_relays_are_private_first_with_read_provider_fallback() {
    let provider = Arc::new(
        AlloyProvider::new("http://node.local:8545", DEFAULT_MAX_RETRIES)
            .await
            .expect("lazy http provider"),
    );
    let mut cfg =
        MevblockerBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
            .into_config();
    assert!(
        build_broadcast_relays(&cfg, &provider).await.is_empty(),
        "an unset private endpoint must leave the read-provider-only list"
    );
    cfg.submission = SubmissionSlot::Mevblocker {
        bundle_url: String::from("wss://searchers.mevblocker.io"),
        private_url: Some(String::from("http://private.local:8545")),
    };
    let relays = build_broadcast_relays(&cfg, &provider).await;
    assert_eq!(relays.len(), 2, "private endpoint + read provider");
    assert_eq!(relays[0].rpc_url(), "http://private.local:8545");
    assert!(
        Arc::ptr_eq(&relays[1], &provider),
        "the read provider is the fallback relay"
    );
    assert!(
        !Arc::ptr_eq(&relays[0], &provider),
        "the private endpoint leads, so it is not the read provider"
    );
}

#[tokio::test]
async fn peer_slot_fans_out_public_relays_with_read_provider_fallback() {
    let provider = Arc::new(
        AlloyProvider::new("http://node.local:8545", DEFAULT_MAX_RETRIES)
            .await
            .expect("lazy http provider"),
    );
    let mut cfg = TxpoolBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
        .into_config();
    cfg.submission = SubmissionSlot::PublicFanOut {
        relays: vec![String::from("http://relay.one:8545")],
    };
    let relays = build_broadcast_relays(&cfg, &provider).await;
    assert_eq!(relays.len(), 2, "public relay + read provider fallback");
    assert_eq!(relays[0].rpc_url(), "http://relay.one:8545");
    assert!(Arc::ptr_eq(&relays[1], &provider));
}

/// The submission-slot divergence the two per-ecosystem strategies exist to
/// express: the `MEVBlocker` slot leads with its private endpoint and anchors a
/// bundle; the peer slot names no private endpoint and fans out publicly.
#[test]
fn submission_slots_diverge_on_private_first_and_target() {
    use degenbot_submission::submit::SubmissionTarget;

    let mevblocker =
        MevblockerBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
            .into_config();
    let mut mevblocker = mevblocker;
    mevblocker.submission = SubmissionSlot::Mevblocker {
        bundle_url: String::from("wss://searchers.mevblocker.io"),
        private_url: Some(String::from("http://private.local:8545")),
    };
    assert_eq!(
        mevblocker
            .submission
            .raw_relay_urls()
            .first()
            .map(String::as_str),
        Some("http://private.local:8545"),
        "the MEVBlocker submit path names the private endpoint first"
    );
    assert!(mevblocker.submission.names_private_endpoint());
    let hash = B256::repeat_byte(0x11);
    assert!(matches!(
        bid_submission_target(&mevblocker, hash, 21_000_001, None),
        Some(SubmissionTarget::Bundle(_))
    ));

    let mut peer =
        TxpoolBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
            .into_config();
    peer.submission = SubmissionSlot::BuilderRelay {
        relays: vec![String::from("https://relay.flashbots.net")],
    };
    assert!(!peer.submission.names_private_endpoint());
    assert!(
        peer.submission.raw_relay_urls().is_empty(),
        "the builder-relay submit path names NO raw fan-out: the bundle POST is the submission"
    );
    // A frame without the target's raw signed bytes has no bundle: the
    // builder-relay slot refuses it (never a raw broadcast).
    assert!(bid_submission_target(&peer, hash, 21_000_001, None).is_none());
    let target_raw = alloy::primitives::Bytes::from(vec![0x02, 0xf8, 0x01]);
    assert!(matches!(
        bid_submission_target(&peer, hash, 21_000_001, Some(&target_raw)),
        Some(SubmissionTarget::BuilderRelay { .. })
    ));
}

/// Both per-ecosystem compositions ride one `StrategyHost`: each is
/// independently activatable, and one config with both facets active binds
/// the divergent submission slots (the `MEVBlocker` arm's private endpoint,
/// the peer arm's public fan-out).
#[test]
fn both_backrun_compositions_host_independently_in_one_process() {
    use degenbot_bot::nonce_authority::{NonceAuthority, StrategyId};
    use degenbot_bot::strategy_host::{DriverPose, FacetStatus, StrategyHost};
    use degenbot_eventhub::Hub;
    use degenbot_substrate::connector_index::V2ConnectorIndex;
    use degenbot_substrate::RouteRegistry;

    let mut cfg = degenbot_config::BotConfig::default();
    cfg.strategy.mevblocker_backrun.active = true;
    cfg.strategy.mevblocker_backrun.endpoints = Some(String::from("wss://searchers.mevblocker.io"));
    cfg.strategy.mevblocker_backrun.mevblocker_url =
        Some(String::from("http://private.local:8545"));
    cfg.strategy.txpool_backrun.active = true;
    cfg.strategy.txpool_backrun.endpoints = Some(String::from("http://relay.one:8545"));

    let mevblocker = MevblockerBackrun::from_config(&cfg, String::new());
    let peer = TxpoolBackrun::from_config(&cfg, String::new());
    assert_eq!(
        mevblocker.config().submission.raw_relay_urls(),
        vec![String::from("http://private.local:8545")]
    );
    assert!(mevblocker.config().submission.names_private_endpoint());
    assert!(
        peer.config().submission.raw_relay_urls().is_empty(),
        "the builder-relay slot names no raw fan-out: the bundle POST is the submission"
    );
    assert!(!peer.config().submission.names_private_endpoint());

    let mut host = StrategyHost::new(
        Arc::new(Hub::new()),
        Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
        Arc::new(NonceAuthority::new(1)),
    );
    let mevblocker_id = StrategyId::new("mevblocker_backrun");
    let peer_id = StrategyId::new("txpool_backrun");
    host.register(mevblocker_id.clone(), FacetStatus::Configured)
        .expect("register mevblocker");
    host.register(peer_id.clone(), FacetStatus::Configured)
        .expect("register peer");

    assert_eq!(host.enable(&mevblocker_id), Ok(DriverPose::Enabled));
    assert_eq!(
        host.state_of(&peer_id),
        Some(DriverPose::Registered),
        "enabling one facet leaves the other dormant"
    );
    assert_eq!(host.enable(&peer_id), Ok(DriverPose::Enabled));
    assert_eq!(host.state_of(&mevblocker_id), Some(DriverPose::Enabled));
}

#[test]
fn rescue_outcome_mapping_routes_transient_gap_and_terminal() {
    assert_eq!(
        reentry_outcome(FrameOutcome::ReplayUnavailable),
        ReentryOutcome::Transient
    );
    assert_eq!(
        reentry_outcome(FrameOutcome::ReplayFailed),
        ReentryOutcome::Transient
    );
    assert_eq!(
        reentry_outcome(FrameOutcome::GapPending),
        ReentryOutcome::GapPending
    );
    assert_eq!(reentry_outcome(FrameOutcome::Bid), ReentryOutcome::Terminal);
    assert_eq!(
        reentry_outcome(FrameOutcome::Terminal("already_settled")),
        ReentryOutcome::Terminal
    );
    assert_eq!(
        outcome_for("replay_unavailable"),
        FrameOutcome::ReplayUnavailable
    );
    assert_eq!(outcome_for("replay_failed"), FrameOutcome::ReplayFailed);
    assert_eq!(outcome_for("gap_pending"), FrameOutcome::GapPending);
    assert_eq!(
        outcome_for("already_settled"),
        FrameOutcome::Terminal("already_settled")
    );
}

/// The predecessor-vs-frame split: a predecessor's structural death owns
/// the pool-pred structural lane; every retryable predecessor class is
/// transient; only the FRAME's own malformed death is terminal.
#[test]
fn predecessor_failure_classes_route_by_evidence_not_by_death() {
    use degenbot_simulation::sim::evm::frame_replay::ReplayFrameError;

    // Structurally unrunnable as fetched -> the structural lane (guarded
    // with content by the rescue arm).
    assert_eq!(
        predecessor_observe_reason(&ReplayFrameError::MalformedTransaction {
            raw: "bad shape".into(),
        }),
        "predecessor_malformed"
    );
    assert_eq!(
        outcome_for("predecessor_malformed"),
        FrameOutcome::PredecessorMalformed
    );

    // RPC/validation class -> transient, no guard.
    assert_eq!(
        predecessor_observe_reason(&ReplayFrameError::Other {
            raw: "rpc timeout".into(),
        }),
        "predecessor_replay_failed"
    );
    assert_eq!(
        outcome_for("predecessor_replay_failed"),
        FrameOutcome::ReplayFailed
    );
    assert_eq!(
        reentry_outcome(outcome_for("predecessor_replay_failed")),
        ReentryOutcome::Transient
    );

    // A predecessor that is itself ahead of the parent (a pool/RPC skew)
    // is also retryable, not structural.
    assert_eq!(
        predecessor_observe_reason(&ReplayFrameError::GapPending {
            claimed: 12,
            expected: 10,
        }),
        "predecessor_replay_failed"
    );

    // Mispriced-for-now (base fee rejected even after the disabled retry)
    // -> transient, no guard: the owner may replace it.
    assert_eq!(
        predecessor_observe_reason(&ReplayFrameError::Mispriced {
            raw: "base fee rejected after retry".into(),
        }),
        "predecessor_replay_failed"
    );
    assert_eq!(
        reentry_outcome(outcome_for("mispriced_transaction")),
        ReentryOutcome::Transient
    );

    // The FRAME's own malformed death keeps chunk-A terminal semantics.
    let (reason, _) =
        crate::frame_pipeline::replay_observe_reason(&ReplayFrameError::MalformedTransaction {
            raw: "call gas cost exceeds the gas limit".into(),
        });
    assert_eq!(reason, "malformed_transaction");
    assert_eq!(
        outcome_for(reason),
        FrameOutcome::Terminal("malformed_transaction")
    );
    assert_eq!(
        reentry_outcome(outcome_for(reason)),
        ReentryOutcome::Terminal
    );

    // A transient predecessor pass records NO guard: the identical rescue
    // re-fires on the next head.
    let mut q = Quarantine::new();
    let f = parked(77);
    q.push(f.clone());
    assert_eq!(
        q.poll_with_content(f.from, f.expected_at_capture, &[10, 11], &[])[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        }
    );
    journal_reentry_outcome(&f, ReentryOutcome::Transient, &mut q, None);
    assert_eq!(
        q.poll_with_content(
            f.from,
            f.expected_at_capture,
            &[10, 11],
            &[(10, B256::repeat_byte(0x01)), (11, B256::repeat_byte(0x02))]
        )[0]
        .1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        },
        "a transient predecessor failure records no guard, so the rescue re-fires"
    );
}

/// A predecessor-malformed route records the guard WITH content, keeps the
/// frame parked and unresolved, suppresses the identical pool-pred rescue,
/// and re-arms on a same-nonce replacement.
#[test]
fn predecessor_malformed_parks_with_guard_content_and_rearms_on_replacement() {
    use crate::gap_quarantine::FrameState;

    let (dir, path) = temp_journal("predecessor-malformed");
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let mut q = Quarantine::new();
    let f = parked(76);
    let content = [B256::repeat_byte(0xa1), B256::repeat_byte(0xa2)];
    route_unhydratable_hydration(
        &f,
        &[10, 11],
        &content,
        "malformed_predecessor",
        &mut q,
        Some(&mut journal),
    );
    assert_eq!(
        q.state(f.hash),
        Some(FrameState::Tracked),
        "the frame stays parked (un-resolved)"
    );
    assert_eq!(
        resolve_count(&path),
        0,
        "no resolve on predecessor evidence"
    );

    let unchanged = [(10, content[0]), (11, content[1])];
    assert_eq!(
        q.poll_with_content(f.from, 10, &[10, 11], &unchanged)[0].1,
        QuarantineDecision::StillWaiting {
            unknown: vec![10, 11]
        },
        "identical content is suppressed"
    );

    let replaced = [(10, B256::repeat_byte(0xb1)), (11, content[1])];
    assert_eq!(
        q.poll_with_content(f.from, 10, &[10, 11], &replaced)[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        },
        "a replaced predecessor re-arms"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A hashless (or zero-hash) node tx object is a broken shape: no hash is
/// lifted, so no `B256::ZERO` can ever enter guard content.
#[test]
fn hashless_predecessor_never_enters_guard_content() {
    assert_eq!(predecessor_hash(&serde_json::json!({})), None);
    assert_eq!(
        predecessor_hash(&serde_json::json!({"hash": "nothex"})),
        None
    );
    assert_eq!(
        predecessor_hash(&serde_json::json!({
            "hash": "0x0000000000000000000000000000000000000000000000000000000000000000"
        })),
        None
    );
    let usable = predecessor_hash(&serde_json::json!({
        "hash": "0x00000000000000000000000000000000000000000000000000000000000000ab"
    }))
    .expect("usable hash");
    assert!(!usable.is_zero());
}

#[test]
fn nonce_lane_failure_is_no_evidence_never_max() {
    // D1 pin: a failed read must not fabricate u64::MAX consumption
    // for every tracked frame of the sender - it is no evidence; the
    // caller skips the tick and every frame holds.
    assert!(nonce_lane_evidence::<()>(Err(())).is_none());
    assert!(
        nonce_lane_evidence::<()>(Ok(serde_json::json!(
            "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        )))
        .is_none(),
        "a nonce too wide for u64 is no evidence, never a saturated one"
    );
    assert_eq!(
        nonce_lane_evidence::<()>(Ok(serde_json::json!("0x7444"))),
        Some(29_764)
    );
}

/// A failed pending-lane read is NO pool evidence: the gap degrades to
/// empty, but the tick still runs the latest-lane classification and a
/// mined frontier rescues with an empty prefix.
#[test]
fn failed_pending_read_degrades_to_empty_gap_and_frontier_still_rescues() {
    let mut q = Quarantine::new();
    let f = parked(71);
    q.push(f.clone());
    let gap = pool_known_gap_for_tick(&q, f.from, f.claimed_nonce, None);
    assert!(
        gap.is_empty(),
        "a failed pending read yields no pool evidence"
    );
    let out = q.poll(f.from, f.claimed_nonce, &gap);
    assert_eq!(
        out[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![]
        },
        "the mined frontier is unaffected by the pending-lane failure"
    );
    assert!(q.is_empty());
}

/// A predecessor miss or decode failure hydrates nothing and routes the
/// rescue as transient: through the journal fold the original frame is
/// re-parked and no resolve is written.
#[test]
fn predecessor_hydration_failure_routes_transient_through_the_fold() {
    use crate::gap_quarantine::FrameState;

    assert!(decode_predecessor(Address::ZERO, 10, &serde_json::Value::Null).is_err());
    assert!(decode_predecessor(Address::ZERO, 10, &serde_json::json!({"to": null})).is_err());
    let good = serde_json::json!({
        "to": "0x0000000000000000000000000000000000000001",
        "value": "0x0",
        "input": "0x",
        "gas": "0x5208",
        "maxFeePerGas": "0x1",
        "maxPriorityFeePerGas": "0x1",
        "nonce": "0xa",
    });
    let decoded = decode_predecessor(Address::ZERO, 10, &good).expect("valid predecessor");
    assert_eq!(decoded.nonce, 10);
    assert_eq!(decoded.gas_limit, 21_000);

    let (dir, path) = temp_journal("hydration-failure");
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let mut q = Quarantine::new();
    let f = parked(72);
    route_failed_hydration(&f, &mut q, Some(&mut journal));
    assert_eq!(q.state(f.hash), Some(FrameState::Tracked));
    assert_eq!(resolve_count(&path), 0, "a transient writes no resolve");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A type-0 (legacy) predecessor envelope carries `gasPrice` instead of the
/// EIP-1559 fee pair; the decode accepts it for both fee fields.
#[test]
fn legacy_type_zero_predecessor_decodes_via_gas_price() {
    let legacy = serde_json::json!({
        "to": "0x0000000000000000000000000000000000000001",
        "value": "0x0",
        "input": "0x",
        "gas": "0x5208",
        "gasPrice": "0x3b9aca00",
        "nonce": "0xa",
        "type": "0x0",
    });
    let decoded = decode_predecessor(Address::ZERO, 10, &legacy).expect("legacy envelope");
    assert_eq!(decoded.max_fee_per_gas, 1_000_000_000);
    assert_eq!(decoded.max_priority_fee_per_gas, 1_000_000_000);
}

/// An RPC-error (or null) hydration failure records no guard: the next tick
/// re-fires the identical rescue instead of sticking the frame.
#[test]
fn transient_hydration_failure_records_no_guard_and_retries() {
    let mut q = Quarantine::new();
    let f = parked(73);
    q.push(f.clone());
    assert_eq!(
        q.poll(f.from, 10, &[10, 11])[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        }
    );
    route_failed_hydration(&f, &mut q, None);
    assert_eq!(
        q.poll(f.from, 10, &[10, 11])[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        },
        "no guard was recorded, so the rescue retries"
    );
}

/// A structural decode failure suppresses the failed pool-pred list once,
/// while the frame still re-enters through the empty mined-frontier rescue.
#[test]
fn unhydratable_predecessor_suppresses_pool_pred_and_keeps_the_frontier() {
    let mut q = Quarantine::new();
    let f = parked(74);
    q.push(f.clone());
    assert_eq!(
        q.poll(f.from, 10, &[10, 11])[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        }
    );
    let content = [B256::repeat_byte(0xa1), B256::repeat_byte(0xa2)];
    route_unhydratable_hydration(&f, &[10, 11], &content, "unparseable value", &mut q, None);
    assert_eq!(
        q.poll_with_content(f.from, 10, &[10, 11], &[(10, content[0]), (11, content[1])])[0].1,
        QuarantineDecision::StillWaiting {
            unknown: vec![10, 11]
        },
        "the failed pool-pred list is suppressed while its content is unchanged"
    );
    // Head reaches the claim: the mined frontier needs no predecessors.
    assert_eq!(
        q.poll(f.from, 12, &[])[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![]
        },
        "the empty mined-frontier rescue is a different evidence class"
    );
    assert!(q.is_empty());
}

/// Two consecutive identical `(hash, boundary)` re-parks write exactly one
/// journal line; the fold still shows the frame parked.
#[test]
fn consecutive_identical_gap_parks_write_one_journal_line() {
    let (dir, path) = temp_journal("gap-dedup");
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let f = parked(75);
    let record = ParkRecord::new(&f.to_event(), f.expected_at_capture, 1);
    let mut memo: GapParkMemo = None;
    assert!(record_gap_park(
        Some(&mut journal),
        &record,
        f.hash,
        f.expected_at_capture,
        &mut memo,
    ));
    assert!(
        !record_gap_park(
            Some(&mut journal),
            &record,
            f.hash,
            f.expected_at_capture,
            &mut memo,
        ),
        "a consecutive identical re-park is folded"
    );
    let raw = std::fs::read_to_string(&path).expect("raw");
    assert_eq!(
        raw.lines()
            .filter(|line| line.contains("\"kind\":\"park\""))
            .count(),
        1,
        "exactly one park line"
    );
    let read = crate::gap_quarantine_journal::read_pending(&path).expect("read");
    assert_eq!(
        read.pending.len(),
        1,
        "the fold still shows the frame parked"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The feed loop and the quarantine rescue re-entry hand `run_frame` the
/// same runtime surfaces: both call sites share one argument list, so a
/// signature drift on either side is a compile error.
#[test]
fn feed_loop_and_rescue_reentry_share_run_frame_surface() {
    let _ = run_frame;
    assert!(
        FEED_PREFIX.is_empty(),
        "the feed path replays with an empty predecessor prefix"
    );
}

fn temp_journal(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "degenbot-knobs-reentry-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join(crate::gap_quarantine_journal::JOURNAL_FILE_NAME);
    (dir, path)
}

fn resolve_count(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |raw| {
        raw.lines()
            .filter(|line| line.contains("\"kind\":\"resolve\""))
            .count()
    })
}

fn parked(hash_byte: u8) -> ParkedFrame {
    use alloy::primitives::{Address, Bytes, B256, U256};
    let mut raw = [0u8; 32];
    raw[0] = hash_byte;
    ParkedFrame {
        hash: B256::from(raw),
        chain_id: 1,
        from: Address::with_last_byte(7),
        to: None,
        value: U256::ZERO,
        data: Bytes::new(),
        gas: 300_000,
        max_fee_per_gas: 514_684_409,
        max_priority_fee_per_gas: 1_000_000_000,
        claimed_nonce: 12,
        expected_at_capture: 10,
        tx_type: 2,
        access_list: serde_json::json!([]),
        received_unix_ms: 1_700_000_000_000,
        raw_signed_tx: None,
    }
}

/// P1: a transient re-entry failure re-parks the ORIGINAL frame and writes
/// no resolve, so the boot fold reloads it alive.
#[test]
fn transient_reentry_failure_reparks_and_leaves_no_resolve() {
    let (dir, path) = temp_journal("transient");
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let mut q = Quarantine::new();
    let f = parked(61);
    journal_reentry_outcome(&f, ReentryOutcome::Transient, &mut q, Some(&mut journal));

    assert_eq!(
        q.state(f.hash),
        Some(crate::gap_quarantine::FrameState::Tracked),
        "the original frame is back in quarantine"
    );
    assert_eq!(
        resolve_count(&path),
        0,
        "a transient failure writes no resolve"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// P2: a gap-pending re-entry already re-parked the frame and wrote the
/// fresh park; the router adds no resolve and no duplicate.
#[test]
fn gap_pending_reentry_does_not_resolve_or_duplicate() {
    let (dir, path) = temp_journal("gap-pending");
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let mut q = Quarantine::new();
    let f = parked(62);
    journal_reentry_outcome(&f, ReentryOutcome::GapPending, &mut q, Some(&mut journal));

    assert_eq!(q.len(), 0, "run_frame already re-parked; no duplicate");
    assert_eq!(resolve_count(&path), 0, "the fresh park is its own truth");

    let _ = std::fs::remove_dir_all(&dir);
}

/// P3: a terminal re-entry writes exactly one resolve and never re-parks.
#[test]
fn terminal_reentry_writes_exactly_one_resolve() {
    let (dir, path) = temp_journal("terminal");
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let mut q = Quarantine::new();
    let f = parked(63);
    journal_reentry_outcome(&f, ReentryOutcome::Terminal, &mut q, Some(&mut journal));

    assert_eq!(q.len(), 0, "a terminal outcome never re-parks");
    assert_eq!(resolve_count(&path), 1, "exactly one resolve");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A hosted lane's journal lands under its namespace; the standalone
/// single-strategy knobs keeps the process-global root (strict parity).
#[test]
fn lane_root_scopes_the_quarantine_journal() {
    let lane = std::path::PathBuf::from("/var/state/backrun");
    let hosted = quarantine_journal_path(Some(&lane)).expect("lane journal path");
    assert_eq!(
        hosted,
        lane.join(crate::gap_quarantine_journal::JOURNAL_FILE_NAME)
    );

    let global = quarantine_journal_path(None).expect("global journal path");
    assert!(
        global.ends_with(crate::gap_quarantine_journal::JOURNAL_FILE_NAME),
        "the unscoped path keeps the process-global journal file"
    );
    assert!(
        !global.starts_with(&lane),
        "an unscoped driver never writes into a lane namespace"
    );
}

/// An operator file naming chain 8453 with per-chain endpoint tables for both
/// chains: the shape a multi-chain host's operator writes.
const SESSION_8453_FILE: &str = concat!(
    "[session]\n",
    "chain_id = 8453\n",
    "\n",
    "[nodes]\n",
    "http = { 1 = \"http://127.0.0.1:1\", 8453 = \"http://127.0.0.1:8453\" }\n",
    "ws = { 1 = \"ws://127.0.0.1:1\", 8453 = \"ws://127.0.0.1:8453\" }\n",
);

/// The same endpoint tables with no named chain: endpoints without a chain are
/// unselectable, because the tables are keyed by chain id.
const NO_CHAIN_FILE: &str = concat!(
    "[nodes]\n",
    "http = { 1 = \"http://127.0.0.1:1\", 8453 = \"http://127.0.0.1:8453\" }\n",
    "ws = { 1 = \"ws://127.0.0.1:1\", 8453 = \"ws://127.0.0.1:8453\" }\n",
);

/// The chain-1-only endpoint table: a host whose operator file still names
/// mainnet while its database was built for another chain.
const CHAIN_1_ENDPOINTS_FILE: &str = concat!(
    "[session]\n",
    "chain_id = 8453\n",
    "\n",
    "[nodes]\n",
    "http = { 1 = \"http://127.0.0.1:1\" }\n",
    "ws = { 1 = \"ws://127.0.0.1:1\" }\n",
);

/// The operator file loaded over no environment, so a test reads exactly the
/// layers it wrote.
fn layers_from(dir: &std::path::Path, body: &str) -> degenbot_config::LoadedConfig {
    let path = dir.join("config.toml");
    std::fs::write(&path, body).expect("write the operator file");
    degenbot_config::BotConfigLoader::new()
        .without_env()
        .with_config_path(&path)
        .load()
        .expect("the operator file loads")
}

/// The node join names the chain it resolved for, and the head feed reads that
/// same value: a 8453 session reaches the 8453 endpoints of both scopes while
/// the chain-1 entries sit right beside them in the same file.
#[tokio::test]

async fn the_node_join_names_the_session_chain_it_resolved() {
    let dir = tempfile::tempdir().expect("temp dir");

    let loaded = layers_from(dir.path(), SESSION_8453_FILE);

    let join =
        super::resolve_backrun_node_join(&loaded, None, Arc::new(RecordingCapability::default()))
            .await
            .expect("the 8453 join resolves");

    assert_eq!(join.chain_id, 8453, "the join carries its resolved chain");

    // The request scope takes the most preferred feed-capable transport it

    // accepts, which here is the ws entry; what the assertion pins is that it

    // is chain 8453's entry and not the chain-1 row beside it in the same table.

    assert_eq!(
        join.rpc_url, "ws://127.0.0.1:8453",
        "the request scope reads the 8453 entry"
    );

    assert_eq!(
        crate::backrun_driver::driver_boot::head_ws_url(&loaded, join.chain_id).as_deref(),
        Some("ws://127.0.0.1:8453"),
        "the head feed reads the same chain's subscription entry"
    );
}

/// An explicit argument outranks the file layer, and the endpoints are then
/// read from the chain that argument named.
#[tokio::test]

async fn an_explicit_chain_argument_outranks_the_file_layer() {
    let dir = tempfile::tempdir().expect("temp dir");

    let loaded = layers_from(dir.path(), SESSION_8453_FILE);

    let join = super::resolve_backrun_node_join(
        &loaded,
        Some("1"),
        Arc::new(RecordingCapability::default()),
    )
    .await
    .expect("the chain-1 join resolves");

    assert_eq!(join.chain_id, 1);

    assert_eq!(
        join.rpc_url, "ws://127.0.0.1:1",
        "the override names the chain read from the table"
    );
}

/// A boot that cannot name its chain is a typed refusal. There is no
/// mainnet-shaped default to fall into, because every downstream artifact (the
/// endpoint tables, the connector index) is keyed by chain.
#[tokio::test]

async fn a_boot_that_cannot_name_its_chain_refuses() {
    let dir = tempfile::tempdir().expect("temp dir");

    let loaded = layers_from(dir.path(), NO_CHAIN_FILE);

    let error =
        super::resolve_backrun_node_join(&loaded, None, Arc::new(RecordingCapability::default()))
            .await
            .err()
            .expect("an unnamed chain never yields a join");

    assert!(
        matches!(error, super::BackrunBootError::SessionChain(_)),
        "the refusal is the chain, not the endpoint: {error:?}"
    );
}

/// A named chain with no endpoint for it is a refusal whose message names the
/// chain the boot needed — not a quiet fall-through to another chain's entry.
#[tokio::test]

async fn an_unresolved_endpoint_names_the_chain_the_boot_needed() {
    let dir = tempfile::tempdir().expect("temp dir");

    let loaded = layers_from(dir.path(), CHAIN_1_ENDPOINTS_FILE);

    let error =
        super::resolve_backrun_node_join(&loaded, None, Arc::new(RecordingCapability::default()))
            .await
            .err()
            .expect("8453 has no endpoint in a chain-1-only file");

    let message = error.to_string();

    assert!(
        message.contains("8453"),
        "the message names the chain it needed: {message}"
    );

    assert!(
        !message.contains("127.0.0.1:1"),
        "the refusal never points at another chain's endpoint: {message}"
    );
}

/// A Base session over a Base-built database loads the Base rows: the registry
/// the driver runs on is the configured chain's, at the same shape the
/// mainnet boot produced.
#[tokio::test]

async fn a_configured_chain_loads_its_own_connector_rows() {
    use alloy::primitives::address;

    const USDC_WETH_V2: alloy::primitives::Address =
        address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");

    const USDC: alloy::primitives::Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");

    const WETH: alloy::primitives::Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

    let dir = tempfile::tempdir().expect("temp dir");

    let db_path = dir.path().join("connectors.db");

    seed_v2_connector(&db_path, 8453, USDC_WETH_V2, USDC, WETH);

    let resources = super::resolve_backrun_boot(
        Arc::new(degenbot_config::BotConfig::default()),
        db_path,
        join_for_chain(8453).await,
    )
    .await;

    assert!(
        resources.boot_error().is_none(),
        "a database built for the configured chain is not a refusal: {:?}",
        resources.boot_error()
    );

    let strategy = resources.strategy_boot(super::BackrunEcosystem::Txpool);

    assert!(
        strategy.registry().is_registered_pool(&USDC_WETH_V2),
        "the 8453 row is the row the driver serves"
    );

    assert_eq!(strategy.registry().registered_pool_count(), 1);
}

/// The defect this fixes: a 8453 session pointed at a database that holds only
/// chain-1 rows must refuse and NAME 8453. Reading chain 1's rows here is a
/// silent, well-formed, wrong-chain boot.
#[tokio::test]

async fn a_foreign_chain_database_refuses_and_names_the_chain() {
    use alloy::primitives::address;

    const USDC_WETH_V2: alloy::primitives::Address =
        address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");

    const USDC: alloy::primitives::Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");

    const WETH: alloy::primitives::Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

    let dir = tempfile::tempdir().expect("temp dir");

    let db_path = dir.path().join("connectors.db");

    seed_v2_connector(&db_path, 1, USDC_WETH_V2, USDC, WETH);

    let resources = super::resolve_backrun_boot(
        Arc::new(degenbot_config::BotConfig::default()),
        db_path,
        join_for_chain(8453).await,
    )
    .await;

    let error = resources
        .boot_error()
        .expect("chain-1 rows are not a Base session's connector index")
        .clone();

    let message = error.to_string();

    assert!(
        message.contains("8453"),
        "the refusal names the chain it needed: {message}"
    );

    assert!(
        matches!(error, super::BackrunBootError::ConnectorIndex { .. }),
        "an empty index for the configured chain is the refusal: {error:?}"
    );

    let strategy = resources.strategy_boot(super::BackrunEcosystem::Txpool);

    assert!(
        !strategy.registry().is_registered_pool(&USDC_WETH_V2),
        "another chain's rows never enter the registry"
    );

    assert_eq!(strategy.registry().registered_pool_count(), 0);
}

/// A provider that dials nothing, over a join that names `chain_id`.
async fn join_for_chain(chain_id: u64) -> super::BackrunNodeJoin {
    super::BackrunNodeJoin {
        rpc_url: "http://127.0.0.1:1".to_string(),

        provider: Arc::new(
            AlloyProvider::new("http://127.0.0.1:1", DEFAULT_MAX_RETRIES)
                .await
                .expect("provider builds without a node"),
        ),

        chain_id,
        capability: Arc::new(super::AnyRequestTransport),
    }
}

/// One canonical USDC/WETH V2 connector under the exchange that chain's rows
/// name, written through the same schema the discovery writer uses.
fn seed_v2_connector(
    db_path: &std::path::Path,
    chain_id: i64,
    pool: alloy::primitives::Address,
    token0: alloy::primitives::Address,
    token1: alloy::primitives::Address,
) {
    let (db, _) = degenbot_db::DegenbotDb::open_for_writes(db_path).expect("seed db");

    db.lock()

        .execute("INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory, deployer) VALUES (1, ?1, 'uniswap_v2', 1, NULL, '0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f', NULL)", (chain_id,))
        .expect("seed exchange");

    db.upsert_v2_pools(
        chain_id,
        "uniswap_v2",
        1,
        10_000,
        &[degenbot_db::V2PoolRowInput {
            address: pool,
            token0_address: token0,
            token1_address: token1,
            fee_token0: 300,
            fee_token1: 300,
            stable: None,
        }],
    )
    .expect("seed v2 connector");
}

/// A node that answers `eth_chainId` with `served_chain_id` and nothing else,
/// so a join over it exercises the boot's chain binding without a live node.
fn join_over_node(chain_id: u64, served_chain_id: u64) -> super::BackrunNodeJoin {
    use alloy::network::Ethereum;
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};

    let asserter = Asserter::new();
    for _ in 0..2 {
        asserter.push_success(&format!("0x{served_chain_id:x}"));
    }
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    let inner = ProviderBuilder::new().connect_client(client).erased();
    super::BackrunNodeJoin {
        rpc_url: format!("http://node.local:{served_chain_id}"),
        provider: Arc::new(AlloyProvider::from_provider(
            Arc::new(inner) as Arc<dyn Provider<Ethereum>>
        )),
        chain_id,
        capability: Arc::new(super::AnyRequestTransport),
    }
}

/// A boot whose endpoint serves a different chain than the join named is a
/// typed refusal that names BOTH chain ids. The boot compares against
/// `join.chain_id` — the chain it already resolved — so the guard and the boot
/// can never disagree about which chain this session is.
#[tokio::test]
async fn a_node_serving_another_chain_refuses_the_boot_and_names_both() {
    let dir = tempfile::tempdir().expect("temp dir");

    let resources = super::resolve_backrun_boot(
        Arc::new(degenbot_config::BotConfig::default()),
        dir.path().join("connectors.db"),
        join_over_node(1, 8453),
    )
    .await;

    let error = resources
        .boot_error()
        .expect("a node serving another chain never boots")
        .clone();

    assert!(
        matches!(error, super::BackrunBootError::ChainMismatch { .. }),
        "a wrong-chain endpoint is the chain refusal, not a connector-index one: {error:?}"
    );

    let message = error.to_string();
    assert!(
        message.contains('1') && message.contains("8453"),
        "the refusal names the resolved chain and the served one: {message}"
    );

    let strategy = resources.strategy_boot(super::BackrunEcosystem::Txpool);
    assert_eq!(
        strategy.registry().registered_pool_count(),
        0,
        "a refused boot opens no lane"
    );
}

/// The matching node boots: the guard refuses a disagreement, not a working
/// endpoint.
#[tokio::test]
async fn a_node_serving_the_resolved_chain_boots() {
    let dir = tempfile::tempdir().expect("temp dir");

    let resources = super::resolve_backrun_boot(
        Arc::new(degenbot_config::BotConfig::default()),
        dir.path().join("connectors.db"),
        join_over_node(8453, 8453),
    )
    .await;

    assert!(
        resources.boot_error().is_none(),
        "an endpoint that serves the boot's chain is not a refusal: {:?}",
        resources.boot_error()
    );
}

// ── The injected node capability ─────────────────────────────────────────
//
// The boot resolves WHICH endpoint (chain + scope); the caller injects WHO
// dials it. These tests read both halves: what the scope chose, and whether
// the consumer that asked for it can actually reach a socket.

/// A dialer that answers every endpoint with a transport it never opens, and
/// records what it was asked for. A resolution test reads the endpoint the
/// scope chose without a node behind it.
#[derive(Default)]
struct RecordingCapability {
    asked: std::sync::Mutex<Vec<String>>,
}

impl RecordingCapability {
    fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .expect("the recording lock is not poisoned")
            .clone()
    }
}

impl super::node_capability::NodeCapability for RecordingCapability {
    fn request_provider(
        &self,
        endpoint: &str,
        _chain_id: u64,
    ) -> super::node_capability::RequestProviderFuture<'_> {
        self.asked
            .lock()
            .expect("the recording lock is not poisoned")
            .push(endpoint.to_string());
        Box::pin(async move {
            use alloy::network::Ethereum;
            use alloy::providers::{Provider, ProviderBuilder};
            use alloy::rpc::client::ClientBuilder;
            use alloy::transports::mock::{Asserter, MockTransport};

            let client =
                ClientBuilder::default().transport(MockTransport::new(Asserter::new()), true);
            let inner = ProviderBuilder::new().connect_client(client).erased();
            Ok(Arc::new(AlloyProvider::from_provider(
                Arc::new(inner) as Arc<dyn Provider<Ethereum>>
            )))
        })
    }
}

/// An IPC node on a real unix socket: it answers `eth_chainId` and
/// `eth_callMany` and nothing else. A mock transport would prove nothing
/// here, because the transport is the subject.
///
/// The client frames its requests as bare JSON values with no delimiter, so
/// the responder parses each buffer as soon as it holds one complete value
/// rather than waiting for a line that never comes.
fn ipc_node(dir: &std::path::Path, chain_id: u64) -> std::path::PathBuf {
    use std::io::{Read, Write};

    let path = dir.join("node.ipc");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the IPC socket");
    let served = format!("0x{chain_id:x}");
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let served = served.clone();
            std::thread::spawn(move || {
                let mut pending = String::new();
                let mut chunk = [0_u8; 512];
                loop {
                    let Ok(read) = stream.read(&mut chunk) else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    pending.push_str(&String::from_utf8_lossy(&chunk[..read]));
                    loop {
                        let Ok(request) = serde_json::from_str::<serde_json::Value>(pending.trim())
                        else {
                            break;
                        };
                        pending.clear();
                        let id = request
                            .get("id")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let method = request
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let result = match method.as_str() {
                            "eth_chainId" => serde_json::json!(served),
                            "eth_callMany" => {
                                serde_json::json!([{"success": true, "returnData": "0x"}])
                            }
                            _ => serde_json::Value::Null,
                        };
                        let response =
                            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                        if write!(stream, "{response}").is_err() || stream.flush().is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    path
}

/// The `eth_callMany` request the driver's sim gate sends, built by the same
/// composer the gate uses.
fn eth_call_many_probe() -> serde_json::Value {
    degenbot_submission::bundle::eth_call_many_bundle_sim_params(
        &[serde_json::json!({
            "to": "0x0000000000000000000000000000000000000000",
            "data": "0x",
        })],
        "latest",
    )
}

/// What one socket endpoint produces for the two consumers that need it: the
/// chain the join's provider read back over the socket, and the sim
/// `eth_callMany` answer served by the client the same injected capability
/// built.
async fn join_and_sim_over(dir: &std::path::Path, endpoint: &str) -> (u64, serde_json::Value) {
    let body =
        format!("[session]\nchain_id = 31337\n\n[nodes]\nipc = {{ 31337 = \"{endpoint}\" }}\n");
    let loaded = layers_from(dir, &body);

    let join =
        super::resolve_backrun_node_join(&loaded, None, Arc::new(super::AnyRequestTransport))
            .await
            .expect("a socket path resolves a node join");

    assert_eq!(
        join.chain_id, 31337,
        "the join carries the chain its endpoint was resolved for"
    );
    assert_eq!(
        join.rpc_url, endpoint,
        "the join reports the endpoint as written"
    );

    let served = join
        .provider
        .get_chain_id()
        .await
        .expect("the join's provider answers which chain the node serves");

    let sim = join
        .capability
        .request_provider(&join.rpc_url, join.chain_id)
        .await
        .expect("the sim endpoint dials the same socket the join did");

    let result = sim
        .make_request("eth_callMany", eth_call_many_probe())
        .await
        .expect("the socket serves eth_callMany");

    (served, result)
}

/// The `ipc://` spelling of a local node reaches the driver: the node join's
/// provider reads the node's chain over the socket, and the sim client built
/// from the same endpoint serves the frame-sim request.
#[tokio::test]
async fn an_ipc_scheme_join_and_its_sim_client_reach_the_local_node() {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = ipc_node(dir.path(), 31337);

    let (served, result) =
        join_and_sim_over(dir.path(), &format!("ipc://{}", socket.display())).await;

    assert_eq!(
        served, 31337,
        "the join's provider reached the node over the socket"
    );
    assert_eq!(
        result[0]["success"],
        serde_json::json!(true),
        "the sim client served eth_callMany over the socket: {result}"
    );
}

/// A bare socket path is the same transport: an operator who wrote the path
/// without the scheme gets the same join and the same sim client.
#[tokio::test]
async fn a_bare_socket_path_join_and_its_sim_client_reach_the_local_node() {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = ipc_node(dir.path(), 31337);

    let (served, result) = join_and_sim_over(dir.path(), &socket.display().to_string()).await;

    assert_eq!(
        served, 31337,
        "the join's provider reached the node over the socket"
    );
    assert_eq!(
        result[0]["success"],
        serde_json::json!(true),
        "the sim client served eth_callMany over the socket: {result}"
    );
}

/// The boot hands the capability exactly the endpoint the request scope chose
/// — the transport is the caller's decision, the endpoint is the boot's.
#[tokio::test]
async fn the_boot_dials_exactly_the_endpoint_the_request_scope_chose() {
    let dir = tempfile::tempdir().expect("temp dir");
    let loaded = layers_from(dir.path(), SESSION_8453_FILE);
    let capability = Arc::new(RecordingCapability::default());

    let join = super::resolve_backrun_node_join(&loaded, None, capability.clone())
        .await
        .expect("the 8453 join resolves");

    assert_eq!(
        capability.asked(),
        vec!["ws://127.0.0.1:8453".to_string()],
        "the capability is asked for the resolved endpoint, and nothing else"
    );
    assert_eq!(join.rpc_url, "ws://127.0.0.1:8453");
}

/// A file whose only entry for the session chain is the http table. A request
/// consumer can use it; a feed cannot.
const HTTP_ONLY_FEED_FILE: &str = concat!(
    "[session]\n",
    "chain_id = 8453\n",
    "\n",
    "[nodes]\n",
    "http = { 8453 = \"http://127.0.0.1:8453\" }\n",
);

/// The head feed reads the SUBSCRIPTION scope, and the operator's http entry
/// is not one: the request scope accepts the same file's http entry, so the
/// contrast is the capability, not a missing endpoint.
#[test]
fn the_head_feed_never_downgrades_to_the_http_entry() {
    let dir = tempfile::tempdir().expect("temp dir");
    let loaded = layers_from(dir.path(), HTTP_ONLY_FEED_FILE);

    assert_eq!(
        degenbot_config::resolve_node_request_uri(
            &loaded,
            8453,
            &degenbot_config::NodeOverrides::new(),
        )
        .expect("a request consumer accepts the http entry")
        .value,
        "http://127.0.0.1:8453",
        "the file does serve requests"
    );

    assert_eq!(
        super::driver_boot::head_ws_url(&loaded, 8453),
        None,
        "an http entry never becomes the head feed; the driver polls instead"
    );

    let error = degenbot_config::resolve_node_subscription_uri(
        &loaded,
        8453,
        &degenbot_config::NodeOverrides::new(),
    )
    .expect_err("an http endpoint is not a feed");

    let message = error.to_string();
    assert!(
        message.contains("ipc") && message.contains("ws"),
        "the refusal names the two transports a feed accepts: {message}"
    );
}

/// An `ipc` entry the config layer accepts and the transport classifies as a
/// socket, but whose path has no listener: an absolute path under the temp
/// directory. The config table is a naming layer and the transport is a dialing
/// one; the dial fails, and the boot surfaces that failure as a typed refusal
/// naming the endpoint rather than a panic.
#[tokio::test]
async fn an_undialable_join_endpoint_is_a_typed_refusal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let endpoint = dir.path().join("node.ipc");
    let body = format!(
        "[session]\nchain_id = 31337\n\n[nodes]\nipc = {{ 31337 = \"{}\" }}\n",
        endpoint.display()
    );
    let loaded = layers_from(dir.path(), &body);

    let error =
        super::resolve_backrun_node_join(&loaded, None, Arc::new(super::AnyRequestTransport))
            .await
            .err()
            .expect("a socket path with no listener never yields a join");

    assert!(
        matches!(error, super::BackrunBootError::NodeJoin(_)),
        "the refusal is the endpoint: {error:?}"
    );
    let rendered = endpoint.display().to_string();
    assert!(
        error.to_string().contains(&rendered),
        "the refusal names the endpoint it could not dial: {error}"
    );
}

#[tokio::test]
async fn an_undialable_sim_endpoint_is_a_typed_refusal() {
    let error = super::AnyRequestTransport
        .request_provider("sim.local/fast", 1)
        .await
        .err()
        .expect("a schemeless sim endpoint never yields a client");

    assert!(
        error.contains("sim.local/fast"),
        "the refusal names the sim endpoint it could not dial: {error}"
    );
}
