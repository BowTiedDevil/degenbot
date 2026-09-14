//! Live submission seam — parity-ledger row 18 (ergo `L4E7RI`, Gap G4).
//!
//! Mirrors `src/degenbot/runner/_dispatch.py::_submit_batch_records`: the
//! driver applies the mutual-exclusivity guard, the `dry_run` guard, and the
//! `inject_code` guard BEFORE reaching the signing path, then delegates each
//! surviving candidate to the umbrella's
//! [`degenbot::submission::dispatch_and_submit`] (EIP-1559 sign + broadcast +
//! receipt monitor).
//!
//! The [`SubmissionSeam`] trait is the driver's signing boundary. It exists so
//! the **dry-run path provably never signs**: `submit_batch` short-circuits on
//! `dry_run`/`inject_code` and never invokes the seam, so a recording/asserting
//! seam observes zero calls (see the `dry_run_never_reaches_the_seam` test).
//!
//! RPC-gated: [`LiveSubmissionSeam`] needs an `AlloyProvider` + `TxSigner` +
//! `ReceiptProbe`; only the live arm (`SMOKE_RPC_URL` + `--live`) constructs it.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use degenbot::rpc::provider::AlloyProvider;
use degenbot::submission::{
    dispatch_and_submit, monitor_pending_transaction, Dispatcher, MonitorOutcome, PoolKey,
    ReceiptProbe, SkipReason, SubmitCandidate, SubmitRecord, SubmittedTx, TxSigner,
};

/// The driver's typed submit decision (the RSP-8 diff surface).
#[derive(Clone, Debug, PartialEq)]
pub enum SubmitDecision {
    /// The tx was broadcast.
    Submitted {
        /// The path id.
        path_id: u64,
        /// The broadcast tx hash.
        tx_hash: alloy::primitives::B256,
        /// The claimed nonce.
        nonce: u64,
    },
    /// The candidate was skipped (the typed core reason).
    Skipped {
        /// The path id.
        path_id: u64,
        /// The typed skip reason.
        reason: SkipReason,
    },
}

impl SubmitDecision {
    /// The stable machine label (the RSP-8 diff column).
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Submitted { .. } => "submitted",
            Self::Skipped { reason, .. } => match reason {
                SkipReason::PoolsClaimed => "skipped-pools-claimed",
                SkipReason::DryRun => "skipped-dry-run",
                SkipReason::InjectCode => "skipped-inject-code",
                SkipReason::BroadcastFailed(_) => "skipped-broadcast-failed",
            },
        }
    }

    /// The candidate's path id.
    #[must_use]
    pub const fn path_id(&self) -> u64 {
        match self {
            Self::Submitted { path_id, .. } | Self::Skipped { path_id, .. } => *path_id,
        }
    }
}

/// The boxed future the signing seam returns.
pub type SeamFuture<'a> = Pin<Box<dyn Future<Output = Result<SubmitDecision, String>> + Send + 'a>>;

/// The driver's signing boundary.
///
/// The dry-run/inject guards live in [`submit_batch`] — the seam is only
/// reached for a candidate that must be signed and broadcast.
pub trait SubmissionSeam: Send + Sync {
    /// Sign + broadcast one candidate (the live impl delegates to
    /// `dispatch_and_submit`).
    fn dispatch_one(
        &self,
        candidate: SubmitCandidate,
        operator_nonce: u64,
        current_block: u64,
    ) -> SeamFuture<'_>;
}

/// Submit a batch with the Python `_submit_batch_records` guard order.
///
/// 1. Sort best-first by net profit (the core re-asserts this; the driver
///    mirrors the seam contract).
/// 2. Mutual-exclusivity skip ([`Dispatcher::is_path_blocked`]).
/// 3. `dry_run` skip — pools are still committed (mutual exclusivity holds)
///    and the seam is NOT called (never signs).
/// 4. `inject_code` skip — the injected contract is absent on-chain; the seam
///    is NOT called.
/// 5. Otherwise delegate to the seam with the operator nonce.
///
/// # Errors
///
/// Returns the seam's hard signer error detail (a broadcast failure is a typed
/// `Skipped` record from the seam, not an `Err`).
pub async fn submit_batch(
    mut candidates: Vec<SubmitCandidate>,
    dispatcher: &Mutex<Dispatcher>,
    seam: &dyn SubmissionSeam,
    operator_nonce: u64,
    current_block: u64,
    dry_run: bool,
    inject_code: bool,
) -> Result<Vec<SubmitDecision>, String> {
    candidates.sort_by_key(|c| std::cmp::Reverse(c.net_profit));
    let mut committed_pools: HashSet<PoolKey> = HashSet::new();
    let mut decisions = Vec::with_capacity(candidates.len());
    let mut nonce = operator_nonce;
    for candidate in candidates {
        let path_pools = candidate.path_pools.clone();
        let blocked = {
            let d = dispatcher.lock().map_err(|e| e.to_string())?;
            d.is_path_blocked(&path_pools, &committed_pools)
        };
        if blocked {
            decisions.push(SubmitDecision::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::PoolsClaimed,
            });
            continue;
        }
        if dry_run {
            // Dry-run is a MODE, not a signing path: commit the pools so
            // mutual exclusivity is respected, then skip WITHOUT touching the
            // seam.
            committed_pools.extend(path_pools);
            decisions.push(SubmitDecision::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::DryRun,
            });
            continue;
        }
        if inject_code {
            committed_pools.extend(path_pools);
            decisions.push(SubmitDecision::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::InjectCode,
            });
            continue;
        }
        let decision = seam.dispatch_one(candidate, nonce, current_block).await?;
        nonce = nonce.saturating_add(1);
        decisions.push(decision);
    }
    Ok(decisions)
}

/// The live signing seam: delegates to `degenbot::submission::dispatch_and_submit`.
pub struct LiveSubmissionSeam<'a> {
    /// The shared dispatcher (nonce + pool claims + monitor tasks).
    pub dispatcher: &'a Arc<Mutex<Dispatcher>>,
    /// The RPC provider (broadcast + access-list recompute).
    pub provider: &'a AlloyProvider,
    /// The operator signer (holds the key; signs EIP-1559).
    pub signer: &'a TxSigner,
    /// The receipt probe the spawned monitor polls.
    pub probe: Arc<dyn ReceiptProbe + Send + Sync>,
    /// Whether executor code is injected (unsafe to broadcast).
    pub inject_code: bool,
}

impl SubmissionSeam for LiveSubmissionSeam<'_> {
    fn dispatch_one(
        &self,
        candidate: SubmitCandidate,
        operator_nonce: u64,
        current_block: u64,
    ) -> SeamFuture<'_> {
        Box::pin(async move {
            let outcome = dispatch_and_submit(
                vec![candidate],
                self.dispatcher,
                self.provider,
                self.signer,
                Arc::clone(&self.probe),
                operator_nonce,
                current_block,
                false,
                self.inject_code,
            )
            .await
            .map_err(|e| e.to_string())?;
            map_single_record(outcome.records)
        })
    }
}

/// Monitor a submitted tx under the driver's configured nonce-expiry window
/// and release its nonce + pools on confirm/expiry.
///
/// Wraps the umbrella `monitor_pending_transaction` with
/// `blocks_before_nonce_expires` sourced from the driver config
/// (`BLOCKS_BEFORE_NONCE_EXPIRES` / `SettlementBotConfig::blocks_before_nonce_expires`).
///
/// # Errors
///
/// Returns the submission error detail (the receipt probe's `Err`).
pub async fn monitor_with_config(
    tx: SubmittedTx,
    probe: &(impl ReceiptProbe + ?Sized),
    dispatcher: &Mutex<Dispatcher>,
    blocks_before_nonce_expires: u64,
) -> Result<MonitorOutcome, String> {
    monitor_pending_transaction(tx, probe, dispatcher, blocks_before_nonce_expires)
        .await
        .map_err(|e| e.to_string())
}

/// Map the core's single-record submit outcome into the driver decision.
///
/// # Errors
///
/// Returns an error only if the core returned no records for a non-empty
/// single-candidate batch (a contract violation).
pub fn map_single_record(records: Vec<SubmitRecord>) -> Result<SubmitDecision, String> {
    match records.into_iter().next() {
        Some(SubmitRecord::Submitted {
            path_id,
            tx_hash,
            nonce,
        }) => Ok(SubmitDecision::Submitted {
            path_id,
            tx_hash,
            nonce,
        }),
        Some(SubmitRecord::Skipped { path_id, reason }) => {
            Ok(SubmitDecision::Skipped { path_id, reason })
        }
        None => Err("dispatch_and_submit returned no records".to_string()),
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use alloy::primitives::{Address, Bytes, B256, U256};

    use super::*;

    fn candidate(path_id: u64, net_profit: u64, pools: &[&str]) -> SubmitCandidate {
        SubmitCandidate {
            path_id,
            gross_profit: U256::from(net_profit + 1),
            net_profit: U256::from(net_profit),
            gas_used: 100_000,
            priority_fee: 1,
            base_fee_next: 1,
            execute_calldata: Bytes::new(),
            executor_address: Address::ZERO,
            access_list: None,
            path_pools: pools.iter().map(|p| PoolKey::new(*p)).collect(),
        }
    }

    #[derive(Default)]
    struct RecordingSeam {
        calls: AtomicUsize,
    }

    impl SubmissionSeam for RecordingSeam {
        fn dispatch_one(
            &self,
            candidate: SubmitCandidate,
            _operator_nonce: u64,
            _current_block: u64,
        ) -> SeamFuture<'_> {
            let calls = &self.calls;
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(SubmitDecision::Submitted {
                    path_id: candidate.path_id,
                    tx_hash: B256::ZERO,
                    nonce: 0,
                })
            })
        }
    }

    #[tokio::test]
    async fn dry_run_never_reaches_the_seam() {
        let seam = RecordingSeam::default();
        let dispatcher = Mutex::new(Dispatcher::for_block(100));
        let decisions = submit_batch(
            vec![candidate(1, 10, &["0xpool"]), candidate(2, 5, &["0xpool2"])],
            &dispatcher,
            &seam,
            0,
            100,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(seam.calls.load(Ordering::SeqCst), 0, "dry-run signed!");
        assert!(decisions.iter().all(|d| d.label() == "skipped-dry-run"));
        assert_eq!(decisions.len(), 2);
    }

    #[tokio::test]
    async fn live_path_reaches_the_seam_for_each_unblocked_candidate() {
        let seam = RecordingSeam::default();
        let dispatcher = Mutex::new(Dispatcher::for_block(100));
        let decisions = submit_batch(
            vec![candidate(1, 10, &["0xpool"]), candidate(2, 5, &["0xpool2"])],
            &dispatcher,
            &seam,
            7,
            100,
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(seam.calls.load(Ordering::SeqCst), 2);
        assert!(decisions.iter().all(|d| d.label() == "submitted"));
    }

    #[tokio::test]
    async fn inject_code_skips_without_signing() {
        let seam = RecordingSeam::default();
        let dispatcher = Mutex::new(Dispatcher::for_block(100));
        let decisions = submit_batch(
            vec![candidate(1, 10, &["0xpool"])],
            &dispatcher,
            &seam,
            0,
            100,
            false,
            true,
        )
        .await
        .unwrap();
        assert_eq!(seam.calls.load(Ordering::SeqCst), 0);
        assert_eq!(decisions[0].label(), "skipped-inject-code");
    }

    #[tokio::test]
    async fn shared_pool_blocks_the_second_candidate() {
        let seam = RecordingSeam::default();
        let dispatcher = Mutex::new(Dispatcher::for_block(100));
        // In live mode the first candidate claims the pool via the seam only
        // if the seam reserves it; here the driver's pool-block guard is
        // exercised against the dispatcher's pending set. Seed a pending pool.
        {
            let mut d = dispatcher.lock().unwrap();
            d.reserve_pools([PoolKey::new("0xshared")]);
        }
        let decisions = submit_batch(
            vec![candidate(1, 10, &["0xshared"])],
            &dispatcher,
            &seam,
            0,
            100,
            false,
            false,
        )
        .await
        .unwrap();
        assert_eq!(seam.calls.load(Ordering::SeqCst), 0);
        assert_eq!(decisions[0].label(), "skipped-pools-claimed");
    }
}
