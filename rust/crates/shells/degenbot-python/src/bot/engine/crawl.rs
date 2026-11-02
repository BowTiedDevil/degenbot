//! The crawl's Python surface: `begin_crawl` and the session it hands back.
//!
//! This module is the SHELL side of the crawl. The engine owns the loop, the
//! fold, and the receipts (`degenbot_bot::arb_engine::crawl`); Python owns the
//! operator surface, the configuration resolution, and the render. Everything
//! here is a translation:
//!
//! - [`crawl_config_from_python`] turns the driver's resolved configuration —
//!   route, cap, discovery scope, retry value — into the core's `CrawlConfig`,
//!   and [`crawl_path_policy_from_python`] turns the strategy's policy into the
//!   core's policy-as-data (`CrawlPathPolicy`). The adapter lives HERE, never in
//!   the cores: the core crate carries no pyo3.
//! - [`PyBatchPolicyOracle`] is the bulk-verdict escape: a foreign predicate the
//!   core cannot evaluate as data, consulted through the core's
//!   `BatchPolicyOracle` trait. A policy expressible as data never crosses.
//! - [`PyCrawlSession`] is the trim-proof identity the retired
//!   `ConstructionContext` used to be: the engine driver booted the crawl and
//!   holds the run's join handle, and this session holds the controller, so
//!   `enqueue_path`/`trigger_discovery` keep working after the main-loop trim.
//!
//! Python renders; it never folds. `progress_line`, `stage_profile_lines`, and
//! `summary` are the core's own fold, rendered byte for byte as the Python
//! pipeline rendered it, so the port's before/after comparison is a text diff.

use super::{Arc, PyArbEngine, PyBot};
use crate::prelude::*;

use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration, PyValueError};
use pyo3::types::{PyDict, PyList};

use degenbot_bot::arb_engine::{CrawlBoot, EngineDriver};
use degenbot_bot::bot_core::ConstructionRoute;
use degenbot_bot::crawl::{
    BatchPolicyOracle, CrawlAbort, CrawlConfig, CrawlConfigError, CrawlController, CrawlPathPolicy,
    CrawlReceipt, CrawlRequestError, DiscoveredHop, DiscoveredPath, DuplicatePoolPolicy, HopBounds,
    LiquidityFloor, LiquidityProxy, OracleError, PathPolicyVerdict, PolicyCandidate, PolicyRefusal,
    RunOutcome, SnapshotArm, TokenFilter, UnitStage,
};
use degenbot_core::retry::RetryPolicy;
use degenbot_db::snapshot::TickMapDb;
use degenbot_pathfinding::graph::PoolKind as CorePoolKind;

/// The crawl's process-lifetime session handle.
///
/// Held for the process's life by whatever booted the crawl: the controller
/// behind it is the operator surface's only door, and the receipt stream is the
/// render's.
#[pyclass(name = "CrawlSession", module = "degenbot._ffi")]
pub struct PyCrawlSession {
    /// The engine's controller — the pose, the fold, the operator surface.
    controller: Arc<CrawlController>,
    /// The receipt stream's receiver, taken once by [`PyCrawlSession::receipts`].
    receipts: parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<CrawlReceipt>>>,
}

#[pymethods]
impl PyCrawlSession {
    /// The crawl's pose: `idle`, `running`, `capped`, `stopping`, or `stopped`.
    fn state(&self) -> &'static str {
        self.controller.state().as_str()
    }

    /// Whether the run future has resolved (the join fact).
    fn is_finished(&self) -> bool {
        self.controller.is_finished()
    }

    /// How the last run resolved, as `(kind, detail)`: `kind` is `completed`,
    /// `aborted`, or `failed`, and `detail` is `None` only for `completed`.
    ///
    /// `None` means no run has resolved yet (or it was cancelled mid-flight —
    /// the driver's join reports that one).
    fn run_outcome(&self) -> Option<(&'static str, Option<String>)> {
        self.controller.run_outcome().map(|outcome| match outcome {
            RunOutcome::Completed => ("completed", None),
            RunOutcome::Aborted(detail) => ("aborted", Some(detail)),
            RunOutcome::Failed(detail) => ("failed", Some(detail)),
        })
    }

    /// The counters line the crawl has always logged (`[build_paths] Progress:
    /// ...`), rendered from the engine's fold.
    fn progress_line(&self) -> String {
        self.controller.progress_line()
    }

    /// The stage-profile block (`[build_paths] Unit-stage profile ...`),
    /// rendered from the engine's fold.
    fn stage_profile_lines(&self) -> Vec<String> {
        self.controller.stage_profile_lines()
    }

    /// The summary counters, as a dict — the engine's fold, never a Python
    /// re-count.
    fn summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let summary = self.controller.summary();
        let report = &summary.report;
        let dict = PyDict::new(py);
        dict.set_item("units_folded", report.units_folded)?;
        dict.set_item("path_count", report.path_count)?;
        dict.set_item("skip_count", report.skip_count)?;
        dict.set_item("token_filter_count", report.token_filter_count)?;
        dict.set_item("engine_reject_count", report.engine_reject_count)?;
        dict.set_item("register_fail_count", report.register_fail_count)?;
        dict.set_item("cap_skip_count", report.cap_skip_count)?;
        dict.set_item("dup_count", report.dup_count)?;
        dict.set_item("v4_pool_count", report.v4_pool_count)?;
        dict.set_item("v4_hook_rejected", report.v4_hook_rejected)?;
        dict.set_item("v4_dynamic_fee_rejected", report.v4_dynamic_fee_rejected)?;
        dict.set_item("other_exc_count", report.other_exc_count)?;
        dict.set_item("uncounted_skip_count", report.uncounted_skip_count)?;
        dict.set_item("capped", report.capped)?;
        let reasons = PyDict::new(py);
        for (label, count) in &report.skip_reasons {
            reasons.set_item(label.as_str(), *count)?;
        }
        dict.set_item("skip_reasons", reasons)?;
        Ok(dict)
    }

    /// The receipt stream — the crawl's observation, taken once.
    ///
    /// The stream ends when the run future resolves, however it resolved: a
    /// consumer that drains it has already reached the resolution edge, and
    /// [`Self::run_outcome`] then says how the run ended.
    ///
    /// # Errors
    ///
    /// `RuntimeError` when the stream was already taken (one consumer).
    fn receipts(&self) -> PyResult<PyCrawlReceiptStream> {
        let rx = self.receipts.lock().take().ok_or_else(|| {
            PyRuntimeError::new_err("the crawl's receipt stream is already taken")
        })?;
        Ok(PyCrawlReceiptStream {
            rx: Arc::new(parking_lot::Mutex::new(Some(rx))),
        })
    }

    /// Add ONE specific path at any time (the operator surface).
    ///
    /// The path is a sequence of `PathStep`-shaped hops (`address`, `type`,
    /// `hash`); `directions` pins the per-hop `zero_for_one` values.
    ///
    /// Returns the unit's submission sequence.
    ///
    /// # Errors
    ///
    /// A typed crawl-state refusal after a stop, a discovery failure, or the
    /// unit's loud abort.
    #[pyo3(signature = (path, directions=None))]
    fn enqueue_path<'py>(
        &self,
        py: Python<'py>,
        path: &Bound<'py, PyAny>,
        directions: Option<Vec<bool>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let discovered = discovered_path_from_py(py, path)?;
        let controller = Arc::clone(&self.controller);
        crate::ambient_runtime::future_into_py(py, async move {
            controller
                .enqueue_path(discovered, directions)
                .await
                .map_err(map_crawl_request_error)
        })
    }

    /// Trigger ONE bounded discovery sweep (the operator surface).
    ///
    /// Returns the number of candidates the sweep processed (`0` when the
    /// structural edition is unchanged since the last natural completion).
    ///
    /// # Errors
    ///
    /// A typed crawl-state refusal, a discovery failure, or a unit's loud abort.
    #[pyo3(signature = (bound=None))]
    fn trigger_discovery<'py>(
        &self,
        py: Python<'py>,
        bound: Option<usize>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let controller = Arc::clone(&self.controller);
        crate::ambient_runtime::future_into_py(py, async move {
            controller
                .trigger_discovery(bound)
                .await
                .map_err(map_crawl_request_error)
        })
    }

    /// Latch the crawl stopped: no new work enters, the window drains, and the
    /// run resolves on its own. Total, idempotent, never blocking.
    fn stop(&self) {
        self.controller.stop();
    }

    fn __repr__(&self) -> String {
        format!(
            "CrawlSession(state={}, finished={})",
            self.controller.state().as_str(),
            self.controller.is_finished()
        )
    }
}

/// The receipt stream: an async iterator of receipt dicts.
#[pyclass(name = "CrawlReceiptStream", module = "degenbot._ffi")]
pub struct PyCrawlReceiptStream {
    rx: Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<CrawlReceipt>>>>,
}

#[pymethods]
impl PyCrawlReceiptStream {
    #[expect(clippy::missing_const_for_fn)]
    fn __aiter__(slf: PyClassGuard<'_, Self>) -> PyClassGuard<'_, Self> {
        slf
    }

    /// Await the next receipt; raises `StopAsyncIteration` when the stream ends
    /// (the crawl's run future resolved).
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = Arc::clone(&self.rx);
        crate::ambient_runtime::future_into_py(py, async move {
            let mut guard = rx.lock().take().ok_or_else(|| {
                PyStopAsyncIteration::new_err("the crawl's receipt stream is over")
            })?;
            let receipt = guard.recv().await;
            *rx.lock() = Some(guard);
            let Some(receipt) = receipt else {
                return Err(PyStopAsyncIteration::new_err(
                    "the crawl's run resolved; the receipt stream is over",
                ));
            };
            Python::attach(|py| receipt_to_py(py, &receipt))
        })
    }
}

/// One receipt as a Python dict (called under the GIL).
fn receipt_to_py(py: Python<'_>, receipt: &CrawlReceipt) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("seq", receipt.seq)?;
    dict.set_item("kind", receipt.kind())?;
    dict.set_item("tag", receipt.refusal_tag())?;
    dict.set_item("path_id", receipt.path_id)?;
    dict.set_item("stage", receipt.answering_stage().map(UnitStage::name))?;
    dict.set_item("stage_ns", receipt.split.as_slice().to_vec())?;
    Ok(dict.into_any().unbind())
}

// ── the boot entry ──────────────────────────────────────────────────────────

#[pymethods]
impl PyArbEngine {
    /// Configure and boot the engine's crawl.
    ///
    /// The engine driver boots the run and holds its join handle; the returned
    /// session holds the controller for the process's life, so the operator
    /// surface keeps working after the main-loop trim. `bot` supplies the
    /// snapshot slot a head-stamped V3/V4 construction asks for its tick-map
    /// handle — per use, so the trim's `close_snapshot_tx()` stays the sole
    /// holder of the handle and later units read the chain instead.
    ///
    /// # Errors
    ///
    /// `ValueError` for a configuration the crawl cannot honour (the core's own
    /// validation message) and `RuntimeError` when the engine has no construction
    /// I/O or a crawl is already installed.
    #[pyo3(signature = (config, bot=None))]
    fn begin_crawl(
        &self,
        py: Python<'_>,
        config: &Bound<'_, PyAny>,
        bot: Option<Bound<'_, PyBot>>,
    ) -> PyResult<PyCrawlSession> {
        let (mut parsed, oracle) = crawl_config_from_python(py, config)?;
        parsed.oracle = oracle.map(|oracle| Arc::new(oracle) as Arc<dyn BatchPolicyOracle>);
        // The tick-map arm is the bot's snapshot SLOT, not a handle: the crawl
        // asks it per unit, so the close owns the one handle it armed.
        let snapshot: Option<Arc<dyn SnapshotArm>> = bot.and_then(|bot| {
            bot.try_borrow().ok().map(|bot| {
                Arc::new(PySnapshotArm {
                    slot: bot.db_slot(),
                }) as Arc<dyn SnapshotArm>
            })
        });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let crawl_boot = CrawlBoot {
            config: parsed,
            receipts: Some(tx),
            snapshot,
        };
        let driver: Arc<EngineDriver> = Arc::clone(self.session.driver());
        let controller = driver
            .begin_crawl_with(crawl_boot)
            .map_err(|error| map_crawl_config_error(&error))?;
        Ok(PyCrawlSession {
            controller,
            receipts: parking_lot::Mutex::new(Some(rx)),
        })
    }
}

/// The crawl's tick-map arm: `PyBot`'s snapshot slot, asked afresh per unit.
///
/// `PyBot` owns the ONE long-lived `SnapshotDb` handle: `load_snapshot_from_db`
/// arms the slot, `close_snapshot_tx()` empties it (its `Arc` canary read the
/// reason it must be the sole holder). This arm is that slot, never a handle: a
/// unit clones for its own run and drops it on resolution, so the close finds
/// the bot's `Arc` alone and every ask after the close answers `None` — the
/// no-DB arm, chain reads. The retired Python pipeline held the same contract by
/// calling `PyBot::db_handle()` per use; a session-scoped clone would pin the
/// read transaction past the close and keep reading a stale DB view.
struct PySnapshotArm {
    slot: Arc<parking_lot::Mutex<Option<Arc<degenbot_db::snapshot_db::SnapshotDb>>>>,
}

impl SnapshotArm for PySnapshotArm {
    fn acquire(&self) -> Option<Arc<dyn TickMapDb>> {
        self.slot.lock().clone().map(|db| db as Arc<dyn TickMapDb>)
    }
}

// ── the configuration adapter ───────────────────────────────────────────────

/// Translate the driver's resolved configuration into the core's `CrawlConfig`.
///
/// The driver supplies VALUES: the route, the cap, the discovery scope, the
/// retry policy, the policy as data. A malformed value raises a named
/// `ValueError` here, at configure, never mid-crawl.
///
/// Returns the configuration beside the optional foreign oracle (a predicate
/// that is NOT expressible as policy data).
///
/// # Errors
///
/// `ValueError` naming the field that could not be translated.
#[expect(
    clippy::too_many_lines,
    reason = "one flat translation table: the config schema is read field by field, in the order the core's CrawlConfig declares them"
)]
pub(crate) fn crawl_config_from_python(
    py: Python<'_>,
    config: &Bound<'_, PyAny>,
) -> PyResult<(CrawlConfig, Option<PyBatchPolicyOracle>)> {
    let config = config
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("begin_crawl: the crawl config must be a dict"))?;
    let required = |key: &str| -> PyResult<Bound<'_, PyAny>> {
        config
            .get_item(key)?
            .ok_or_else(|| PyValueError::new_err(format!("begin_crawl: the config needs `{key}`")))
    };
    let optional = |key: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
        Ok(config.get_item(key)?.filter(|value| !value.is_none()))
    };

    let route = required("route")?;
    let route = route
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("begin_crawl: `route` must be a dict"))?;
    let factories = route
        .get_item("factories")?
        .map(|value| value.extract::<Vec<String>>())
        .transpose()?
        .unwrap_or_default()
        .iter()
        .map(|text| parse_config_address("route.factories", text))
        .collect::<PyResult<Vec<_>>>()?;
    let generic = route
        .get_item("generic")?
        .map(|value| value.extract::<bool>())
        .transpose()?
        .unwrap_or(true);

    let pool_kinds = pool_kinds_from_py(py, &required("pool_kinds")?, "pool_kinds")?;
    let pool_type_per_depth = match optional("pool_type_per_depth")? {
        Some(value) => Some(
            value
                .extract::<Vec<Option<Vec<Bound<'_, PyAny>>>>>()?
                .into_iter()
                .map(|depth| -> PyResult<Option<Vec<CorePoolKind>>> {
                    depth
                        .map(|kinds| {
                            pool_kinds_from_py(
                                py,
                                &PyList::new(py, kinds)?.into_any(),
                                "pool_type_per_depth",
                            )
                        })
                        .transpose()
                })
                .collect::<PyResult<Vec<_>>>()?,
        ),
        None => None,
    };

    let retry = required("retry")?;
    let retry = retry
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("begin_crawl: `retry` must be a dict"))?;
    let retry = RetryPolicy {
        max_attempts: retry
            .get_item("max_attempts")?
            .map(|value| value.extract::<u32>())
            .transpose()?
            .unwrap_or(1),
        base_delay: retry
            .get_item("base_delay")?
            .map(|value| value.extract::<f64>())
            .transpose()?
            .unwrap_or(0.0),
        max_delay: retry
            .get_item("max_delay")?
            .map(|value| value.extract::<f64>())
            .transpose()?
            .unwrap_or(0.0),
        jitter: retry
            .get_item("jitter")?
            .map(|value| value.extract::<f64>())
            .transpose()?
            .unwrap_or(0.0),
    };

    let policy = match optional("policy")? {
        Some(value) => crawl_path_policy_from_python(&value)?,
        None => CrawlPathPolicy::default(),
    };
    let oracle = optional("oracle")?.map(|value| PyBatchPolicyOracle::new(py, value.unbind()));

    let crawl = CrawlConfig {
        chain_id: required("chain_id")?.extract()?,
        database_path: std::path::PathBuf::from(required("database_path")?.extract::<String>()?),
        route: ConstructionRoute { factories, generic },
        pool_kinds,
        pool_type_per_depth,
        start_tokens: config_addresses(&required("start_tokens")?, "start_tokens")?,
        end_tokens: config_addresses(&required("end_tokens")?, "end_tokens")?,
        min_depth: required("min_depth")?.extract()?,
        max_depth: optional("max_depth")?
            .map(|value| value.extract::<usize>())
            .transpose()?,
        discovery_batch_size: required("discovery_batch_size")?.extract()?,
        intake_window: required("intake_window")?.extract()?,
        path_cap: optional("path_cap")?
            .map(|value| value.extract::<usize>())
            .transpose()?,
        retry,
        policy,
        oracle: None,
        policy_batch_size: optional("policy_batch_size")?
            .map(|value| value.extract::<usize>())
            .transpose()?
            .unwrap_or(1),
        profit_token: parse_config_address(
            "profit_token",
            &required("profit_token")?.extract::<String>()?,
        )?,
        snapshot_block: optional("snapshot_block")?
            .map(|value| value.extract::<u64>())
            .transpose()?,
        state_block: optional("state_block")?
            .map(|value| value.extract::<u64>())
            .transpose()?,
    };
    Ok((crawl, oracle))
}

/// The core's path-composition policy, from the strategy's policy as data.
///
/// The Python `PathPolicy` carries a caller-supplied `liquidity_of` callable;
/// the core cannot hold a callable, so the driver names the proxy it resolved
/// (`reserve_sum` / `active_liquidity`) and a floor with no named proxy is a
/// configuration error rather than a silently disabled rule.
///
/// # Errors
///
/// `ValueError` naming the rule that cannot be translated (a malformed token
/// address, an unknown proxy, a non-integer bound).
pub(crate) fn crawl_path_policy_from_python(value: &Bound<'_, PyAny>) -> PyResult<CrawlPathPolicy> {
    let policy = value
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("begin_crawl: `policy` must be a dict"))?;
    let optional = |key: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
        Ok(policy.get_item(key)?.filter(|value| !value.is_none()))
    };

    let hop_bounds = HopBounds {
        min: optional("min_hops")?
            .map(|value| value.extract::<usize>())
            .transpose()?
            .unwrap_or(1),
        max: optional("max_hops")?
            .map(|value| value.extract::<usize>())
            .transpose()?,
    };
    let deny = match optional("disallowed_tokens")? {
        Some(value) => config_addresses(&value, "policy.disallowed_tokens")?
            .into_iter()
            .collect(),
        None => std::collections::BTreeSet::new(),
    };
    let allow = match optional("allowed_tokens")? {
        Some(value) => Some(
            config_addresses(&value, "policy.allowed_tokens")?
                .into_iter()
                .collect(),
        ),
        None => None,
    };
    let duplicate_pools = if optional("reject_duplicate_pools")?
        .map(|value| value.extract::<bool>())
        .transpose()?
        .unwrap_or(true)
    {
        DuplicatePoolPolicy::Reject
    } else {
        DuplicatePoolPolicy::Allow
    };
    let liquidity_floor = match optional("min_liquidity")? {
        Some(value) => {
            let min = value.extract::<u128>()?;
            if min == 0 {
                None
            } else {
                let proxy = match optional("liquidity_proxy")?
                    .map(|proxy| proxy.extract::<String>())
                    .transpose()?
                    .as_deref()
                {
                    Some("reserve_sum") => LiquidityProxy::ReserveSum,
                    Some("active_liquidity") => LiquidityProxy::ActiveLiquidity,
                    Some(other) => {
                        return Err(PyValueError::new_err(format!(
                            "begin_crawl: policy.liquidity_proxy `{other}` is not a proxy the core evaluates (reserve_sum, active_liquidity)"
                        )));
                    }
                    None => {
                        return Err(PyValueError::new_err(
                            "begin_crawl: policy.min_liquidity needs policy.liquidity_proxy (the callable `liquidity_of` cannot cross the seam)",
                        ));
                    }
                };
                Some(LiquidityFloor {
                    min: alloy::primitives::U256::from(min),
                    proxy,
                })
            }
        }
        None => None,
    };
    Ok(CrawlPathPolicy {
        hop_bounds,
        tokens: TokenFilter { deny, allow },
        duplicate_pools,
        liquidity_floor,
    })
}

/// The bulk-verdict escape: a foreign Python policy the core cannot evaluate as
/// data.
///
/// The Python object answers `verdicts(candidates)` with a list of `None`
/// (admit) or a denial detail string, one per candidate, in order. The crossing
/// is one call per consult; the crawl consults one candidate at a time in v1
/// order (the composition view exists only after the build stage), and any
/// answer whose length disagrees with the batch is a loud error, never a guess.
pub struct PyBatchPolicyOracle {
    predicate: Py<PyAny>,
}

impl std::fmt::Debug for PyBatchPolicyOracle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PyBatchPolicyOracle")
    }
}

impl PyBatchPolicyOracle {
    /// Wrap a Python object implementing `verdicts(candidates)`.
    fn new(_py: Python<'_>, predicate: Py<PyAny>) -> Self {
        Self { predicate }
    }
}

impl BatchPolicyOracle for PyBatchPolicyOracle {
    fn verdicts(&self, batch: &[PolicyCandidate]) -> Result<Vec<PathPolicyVerdict>, OracleError> {
        let batch = batch.to_vec();
        Python::attach(|py| {
            let mut items = Vec::with_capacity(batch.len());
            for candidate in &batch {
                items.push(
                    candidate_to_py(py, candidate).map_err(|err| OracleError::Refused {
                        detail: err.to_string(),
                    })?,
                );
            }
            let list = PyList::new(py, items).map_err(|err| OracleError::Refused {
                detail: err.to_string(),
            })?;
            let answer = self
                .predicate
                .bind(py)
                .call_method1("verdicts", (list,))
                .map_err(|err| OracleError::Refused {
                    detail: err.to_string(),
                })?;
            let answers =
                answer
                    .extract::<Vec<Option<String>>>()
                    .map_err(|err| OracleError::Refused {
                        detail: format!(
                        "the policy oracle's verdict list is not `None | str` per candidate: {err}"
                    ),
                    })?;
            if answers.len() != batch.len() {
                return Err(OracleError::LengthMismatch {
                    asked: batch.len(),
                    answered: answers.len(),
                });
            }
            Ok(answers
                .into_iter()
                .map(|answer| match answer {
                    None => PathPolicyVerdict::Admit,
                    Some(detail) => PathPolicyVerdict::Deny {
                        refusal: PolicyRefusal::Oracle { detail },
                    },
                })
                .collect())
        })
    }
}

/// One policy candidate as the Python oracle's dict.
fn candidate_to_py(py: Python<'_>, candidate: &PolicyCandidate) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("seq", candidate.seq)?;
    dict.set_item("hop_sig", candidate.hop_sig.clone())?;
    let hops = PyList::empty(py);
    for hop in &candidate.view.hops {
        let hop_dict = PyDict::new(py);
        hop_dict.set_item("pool_id", hop.pool_id)?;
        hop_dict.set_item("family", format!("{:?}", hop.family))?;
        hop_dict.set_item("zero_for_one", hop.zero_for_one)?;
        hop_dict.set_item("token_in", hop.token_in.to_string())?;
        hop_dict.set_item("token_out", hop.token_out.to_string())?;
        hop_dict.set_item("identity", format!("{:?}", hop.identity))?;
        hops.append(hop_dict)?;
    }
    dict.set_item("hops", hops)?;
    Ok(dict.into_any().unbind())
}

// ── the shape adapters ──────────────────────────────────────────────────────

/// Read a boundary/profit address list, each entry a `0x`-hex string.
fn config_addresses(
    value: &Bound<'_, PyAny>,
    field: &str,
) -> PyResult<Vec<alloy::primitives::Address>> {
    value
        .extract::<Vec<String>>()?
        .iter()
        .map(|text| parse_config_address(field, text))
        .collect()
}

/// Parse one configured address, naming the field on failure.
fn parse_config_address(field: &str, text: &str) -> PyResult<alloy::primitives::Address> {
    text.parse::<alloy::primitives::Address>().map_err(|err| {
        PyValueError::new_err(format!(
            "begin_crawl: {field}: `{text}` is not an address: {err}"
        ))
    })
}

/// Read the typed `PoolKind` values of one configured family list.
fn pool_kinds_from_py(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    field: &str,
) -> PyResult<Vec<CorePoolKind>> {
    value
        .extract::<Vec<Bound<'_, PyAny>>>()?
        .into_iter()
        .map(|kind| {
            crate::pathfinding::extract_pool_kind(py, &kind)
                .map(crate::pathfinding::PoolKind::to_core)
                .map_err(|err| PyValueError::new_err(format!("begin_crawl: {field}: {err}")))
        })
        .collect()
}

/// Translate a discovered path (a sequence of `PathStep`-shaped hops) into the
/// core's per-hop identity.
fn discovered_path_from_py(py: Python<'_>, path: &Bound<'_, PyAny>) -> PyResult<DiscoveredPath> {
    let mut hops = Vec::new();
    for step in path.extract::<Vec<Bound<'_, PyAny>>>()? {
        hops.push(discovered_hop_from_py(py, &step)?);
    }
    if hops.is_empty() {
        return Err(PyValueError::new_err(
            "enqueue_path: the path carries no hops",
        ));
    }
    Ok(DiscoveredPath { hops })
}

/// One `PathStep` (`address`, `type`, `hash`) as a discovered hop.
fn discovered_hop_from_py(py: Python<'_>, step: &Bound<'_, PyAny>) -> PyResult<DiscoveredHop> {
    let address = step
        .getattr("address")
        .and_then(|value| value.extract::<String>())
        .map_err(|_| PyValueError::new_err("enqueue_path: a hop needs an `address` string"))?;
    let address = parse_config_address("enqueue_path.hop.address", &address)?;
    let kind = crate::pathfinding::extract_pool_kind(
        py,
        &step
            .getattr("type")
            .map_err(|_| PyValueError::new_err("enqueue_path: a hop needs a `type` PoolKind"))?,
    )?;
    Ok(match kind {
        crate::pathfinding::PoolKind::V2 => DiscoveredHop::V2 { address },
        crate::pathfinding::PoolKind::V3 => DiscoveredHop::V3 { address },
        crate::pathfinding::PoolKind::V4 => {
            let hash = step
                .getattr("hash")
                .ok()
                .filter(|value| !value.is_none())
                .map(|value| -> PyResult<[u8; 32]> {
                    let text = value.extract::<String>().map_err(|_| {
                        PyValueError::new_err("enqueue_path: a V4 hop's `hash` must be 0x-hex")
                    })?;
                    let parsed = text.parse::<alloy::primitives::B256>().map_err(|err| {
                        PyValueError::new_err(format!(
                            "enqueue_path: bad V4 pool hash `{text}`: {err}"
                        ))
                    })?;
                    Ok(parsed.0)
                })
                .transpose()?;
            DiscoveredHop::V4 {
                pool_manager: address,
                pool_hash: hash,
            }
        }
    })
}

// ── the error translators ───────────────────────────────────────────────────

/// A configuration refusal: a bad value is the caller's `ValueError`; a missing
/// construction I/O, an existing crawl, or a refused receipt sink is a runtime
/// precondition failure.
fn map_crawl_config_error(error: &CrawlConfigError) -> PyErr {
    let message = error.to_string();
    match error {
        CrawlConfigError::MissingConstructionIo
        | CrawlConfigError::CrawlAlreadyBegun
        | CrawlConfigError::Receipts(_) => PyRuntimeError::new_err(message),
        _ => PyValueError::new_err(message),
    }
}

/// A crawl request refusal. The loud-abort arm keeps the verify/build/direction
/// vocabulary Python already catches (the same typed exceptions the Python
/// pipeline raised), so a caller's `except` clauses stay true.
fn map_crawl_request_error(error: CrawlRequestError) -> PyErr {
    match error {
        CrawlRequestError::Abort(abort) => map_crawl_abort(abort),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// The loud-abort translation.
fn map_crawl_abort(abort: CrawlAbort) -> PyErr {
    match abort {
        CrawlAbort::Verify(kind) | CrawlAbort::Construction(kind) => {
            crate::bot::errmap::map_refusal(kind)
        }
        CrawlAbort::Direction(error) => {
            let detail = error.to_string();
            Python::attach(|py| crate::pathfinding::direction_resolution_error(py, detail))
        }
        CrawlAbort::Invariant { detail } | CrawlAbort::Policy { detail } => {
            PyRuntimeError::new_err(detail)
        }
    }
}
