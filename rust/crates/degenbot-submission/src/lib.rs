//! # `degenbot-submission` — EIP-1559 transaction signing + fee finalization
//!
//! A pyo3-free core crate owning the **transaction-submission mechanism** for
//! the MEV bot: it holds the operator private key once via
//! [`alloy::signers::local::PrivateKeySigner`], finalizes the EIP-1559 fee
//! fields, and signs type-2 `TxEnvelope` transactions ready for
//! `eth_sendRawTransaction`.
//!
//! # What lives here (rows N4 + N5 of the submission scope — `port-now`)
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
//!
//! # Standalone-core constraint (ADR-005)
//!
//! A standalone Rust consumer `cargo add degenbot` reaches this crate via the
//! umbrella `pub use degenbot_submission;` and can sign + broadcast EIP-1559
//! transactions with **zero Python, zero `pyo3`** (verified by
//! `just check-no-pyo3-in-cores`). The broadcast itself (`eth_sendRawTransaction`
//! `bytes → TxHash`) is the sibling RPC task (`ZUZANP`); this crate owns only
//! the SIGNING that produces the bytes.
//!
//! # Parity (ADR-005 §4.2)
//!
//! The §4.2 oracle is `eth_account.Account.sign_transaction(transaction_dict=
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
//! `_compute_priority_fee` (`YL2MTH`, Simulation) — NOT this crate's concern.
//! The `priority_fee` is CONSUMED off `tx_params` (no hard edge — the
//! Simulation leaf computes it; submission reads it). For §4.2 parity the fee
//! is a fixture value.
//!
//! # Non-goals
//!
//! - The `eth_sendRawTransaction` broadcast (`bytes → TxHash`) — `ZUZANP`.
//! - `next_base_fee` — `JTLWA3` (consumed via `base_fee_next`).
//! - `_compute_priority_fee` (the `priority_fee` value) — `YL2MTH` (consumed
//!   via `tx_params`; for §4.2 it is a fixture).
//! - The submit orchestration (claim nonce → finalize → re-compute access
//!   list → sign → broadcast → monitor) — the N6 sibling.
//! - Private-key CONFIG load — stays Python (the driver loads the key and
//!   hands it to the Rust signer ONCE).
//! - The `SubmittedTx` monitor / dispatcher state — sibling tasks in the epic.

pub mod dispatcher;
pub mod error;
pub mod fee;
pub mod monitor;
pub mod params;
pub mod signer;
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
pub use monitor::{
    monitor_pending_transaction, monitor_pending_transaction_default, MonitorOutcome, ReceiptProbe,
    SubmittedTx, BLOCKS_BEFORE_NONCE_EXPIRES, MONITOR_POLL_INTERVAL,
};
pub use submit::{
    dispatch_and_submit, fetch_fee_history, SkipReason, SubmitCandidate, SubmitOutcome,
    SubmitRecord,
};

// Test-only span capture for the OTel tier-1 span tests (RMHQAR, epic
// 2LXPPV). One global subscriber per test process: async tests cross
// tasks, where scoped `with_default` guards would be unsafe (MQUKB6
// finding), so every test module in this crate shares the same capture
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
