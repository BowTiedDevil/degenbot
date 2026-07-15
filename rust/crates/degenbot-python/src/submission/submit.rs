//! `dispatch_and_submit_py` / `fetch_fee_history_py` — the `PyO3` seams over the
//! N6 submit-orchestration leaves ([`dispatch_and_submit`] + [
//! `fetch_fee_history`]) + the `PySubmitCandidate` builder + the local
//! [`ReceiptProbe`] impl.
//!
//! These close the cutover: the example's Python `dispatch_profitable_results`
//! submit-tail (the mutual-excl guard → `claim_nonce` → access-list re-compute
//! → sign → `eth_sendRawTransaction` → `reserve_pools` → monitor-spawn) becomes
//! a `dispatch_and_submit_py(candidates, dispatcher, provider, signer, probe,
//! …)` call, + `_apply_block_if_ready`'s `eth_feeHistory`+hex-decode block
//! becomes `fetch_fee_history_py(…)`.
//!
//! # GIL discipline (ADR-005 §3 C)
//!
//! Both `#[pyfunction]`s are `async` + release the GIL across the RPC `.await`s
//! (`future_into_py` runs the future on the tokio runtime the Python event loop
//! drives — the GIL is NOT held while `eth_sendRawTransaction` /
//! `eth_createAccessList` / `eth_feeHistory` block on the network). The arg
//! extraction (candidate build, address parse, percentiles) is done under the
//! GIL before the `await`; the result wrap (Python dicts from `SubmitRecord`)
//! is done under the GIL after.
//!
//! # The receipt probe (orphan-rule solution)
//!
//! [`ReceiptProbe`] is a foreign trait (from `degenbot_submission`) + the
//! `Arc<dyn Provider<Ethereum>>` it polls is a foreign type — but the impl is
//! on [`PyReceiptProbe`], a LOCAL struct defined in this crate, so the orphan
//! rule is satisfied. The probe polls `Provider::get_transaction_receipt`
//! (typed) — `true` iff a receipt exists (the transaction is mined).

use crate::prelude::*;
use crate::rpc::async_provider::PyAsyncAlloyProvider;
use crate::submission::dispatcher::PyDispatcher;
use crate::submission::params::parse_access_list;
use crate::submission::signer::PyTxSigner;
use degenbot_submission::{
    dispatch_and_submit, fetch_fee_history, PoolKey, ReceiptProbe, SkipReason, SubmitCandidate,
    SubmitOutcome, SubmitRecord,
};
use pyo3::exceptions::PyValueError;
use pyo3::types::{PyBool, PyBytes, PyDict, PyList};
use pyo3_async_runtimes::tokio::future_into_py;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::provider::AlloyProvider;
use alloy::network::Ethereum;
use alloy::primitives::{B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::AccessList as AlloyAccessList;

/// `PySubmitCandidate` — the builder the Python submit-tail constructs from
/// each `gas_profitable` entry before handing the batch to
/// [`dispatch_and_submit_py`].
///
/// Fields mirror [`SubmitCandidate`] 1:1. Python passes:
///  * `path_id`, `gas_used`: `int`
///  * `gross_profit`, `net_profit`: `int` (wei)
///  * `priority_fee`, `base_fee_next`: `int` (wei)
///  * `execute_calldata`: `bytes`
///  * `executor_address`: hex string
///  * `access_list`: web3-shape list (optional; `None` to skip)
///  * `path_pools`: `set[str]` (V4 `pool_id_hex` / V2-V3 `pool_address`)
#[pyclass(
    name = "PySubmitCandidate",
    skip_from_py_object,
    module = "degenbot._ffi"
)]
pub struct PySubmitCandidate {
    pub(crate) inner: SubmitCandidate,
}

#[pymethods]
impl PySubmitCandidate {
    /// Build a submit candidate from the dispatch fan-out's per-path fields.
    ///
    /// All money fields are wei (`int`). The priority fee is the
    /// `_compute_priority_fee` output (computed Python-side — S3 stay-Python —
    /// + passed IN).
    #[new]
    #[pyo3(signature = (
        path_id, gross_profit, net_profit, gas_used, priority_fee,
        base_fee_next, execute_calldata, executor_address,
        access_list=None, path_pools=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        path_id: u64,
        gross_profit: &Bound<'_, PyAny>,
        net_profit: &Bound<'_, PyAny>,
        gas_used: u64,
        priority_fee: u128,
        base_fee_next: u128,
        execute_calldata: &Bound<'_, PyBytes>,
        executor_address: &str,
        access_list: Option<Bound<'_, PyList>>,
        path_pools: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let gross = int_to_u256(py, gross_profit)?;
        let net = int_to_u256(py, net_profit)?;
        let executor = crate::address_utils::parse_address(executor_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid executor address: {e}")))?;
        let calldata = execute_calldata.as_bytes().to_vec();

        let access_list: Option<AlloyAccessList> =
            access_list.map(|lst| parse_access_list(&lst)).transpose()?;

        let path_pools: HashSet<PoolKey> = match path_pools {
            Some(p) => p
                .try_iter()?
                .map(|item| {
                    let s: String = item?.extract()?;
                    Ok::<_, PyErr>(PoolKey::from(s))
                })
                .collect::<PyResult<_>>()?,
            None => HashSet::new(),
        };

        let inner = SubmitCandidate {
            path_id,
            gross_profit: gross,
            net_profit: net,
            gas_used,
            priority_fee,
            base_fee_next,
            execute_calldata: calldata.into(),
            executor_address: executor,
            access_list,
            path_pools,
        };
        Ok(Self { inner })
    }

    // ── read-only getters (the rewired `[dispatch]` per-path log reads these
    //    from each survivor — A5). --------------------------------------------
    #[getter]
    fn path_id(&self) -> u64 {
        self.inner.path_id
    }

    #[getter]
    fn gross_profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        alloy_py::u256_to_py(py, &self.inner.gross_profit)
    }

    #[getter]
    fn net_profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        alloy_py::u256_to_py(py, &self.inner.net_profit)
    }

    #[getter]
    fn gas_used(&self) -> u64 {
        self.inner.gas_used
    }

    #[getter]
    fn priority_fee(&self) -> u128 {
        self.inner.priority_fee
    }
}

/// The local `ReceiptProbe` impl — polls `Provider::get_transaction_receipt`.
///
/// Held as an `Arc<dyn Provider<Ethereum>>` (cloned from the
/// `AsyncAlloyProvider` the caller passes) so the spawned monitor tasks can
/// poll receipts without crossing back into Python per-poll (ADR-005 §3 D —
/// the monitor is pure Rust-side coordination state release).
pub(crate) struct PyReceiptProbe {
    provider: Arc<dyn Provider<Ethereum>>,
}

impl PyReceiptProbe {
    /// Wrap the provider's inner typed handle.
    pub(crate) fn new(provider: &AlloyProvider) -> Self {
        Self {
            provider: provider.provider_arc(),
        }
    }
}

impl ReceiptProbe for PyReceiptProbe {
    fn receipt_found(
        &self,
        tx_hash: B256,
    ) -> Pin<Box<dyn Future<Output = degenbot_submission::SubmissionResult<bool>> + Send + '_>>
    {
        let provider = Arc::clone(&self.provider);
        Box::pin(async move {
            // `get_transaction_receipt` returns `Ok(Some(_))` once mined,
            // `Ok(None)` while pending, `Err(_)` on RPC failure. The Python
            // oracle wrapped ONLY `except TransactionNotFound`; other RPC
            // errors propagate — `unwrap_or(false)` would mask them. Map the
            // RPC error to a SubmissionError non-"not-found" propagation
            // (matches the monitor's `?` propagation the leaf docs describe).
            let receipt = provider
                .get_transaction_receipt(tx_hash)
                .await
                .map_err(|e| degenbot_submission::SubmissionError::MonitorProbe(e.to_string()))?;
            Ok(receipt.is_some())
        })
    }
}

/// Route a batch of profitable candidates through the Rust submit leaf.
///
/// This replaces the Python submit-tail of `dispatch_profitable_results`
/// (the mutual-excl guard → `claim_nonce` → access-list re-compute → sign →
/// `eth_sendRawTransaction` → `reserve_pools` → monitor-spawn). Python builds
/// the `PySubmitCandidate` list from `gas_profitable` (encode/sim/gas stays
/// Python — S3 stay-Python), then hands the batch + the coordination args.
///
/// Args:
///     `candidates`: list of `PySubmitCandidate` (profit-descending is
///         re-asserted by the leaf).
///     `dispatcher`: the `PyDispatcher` holding the coordination state
///         (the Rust leaves lock the SAME `Arc`).
///     `provider`: the `AsyncAlloyProvider` (the typed RPC handle).
///     `signer`: the `PyTxSigner` (per-tx key never crosses to Python).
///     `operator_nonce`: the baseline account nonce.
///     `current_block`: the by-ref block clock read.
///     `dry_run`: skip live submission (commit pools anyway).
///     `inject_code`: skip live submission (the injected contract doesn't
///         exist on-chain).
///
/// Returns:
///     A list of record dicts — `{"kind": "submitted", "path_id": ...,
///     "tx_hash": "0x...", "nonce": ...}` or `{"kind": "skipped",
///     "path_id": ..., "reason": "pools_claimed"|"dry_run"|"inject_code"|
///     "broadcast_failed", "detail": "..."?}`.
///
/// # Errors
/// `ValueError`: If the dispatch+submit leaf returns a `SubmissionError`
///         (a non-"not-found" RPC error during broadcast/access-list/sign).
#[pyfunction]
#[pyo3(signature = (candidates, dispatcher, provider, signer, operator_nonce, current_block, dry_run, inject_code))]
#[allow(clippy::too_many_arguments)]
pub fn dispatch_and_submit_py<'py>(
    py: Python<'py>,
    candidates: &Bound<'_, PyList>,
    dispatcher: &PyDispatcher,
    provider: &PyAsyncAlloyProvider,
    signer: &PyTxSigner,
    operator_nonce: u64,
    current_block: u64,
    dry_run: bool,
    inject_code: bool,
) -> PyResult<Bound<'py, PyAny>> {
    // ── GIL-held arg extraction ──
    let mut built: Vec<SubmitCandidate> = Vec::with_capacity(candidates.len());
    for item in candidates.iter() {
        let c = item
            .extract::<PyRef<'_, PySubmitCandidate>>()
            .map_err(|_| {
                PyValueError::new_err("candidates must be a list of PySubmitCandidate instances")
            })?;
        built.push(c.inner.clone());
    }
    let dispatcher_arc = dispatcher.inner_arc();
    let provider_arc = provider.provider_arc();
    let signer = signer.signer().clone();
    let probe = Arc::new(PyReceiptProbe::new(&provider_arc));

    // ── GIL release across the RPC submits ──
    future_into_py(py, async move {
        let outcome: SubmitOutcome = dispatch_and_submit(
            built,
            &dispatcher_arc,
            &provider_arc,
            &signer,
            probe as Arc<dyn ReceiptProbe + Send + Sync>,
            operator_nonce,
            current_block,
            dry_run,
            inject_code,
        )
        .await
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:?}")))?;

        // ── GIL re-acquired result wrap ──
        Python::attach(|py| {
            let list = PyList::empty(py);
            for record in &outcome.records {
                let dict = PyDict::new(py);
                match record {
                    SubmitRecord::Submitted {
                        path_id,
                        tx_hash,
                        nonce,
                    } => {
                        dict.set_item("kind", "submitted")?;
                        dict.set_item("path_id", *path_id)?;
                        dict.set_item("tx_hash", format!("{tx_hash:#x}"))?;
                        dict.set_item("nonce", *nonce)?;
                    }
                    SubmitRecord::Skipped { path_id, reason } => {
                        dict.set_item("kind", "skipped")?;
                        dict.set_item("path_id", *path_id)?;
                        let (reason_str, detail) = skip_reason_to_py(reason);
                        dict.set_item("reason", reason_str)?;
                        if let Some(d) = detail {
                            dict.set_item("detail", d)?;
                        }
                    }
                }
                list.append(dict)?;
            }
            Ok::<_, PyErr>(list.unbind())
        })
    })
}

/// Route a single-block `eth_feeHistory` RPC + record into the dispatcher.
///
/// Replaces `_apply_block_if_ready`'s `make_request('eth_feeHistory')` +
/// hex-decode block. Returns `True` if the fee history was fetched + recorded
/// (the dispatcher's `record_priority_fees` ran), `False` on RPC failure
/// (matches the Python `except Web3Exception: pass` no-op).
///
/// Args:
///     `provider`: the `AsyncAlloyProvider`.
///     `dispatcher`: the `PyDispatcher`.
///     `block_count`: blocks to fetch (the Python passed `1`).
///     `last_block`: the highest block in the range (hex-tag in Python).
///     `reward_percentiles`: list of float percentiles (e.g. `[10., 50., 90.]`).
///
/// # Errors
/// `ValueError`: If the RPC `eth_feeHistory` call surfaces a non-"not-found"
///         error (the leaf no-ops on `Web3Exception`, so this is rare).
#[pyfunction]
pub fn fetch_fee_history_py<'py>(
    py: Python<'py>,
    provider: &PyAsyncAlloyProvider,
    dispatcher: &PyDispatcher,
    block_count: u64,
    last_block: u64,
    reward_percentiles: Vec<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let provider_arc = provider.provider_arc();
    let dispatcher_arc = dispatcher.inner_arc();
    let percentiles: Vec<f64> = reward_percentiles;
    future_into_py(py, async move {
        let recorded = fetch_fee_history(
            &provider_arc,
            &dispatcher_arc,
            block_count,
            last_block,
            &percentiles,
        )
        .await;
        Python::attach(|py| Ok(PyBool::new(py, recorded).to_owned().unbind()))
    })
}

/// Map `U256` from a Python int (decimal or `hex` string) — mirrors the
/// `executor/mod.rs` `obj_to_u256` helper shape.
fn int_to_u256(_py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    // Direct int extraction (the common path — Python passes a plain int).
    if let Ok(n) = obj.extract::<u128>() {
        return Ok(U256::from(n));
    }
    // Large wei values exceed u128 — parse via the int's decimal string repr.
    let s = obj.str()?.to_str()?.to_owned();
    if let Some(hex) = s.strip_prefix("0x") {
        return U256::from_str_radix(hex, 16)
            .map_err(|e| PyValueError::new_err(format!("Invalid hex U256: {e}")));
    }
    U256::from_str_radix(&s, 10)
        .map_err(|e| PyValueError::new_err(format!("Invalid decimal U256: {e}")))
}

/// Map `SkipReason` to a Python-friendly `(reason_str, Option<detail>)`.
fn skip_reason_to_py(reason: &SkipReason) -> (&'static str, Option<String>) {
    match reason {
        SkipReason::PoolsClaimed => ("pools_claimed", None),
        SkipReason::DryRun => ("dry_run", None),
        SkipReason::InjectCode => ("inject_code", None),
        SkipReason::BroadcastFailed(detail) => ("broadcast_failed", Some(detail.clone())),
    }
}
