//! # `degenbot-submission` — the transaction-submission mechanism
//!
//! A pyo3-free core crate owning the path from a profitable candidate to a
//! confirmed nonce for the MEV bot: it holds the operator private key once via
//! [`alloy::signers::local::PrivateKeySigner`], finalizes the EIP-1559 fee
//! fields, signs type-2 `TxEnvelope` transactions, dispatches them under a
//! nonce lease, broadcasts them (raw fan-out, `MEVBlocker` bundle, or builder
//! relay), monitors the receipts, and reconciles the submission ledger as the
//! head advances.
//!
//! # What lives here
//!
//! - [`signer::TxSigner`] — `pub struct TxSigner { signer: PrivateKeySigner,
//!   chain_id: u64 }` holding the operator key ONCE (constructed from
//!   Python-provided key bytes/hex via the `PyO3` wrapper — the key never
//!   round-trips back to Python per-tx). [`signer::TxSigner::sign_eip1559`]
//!   builds the type-2 `TxEnvelope` from [`params::TxParams`] + signs it
//!   synchronously (ECDSA secp256k1, RFC 6979 deterministic nonces).
//! - [`fee::finalize_fees`] — `maxFeePerGas = int(1.5 * base_fee_next) +
//!   priority_fee`, `maxPriorityFeePerGas = priority_fee` (the Python oracle's
//!   inline fee computation, including the float→int truncate boundary —
//!   documented on [`fee`]).
//! - [`params::TxParams`] — the EIP-1559 field set the caller assembles, the
//!   signer finalizes fees onto, and `TxSigner` signs (`to`/`data`=`execute()`
//!   calldata/`gas`/`nonce`/`value=0`/`accessList` + the two fee fields).
//!   `chain_id` is NOT here — it lives on `TxSigner` (the signer owns the
//!   replay-protection context; no duplicated-state smell).
//! - [`delivery`] — the delivery-failure classifier: the one
//!   `Release`/`Hold`/`Promote` policy the three delivery arms share, from the
//!   provider's own error taxonomy.
//! - [`submit::dispatch_and_submit`] — the claim → finalize → re-compute
//!   access list → sign → broadcast → monitor orchestration, over the
//!   version-agnostic [`SubmissionTarget`] (raw fan-out, `MEVBlocker` bundle,
//!   builder-relay bundle); [`SubmitCandidate`] is its input shape,
//!   [`SubmitOutcome`]/[`SubmitRecord`]/[`SkipReason`] its result.
//! - [`dispatcher::Dispatcher`] — nonce and pool-key claim arbitration across
//!   the monitor tasks, plus per-path [`PathSuppression`].
//! - [`monitor::monitor_pending_transaction`] — receipt polling to a
//!   [`MonitorOutcome`] (`BLOCKS_BEFORE_NONCE_EXPIRES` bounds the wait).
//! - [`submission_ledger::SubmissionLedger`] — the per-target record of
//!   signed/broadcast/delivery-unknown/confirmed submissions, with
//!   [`HeadPolicy`] declines and
//!   [`RepackageRequest`]/[`Notification`] outputs.
//! - [`head_reconciliation`] — replays the ledger's outstanding submissions
//!   against each new head.
//! - [`sim_pipeline`] — the sim→submit seam ([`SimSubmitPipeline`],
//!   [`SimLeaf`]/[`SubmitLeaf`]).
//! - [`relay`] / [`bundle`] — the relay fan-out and the `MEVBlocker`
//!   `eth_sendBundle` wire.
//!
//! # Standalone-core constraint (ADR-005)
//!
//! A standalone Rust consumer `cargo add degenbot` reaches this crate via the
//! umbrella `pub use degenbot_submission;` and can sign + broadcast EIP-1559
//! transactions with **zero Python, zero `pyo3`** (verified by
//! `just check-no-pyo3-in-cores`). Signing, dispatch, broadcast (through
//! `degenbot-rpc`'s typed provider or a relay), monitoring, and reconciliation
//! all live here; nothing in the crate reaches for a Python runtime.
//!
//! # Parity gate (`docs/architecture/parity-gate.md`)
//!
//! The parity oracle is `eth_account.Account.sign_transaction(transaction_dict=
//! tx_params, private_key=operator_private_key).raw_transaction`
//! (`examples/eth_backrun_v2_v3_v4_rust.py` L2623–L2640). Because both
//! `eth_account` and `alloy-signer-local` use RFC 6979 deterministic ECDSA
//! over secp256k1, a pinned key + `tx_params` produces **byte-for-byte
//! identical** raw signed `bytes` in Rust and Python — pinned in
//! [`signer::tests::signed_bytes_match_eth_account_oracle_byte_for_byte`]
//! against a canonical anvil-account-0 fixture (raw signed bytes captured
//! from `eth_account` and asserted equal to the Rust output). A round-trip
//! decode + signer-recovery proptest pins the field-level parity.
//!
//! The fee constants (`TARGET_PROFIT_RATIO` / `AGE_DECAY_CONSTANT` /
//! `MIN_PRIORITY_FEE_PERCENTILE` / `MAX_PRIORITY_FEE_PERCENTILE`) live in
//! `_compute_priority_fee` (Simulation) — NOT this crate's concern.
//! The `priority_fee` is CONSUMED off `tx_params` (no hard edge — the
//! Simulation leaf computes it; submission reads it). For parity-gate testing the fee
//! is a fixture value.
//!
//! # Non-goals
//!
//! - `next_base_fee` (consumed via `base_fee_next`).
//! - `_compute_priority_fee` (the `priority_fee` value) (consumed
//!   via `tx_params`; for the parity gate it is a fixture).
//! - Private-key CONFIG load — stays Python (the driver loads the key and
//!   hands it to the Rust signer ONCE).

pub mod bundle;
pub mod delivery;
pub mod dispatcher;
pub mod error;
pub mod fee;
pub mod head_reconciliation;
pub mod monitor;
pub mod params;
pub mod relay;
pub mod signer;
pub mod sim_pipeline;
pub mod submission_ledger;
pub mod submit;

// ---- Convenience top-level re-exports (mirrors the umbrella convention) ----

pub use error::{SubmissionError, SubmissionResult};
pub use fee::{compute_max_fee, finalize_fees, MAX_FEE_HEADROOM};
pub use params::TxParams;
pub use signer::TxSigner;

pub use dispatcher::{
    CommittedTx, Dispatcher, PathSuppression, PoolKey, BLOCK_TIMES_WINDOW, FEE_HISTORY_WINDOW,
    PATH_SUPPRESS_RETRY_INTERVAL, PATH_SUPPRESS_THRESHOLD,
};
pub use head_reconciliation::SweepOutcome;
pub use monitor::{
    monitor_pending_transaction, monitor_pending_transaction_default, MonitorOutcome, ReceiptProbe,
    SubmittedTx, BLOCKS_BEFORE_NONCE_EXPIRES,
};
pub use sim_pipeline::{
    PipelineFailure, SimFuture, SimLeaf, SimSubmitPipeline, SubmitFuture, SubmitLeaf,
};
pub use submission_ledger::{
    release_hold, replace_hold, HeadPolicy, HoldDecline, LedgerDecline, Notification,
    NotificationKind, PolicyAction, RepackageRequest, Replacement, ShadowPosture, SubmissionLedger,
    SubmissionRecord, SubmissionState, TargetId,
};
pub use submit::{
    dispatch_and_submit, fetch_fee_history, SkipReason, SubmissionTarget, SubmitCandidate,
    SubmitOutcome, SubmitRecord,
};

/// The configured durable-state root, or `None` when it cannot be resolved.
///
/// A hosted strategy scopes its lane artifacts under this root; the standalone
/// single-strategy boot ignores it and keeps its process-global paths.
#[must_use]
pub fn resolve_state_root() -> Option<std::path::PathBuf> {
    degenbot_runs::resolve_state_root().ok()
}

// Test-only span capture for the OTel tier-1 span tests. One global
// subscriber per test process: async tests cross tasks, where scoped
// `with_default` guards would be unsafe, so every test module in this
// crate shares the same capture
// via `span_capture::global()`.
#[cfg(test)]
pub(crate) mod span_capture {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex, OnceLock};
    use tracing::field::Visit;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

    type SpanEntry = (String, BTreeMap<String, String>);

    #[derive(Default)]
    pub struct SpanCapture {
        spans: Mutex<HashMap<tracing::span::Id, SpanEntry>>,
    }

    impl SpanCapture {
        /// All captured spans as (name, recorded fields).
        pub fn snapshot(&self) -> Vec<(String, BTreeMap<String, String>)> {
            let Ok(g) = self.spans.lock() else {
                return Vec::new();
            };
            g.values()
                .map(|entry| (entry.0.clone(), entry.1.clone()))
                .collect()
        }
    }

    pub struct CaptureLayer(pub Arc<SpanCapture>);

    struct FieldPusher<'a>(&'a mut BTreeMap<String, String>);

    impl Visit for FieldPusher<'_> {
        fn record_debug(&mut self, key: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.insert(key.name().to_string(), format!("{value:?}"));
        }
        fn record_str(&mut self, key: &tracing::field::Field, value: &str) {
            self.0.insert(key.name().to_string(), value.to_string());
        }
        fn record_i64(&mut self, key: &tracing::field::Field, value: i64) {
            self.0.insert(key.name().to_string(), value.to_string());
        }
        fn record_u64(&mut self, key: &tracing::field::Field, value: u64) {
            self.0.insert(key.name().to_string(), value.to_string());
        }
        fn record_bool(&mut self, key: &tracing::field::Field, value: bool) {
            self.0.insert(key.name().to_string(), value.to_string());
        }
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            _ctx: Context<'_, S>,
        ) {
            let name = attrs.metadata().name().to_string();
            let Ok(mut g) = self.0.spans.lock() else {
                return;
            };
            let entry = g
                .entry(id.clone())
                .or_insert_with(|| (name, BTreeMap::new()));
            attrs.values().record(&mut FieldPusher(&mut entry.1));
        }

        fn on_record(
            &self,
            id: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            _ctx: Context<'_, S>,
        ) {
            let Ok(mut g) = self.0.spans.lock() else {
                return;
            };
            if let Some(entry) = g.get_mut(id) {
                values.record(&mut FieldPusher(&mut entry.1));
            }
        }
    }

    /// The shared capture, installed as the process-global subscriber on
    /// first use (once per test binary; a second install is a no-op and the
    /// shared `Arc` keeps capturing regardless).
    #[must_use]
    pub fn global() -> Arc<SpanCapture> {
        static CAP: OnceLock<Arc<SpanCapture>> = OnceLock::new();
        let cap = CAP.get_or_init(|| Arc::new(SpanCapture::default())).clone();
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(CaptureLayer(cap.clone())),
        );
        cap
    }
}
