//! Driver-assembly tests: the boot registry parity across standalone and
//! hosted boot, the loop lifecycle table, the bid-economics policy pins, and
//! the quarantine rescue-routing folds.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use crate::frame_pipeline::predecessor_observe_reason;
use crate::gap_quarantine::{ParkedFrame, Quarantine, QuarantineDecision};
use crate::gap_quarantine_journal::{ParkRecord, QuarantineJournal};
use alloy::primitives::{Address, B256, U256};
use degenbot_bot::sidecar::SidecarConfig;
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
use std::sync::Arc;

use super::driver_loop::{
    decode_predecessor, journal_reentry_outcome, nonce_lane_evidence, outcome_for,
    pool_known_gap_for_tick, predecessor_hash, quarantine_journal_path, record_gap_park,
    reentry_outcome, route_failed_hydration, route_unhydratable_hydration, run_frame, DriverHandle,
    FrameOutcome, GapParkMemo, LoopDecline, LoopPhase, LoopShared, ReentryOutcome, FEED_PREFIX,
};
use super::driver_policy::{bid_submission_target, build_broadcast_relays};

/// The boot registry has ONE construction seam: the standalone sidecar's
/// boot and the hosted boot both hand it a DB path and the lane's node
/// join, and both read the same populated snapshot back. The fixture seeds
/// the canonical USDC/WETH V2 connector plus a V3 connector, pinning the
/// order -- the V2 scan, then its V3 additions, then the ranker and the
/// frozen registry.
#[tokio::test]
async fn the_boot_registry_resolver_populates_both_boot_paths() {
    use alloy::primitives::address;
    use degenbot_db::{V2PoolRowInput, V3PoolRowInput};

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
    let config = degenbot_config::BotConfig::default();

    let (registry, _db) = super::resolve_backrun_registry(&config, &db_path, &provider)
        .await
        .expect("the resolver yields a populated registry");
    assert!(
        registry.is_registered_pool(&USDC_WETH_V2),
        "V2 connector landed"
    );
    assert!(
        registry.is_registered_pool(&USDC_WETH_V3),
        "V3 additions landed"
    );
    assert_eq!(
        registry.registered_pool_count(),
        2,
        "both connector families landed"
    );

    let hosted = super::resolve_backrun_host_registry(&config, &db_path, &provider).await;
    assert_eq!(
        hosted.registered_pool_count(),
        registry.registered_pool_count(),
        "the hosted boot reaches the same resolver"
    );
    assert!(hosted.is_registered_pool(&USDC_WETH_V2));
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
    let mut cfg = SidecarConfig::from_config(&degenbot_config::BotConfig::default(), String::new());
    assert!(
        build_broadcast_relays(&cfg, &provider).await.is_empty(),
        "an unset mevblocker_url must leave the read-provider-only list"
    );
    cfg.mevblocker_url = Some("http://private.local:8545".to_string());
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

#[test]
fn mevblocker_url_does_not_alter_the_target() {
    // Adjudicated policy: augment, never replace. The key only changes the
    // `extra_broadcast` list content; the target-selection region computes
    // the same target with and without it set.
    use crate::submit::SubmissionTarget;

    let mut cfg = SidecarConfig::from_config(&degenbot_config::BotConfig::default(), String::new());
    let hash = alloy::primitives::B256::repeat_byte(0x11);
    let without = bid_submission_target(&cfg, hash, 21_000_001);

    cfg.mevblocker_url = Some("http://private.local:8545".to_string());
    let with = bid_submission_target(&cfg, hash, 21_000_001);

    let to_bundle = |target: SubmissionTarget| match target {
        SubmissionTarget::Bundle(b) => Some(b),
        SubmissionTarget::Public => None,
    };
    let a = to_bundle(without).expect("target must be the Bundle arm regardless of mevblocker_url");
    let b = to_bundle(with).expect("target must be the Bundle arm regardless of mevblocker_url");
    assert_eq!(a.stream_url, b.stream_url);
    assert_eq!(a.target_tx_hash, b.target_tx_hash);
    assert_eq!(a.block_number, b.block_number);
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
    assert!(nonce_lane_evidence::<()>(Ok(U256::MAX)).is_none());
    assert_eq!(
        nonce_lane_evidence::<()>(Ok(U256::from(29_764u64))),
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
        "degenbot-sidecar-reentry-{}-{tag}",
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
/// single-strategy sidecar keeps the process-global root (strict parity).
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
