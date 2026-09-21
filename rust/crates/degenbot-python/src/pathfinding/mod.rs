//! `PyO3` bindings for the pathfinding graph + DFS.
//!
//! Thin translator over `degenbot_pathfinding::graph`. No business logic —
//! extract args (flat int tuples from Python) → build `OwnedPathFinder` →
//! yield paths lazily via the Python iterator protocol.
//!
//! `build_path_graph` choreographs the DB read
//! (`degenbot_db::fetch_path_graph_edges` and `fetch_tokens_with_min_degree`),
//! the candidate-token edge filter, and `PathGraph::from_edges`/
//! `prune_dead_ends`; the Python `_prepare_graph` becomes a delegating shell.
//! The DFS half (`find_paths_rust`) is unchanged.

#![expect(clippy::doc_markdown)]

use crate::prelude::*;
#[cfg(all(feature = "pathfinding", feature = "db"))]
// Note: `alloy::primitives::Address` is no longer named in this module
// after the GIL fix — the address maps are pre-computed to
// checksum STRINGS inside the `py.detach` span in `build_path_graph`, so
// `build_graph_dict` holds no `Address` values. Re-add the import if a
// downstream helper here regains an `Address`-typed surface.
#[cfg(all(feature = "pathfinding", feature = "db"))]
use degenbot_db::DegenbotDb;
use degenbot_pathfinding::graph::{OwnedPathFinder, PoolKind as CorePoolKind};
use pyo3::exceptions::{PyKeyError, PyStopAsyncIteration, PyValueError};
use pyo3::types::{PyDict, PyList, PyTuple};
use std::collections::{HashMap, HashSet};
#[cfg(all(feature = "pathfinding", feature = "db"))]
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The pool-family discriminant crossing the FFI.
///
/// This is the single source of truth for the numeric pool-kind
/// discriminants the pathfinding core uses. Python passes these values
/// through the seam — never bare integers — so a kind change is a compile
/// error on both sides, not a silently mistranslated constant.
#[pyclass(eq, hash, frozen, from_py_object, module = "degenbot._ffi")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PoolKind {
    V2,
    V3,
    V4,
}

impl PoolKind {
    /// The matching `degenbot_pathfinding` core discriminant.
    #[must_use]
    pub const fn to_core(self) -> CorePoolKind {
        match self {
            PoolKind::V2 => CorePoolKind::V2,
            PoolKind::V3 => CorePoolKind::V3,
            PoolKind::V4 => CorePoolKind::V4,
        }
    }

    /// Convert from the core discriminant.
    ///
    /// The core enum is `#[non_exhaustive]`, so this routes through its own
    /// `u8` discriminant (which the core defines exhaustively) rather than
    /// matching its variants directly.
    #[must_use]
    pub const fn from_core(kind: CorePoolKind) -> Self {
        match kind.as_u8() {
            0 => PoolKind::V2,
            1 => PoolKind::V3,
            _ => PoolKind::V4,
        }
    }
}

/// Map a Python pool-table class to its [`PoolKind`] family.
///
/// The classification is ordered most-specific first (`UniswapV3PoolTableBase`
/// and `UniswapV2PoolTableBase` both derive from `LiquidityPoolTable`). An
/// unmapped family aborts loudly — at a use site it is an infrastructure gap,
/// never a silent omission.
///
/// # Errors
///
/// Returns `PyValueError` when `pool_type` is not a recognized pool family.
#[pyfunction]
pub fn classify_pool_kind(pool_type: &Bound<'_, PyAny>) -> PyResult<PoolKind> {
    let pools = pool_type.py().import("degenbot.database.models.pools")?;
    let py_type = pool_type.cast::<pyo3::types::PyType>()?;
    if py_type.is_subclass(&pools.getattr("UniswapV4PoolTable")?)? {
        return Ok(PoolKind::V4);
    }
    if py_type.is_subclass(&pools.getattr("UniswapV3PoolTableBase")?)? {
        return Ok(PoolKind::V3);
    }
    if py_type.is_subclass(&pools.getattr("UniswapV2PoolTableBase")?)? {
        return Ok(PoolKind::V2);
    }
    Err(degenbot_value_error(
        pool_type.py(),
        format!(
            "Unsupported pool type: {}",
            pool_type.repr()?.to_string_lossy()
        ),
    )?)
}

/// Build a `degenbot.exceptions.base.DegenbotValueError(message=..)`.
///
/// Raises the driver's own exception type (not a bare `PyValueError`) so the
/// pool-type classification contract is unchanged by the move into Rust.
fn degenbot_value_error(py: Python<'_>, message: String) -> PyResult<PyErr> {
    let exc_type = py
        .import("degenbot.exceptions.base")?
        .getattr("DegenbotValueError")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("message", message)?;
    Ok(PyErr::from_value(exc_type.call((), Some(&kwargs))?))
}

/// Map a sequence of Python pool-table classes to the deduped [`PoolKind`] set.
///
/// The general `LiquidityPoolTable` base selects single-table-inheritance rows
/// for BOTH V2 and V3, so it expands to `{V2, V3}`.
///
/// # Errors
///
/// Returns `PyValueError` when any declared type has no known pool family.
#[pyfunction]
#[expect(clippy::needless_pass_by_value)]
pub fn classify_pool_kinds(pool_types: Vec<Bound<'_, PyAny>>) -> PyResult<HashSet<PoolKind>> {
    let mut kinds = HashSet::new();
    for pool_type in &pool_types {
        let pools = pool_type.py().import("degenbot.database.models.pools")?;
        if pool_type.is(&pools.getattr("LiquidityPoolTable")?) {
            kinds.insert(PoolKind::V2);
            kinds.insert(PoolKind::V3);
            continue;
        }
        let py_type = pool_type.cast::<pyo3::types::PyType>()?;
        if py_type.is_subclass(&pools.getattr("UniswapV4PoolTable")?)? {
            kinds.insert(PoolKind::V4);
        } else if py_type.is_subclass(&pools.getattr("UniswapV3PoolTableBase")?)? {
            kinds.insert(PoolKind::V3);
        } else if py_type.is_subclass(&pools.getattr("UniswapV2PoolTableBase")?)? {
            kinds.insert(PoolKind::V2);
        } else {
            let name = pool_type
                .getattr("__name__")
                .and_then(|n| n.extract::<String>())
                .unwrap_or_default();
            return Err(PyValueError::new_err(format!(
                "_resolve_pool_kinds cannot serve pool type '{name}': no known pool-kind mapping"
            )));
        }
    }
    Ok(kinds)
}

/// Convert the Python per-depth `set[type]` filter to typed [`PoolKind`] sets.
///
/// # Errors
///
/// Returns `PyValueError` when any declared type has no known pool family.
#[pyfunction]
pub fn convert_pool_type_filter(
    pool_type_per_depth: Option<Bound<'_, PyAny>>,
) -> PyResult<Option<Vec<Option<HashSet<PoolKind>>>>> {
    let Some(value) = pool_type_per_depth else {
        return Ok(None);
    };
    if value.is_none() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for depth in value.try_iter()? {
        let depth = depth?;
        if depth.is_none() {
            out.push(None);
            continue;
        }
        let mut allowed = HashSet::new();
        for pool_type in depth.try_iter()? {
            allowed.insert(classify_pool_kind(&pool_type?)?);
        }
        out.push(Some(allowed));
    }
    Ok(Some(out))
}

/// Assemble the traversal plan for a search request.
///
/// Returns one `(start_token_id, end_token_id, include_reverse, min_depth)`
/// entry per plan position, in product order with reverse pairs consolidated.
/// `filter_len` is the per-depth filter length (or `None`); when set it floors
/// the effective minimum depth at that length.
#[pyfunction]
#[must_use]
#[expect(clippy::needless_pass_by_value)]
pub fn prepare_traversal_plan(
    start_token_ids: Vec<u64>,
    end_token_ids: Vec<u64>,
    min_depth: usize,
    filter_len: Option<usize>,
) -> Vec<(u64, u64, bool, usize)> {
    degenbot_pathfinding::plan::prepare_traversal_plan(
        &start_token_ids,
        &end_token_ids,
        min_depth,
        filter_len,
    )
    .into_iter()
    .map(|traversal| {
        (
            traversal.start_token_id,
            traversal.end_token_id,
            traversal.include_reverse,
            traversal.min_depth,
        )
    })
    .collect()
}

/// Resolves raw `(pool_id, pool_kind)` hops into `PathStep` objects.
///
/// Owns the address lookups + the `kind_string -> concrete table class`
/// registry built once per graph, so neither crosses the FFI per path. The
/// step class is injected by the caller (the Python `PathStep` dataclass),
/// keeping the object's public shape Python-owned while the assembly lives
/// here.
#[pyclass(module = "degenbot._ffi")]
pub struct PathStepBuilder {
    /// Namespaced graph pool id → raw DB `kind` string.
    pool_id_to_kind_string: HashMap<u64, String>,
    /// Raw `kind` string → concrete table class.
    kind_string_to_class: HashMap<String, Py<PyAny>>,
    /// Family-base fallback classes, indexed V2/V3/V4.
    family_base: [Py<PyAny>; 3],
    /// V2/V3 pool id → checksummed address.
    v2v3_addresses: HashMap<u64, String>,
    /// V4 namespaced pool id → `(manager_address, pool_hash)`.
    v4_lookups: HashMap<u64, (String, String)>,
    /// The Python `PathStep` class to instantiate.
    step_cls: Py<PyAny>,
}

impl PathStepBuilder {
    /// Resolve the concrete table class for one hop.
    fn resolve_class(&self, py: Python<'_>, pool_id: u64, kind: PoolKind) -> Py<PyAny> {
        if let Some(kind_string) = self.pool_id_to_kind_string.get(&pool_id) {
            if let Some(class) = self.kind_string_to_class.get(kind_string) {
                return class.clone_ref(py);
            }
        }
        let index = match kind {
            PoolKind::V2 => 0,
            PoolKind::V3 => 1,
            PoolKind::V4 => 2,
        };
        self.family_base[index].clone_ref(py)
    }
}

#[pymethods]
impl PathStepBuilder {
    #[new]
    #[expect(clippy::needless_pass_by_value)]
    fn new(
        py: Python<'_>,
        pool_types: Vec<Py<PyAny>>,
        pool_id_to_kind_string: HashMap<u64, String>,
        v2v3_addresses: HashMap<u64, String>,
        v4_lookups: HashMap<u64, (String, String)>,
        step_cls: Py<PyAny>,
    ) -> PyResult<Self> {
        let mut kind_string_to_class = HashMap::new();
        for pool_type in &pool_types {
            let bound = pool_type.bind(py);
            let Ok(mapper) = bound.getattr("__mapper__") else {
                continue;
            };
            let Ok(identity) = mapper.getattr("polymorphic_identity") else {
                continue;
            };
            if identity.is_none() {
                continue;
            }
            if let Ok(kind_string) = identity.extract::<String>() {
                kind_string_to_class.insert(kind_string, pool_type.clone_ref(py));
            }
        }
        let pools = py.import("degenbot.database.models.pools")?;
        let family_base = [
            pools.getattr("UniswapV2PoolTableBase")?.unbind(),
            pools.getattr("UniswapV3PoolTableBase")?.unbind(),
            pools.getattr("UniswapV4PoolTable")?.unbind(),
        ];
        Ok(Self {
            pool_id_to_kind_string,
            kind_string_to_class,
            family_base,
            v2v3_addresses,
            v4_lookups,
            step_cls,
        })
    }

    /// Convert a raw `[(pool_id, pool_kind)]` path into `PathStep` objects.
    ///
    /// # Errors
    ///
    /// Returns `PyKeyError` when a hop's address is missing from the lookups.
    fn build(&self, py: Python<'_>, raw_path: Vec<(u64, PoolKind)>) -> PyResult<Vec<Py<PyAny>>> {
        let mut steps = Vec::with_capacity(raw_path.len());
        for (pool_id, kind) in raw_path {
            let class = self.resolve_class(py, pool_id, kind);
            if kind == PoolKind::V4 {
                let (manager_address, pool_hash) =
                    self.v4_lookups.get(&pool_id).ok_or_else(|| {
                        PyKeyError::new_err(format!("no V4 lookup for pool id {pool_id}"))
                    })?;
                steps.push(
                    self.step_cls
                        .call1(py, (manager_address.clone(), class, pool_hash.clone()))?,
                );
            } else {
                let address = self.v2v3_addresses.get(&pool_id).ok_or_else(|| {
                    PyKeyError::new_err(format!("no V2/V3 address for pool id {pool_id}"))
                })?;
                steps.push(
                    self.step_cls
                        .call1(py, (address.clone(), class, py.None()))?,
                );
            }
        }
        Ok(steps)
    }
}

/// Find arbitrage paths (cycles) through a liquidity-pool graph.
///
/// This is the Rust-backed DFS that replaces the Python ``networkx``-based
/// ``_dfs``. Returns a **lazy iterator** — paths are yielded one at a time,
/// so memory usage is bounded even for graphs that produce millions of paths.
///
/// Args:
///     edges: A list of ``(token0_id, token1_id, pool_id, pool_kind)`` tuples,
///         where ``pool_kind`` is a typed [`PoolKind`].
///     start_token_id: The token ID where the search begins.
///     end_token_id: The token ID the path must return to.
///     min_depth: Minimum number of hops in a completed path.
///     max_depth: Maximum number of hops, or ``None`` for no limit.
///     include_reverse: If ``True``, yield each found path again reversed.
///     pool_type_per_depth: Optional per-depth allowed pool kinds. A list where
///         each element is ``None`` (all kinds allowed) or a set of
///         [`PoolKind`] values. Implicitly caps ``max_depth`` at its length.
///
/// Returns:
///     A ``PathIterator`` — iterate it (``for path in iter: ...``) to lazily
///     yield paths, each a list of ``(pool_id, pool_kind)`` tuples.
#[must_use]
#[expect(clippy::implicit_hasher)]
#[pyfunction]
#[pyo3(signature = (
    edges,
    start_token_id,
    end_token_id,
    min_depth,
    max_depth,
    include_reverse,
    pool_type_per_depth=None,
))]
pub fn find_paths_rust(
    edges: Vec<(u64, u64, u64, PoolKind)>,
    start_token_id: u64,
    end_token_id: u64,
    min_depth: usize,
    max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<Vec<Option<HashSet<PoolKind>>>>,
) -> PathIterator {
    PathIterator {
        finder: build_owned_finder(
            edges,
            start_token_id,
            end_token_id,
            min_depth,
            max_depth,
            include_reverse,
            pool_type_per_depth,
        ),
        buffer: Vec::new(),
        batch_lens: Vec::new(),
        pool_keys: Vec::new(),
    }
}

/// Create a batched **async** iterator over the lazy arbitrage DFS (4IOEVT).
///
/// The async twin of [`find_paths_rust`]: it builds the same owning lazy DFS
/// but returns a [`PathBatchIterator`] whose `__anext__` yields up to
/// `batch_size` paths per call on the shared tokio runtime. Python's
/// `find_paths_async` drives it with `async for`, so the event loop is
/// suspended on a Rust future — no Python worker thread, queue, or stop flag.
///
/// Args:
///     edges: Same flat `(token0_id, token1_id, pool_id, pool_kind)` list as
///         [`find_paths_rust`].
///     start_token_id: The token ID where the search begins.
///     end_token_id: The token ID the path must return to.
///     min_depth: Minimum number of hops in a completed path.
///     max_depth: Maximum number of hops, or `None` for no limit.
///     include_reverse: If `True`, yield each found path again reversed.
///     pool_type_per_depth: Optional per-depth allowed pool kinds.
///     batch_size: Maximum number of paths per `__anext__` batch
///         (positive-clamped to `>= 1`).
///
/// Returns:
///     A `PathBatchIterator` — `async for batch in iter:` yields
///     `list[list[tuple[int, int]]]`; exhaustion raises
///     `StopAsyncIteration`. Dropping the iterator cancels a mid-search DFS.
#[must_use]
#[expect(clippy::implicit_hasher)]
#[expect(clippy::too_many_arguments)] // pyfunction surface mirrors the Python call 1:1
#[pyfunction]
#[pyo3(signature = (
    edges,
    start_token_id,
    end_token_id,
    min_depth,
    max_depth,
    include_reverse,
    pool_type_per_depth=None,
    batch_size=1000,
))]
pub fn find_paths_async_rust(
    edges: Vec<(u64, u64, u64, PoolKind)>,
    start_token_id: u64,
    end_token_id: u64,
    min_depth: usize,
    max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<Vec<Option<HashSet<PoolKind>>>>,
    batch_size: usize,
) -> PathBatchIterator {
    let cancel = Arc::new(AtomicBool::new(false));
    let finder = build_owned_finder(
        edges,
        start_token_id,
        end_token_id,
        min_depth,
        max_depth,
        include_reverse,
        pool_type_per_depth,
    )
    .with_cancel(Arc::clone(&cancel));
    PathBatchIterator::new(finder, cancel, batch_size)
}

/// Parse the flat int tuples + optional per-depth kind filter, build the
/// pruned `PathGraph`, and return the owning lazy DFS.
///
/// Shared by the sync [`find_paths_rust`] and async [`find_paths_async_rust`]
/// seams so both validate their arguments identically.
fn build_owned_finder(
    edges: Vec<(u64, u64, u64, PoolKind)>,
    start_token_id: u64,
    end_token_id: u64,
    min_depth: usize,
    max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<Vec<Option<HashSet<PoolKind>>>>,
) -> OwnedPathFinder {
    let rust_edges: Vec<(u64, u64, u64, CorePoolKind)> = edges
        .into_iter()
        .map(|(t0, t1, pid, kind)| (t0, t1, pid, kind.to_core()))
        .collect();

    let rust_filter: Option<Vec<Option<Vec<CorePoolKind>>>> = pool_type_per_depth.map(|raw| {
        raw.into_iter()
            .map(|allowed| allowed.map(|kinds| kinds.into_iter().map(PoolKind::to_core).collect()))
            .collect()
    });

    // Build the graph + create a lazy iterator. The graph is pruned and
    // node-valid-depths are computed inside OwnedPathFinder::new.
    let mut graph = degenbot_pathfinding::graph::PathGraph::from_edges(rust_edges);
    graph.prune_dead_ends();

    OwnedPathFinder::new(
        graph,
        start_token_id,
        end_token_id,
        min_depth,
        max_depth,
        include_reverse,
        rust_filter,
    )
}

/// Build the pathfinding edge list + address lookups via the Rust DB core
///
/// Choreographs `degenbot_db::fetch_tokens_with_min_degree` (the candidate-
/// token set, `degree` ≥ #requested pool kinds for a token to anchor a
/// cycle) → `fetch_path_graph_edges` (the bulk edge + address read) → the
/// candidate-token edge filter (mirrors Python `_prepare_graph`'s inline
/// `candidate_tokens` intersection). The filtered edge list is ready to pass
/// to `find_paths_rust`; the address maps (`v2v3_addresses`/`v4_lookups`)
/// + `pool_id_to_kind` reconstruct `PathStep`s in `_build_path_steps`.
///
/// This replaces Python's `_prepare_graph` + `_get_tokens_with_min_degree`
/// inline SQLAlchemy selects with a single GIL-released Rust pass. The DFS
/// half (`find_paths_rust`) + `_build_path_steps` shape are unchanged.
///
/// Args:
///     database_path: The SQLite file path (resolved by the Python caller
///         from `db._engine.url.database`).
///     chain_id: The chain ID to restrict pool + token queries.
///     pool_kinds: A set of `pool_kind` ints (`0` = V2, `1` = V3, `2` = V4).
///         Mirrors the `pool_types` input mapped via `_pool_kind_for_type`.
///     allowed_intermediate_token_ids: Optional set of token IDs; when set,
///         the candidate-token set is intersected with it before edge
///         filtering (mirrors Python's `allowed_token_ids` whitelist).
///
/// Returns:
///     A dict ``{``edges``, ``v2v3_addresses``, ``v4_lookups``,
///     ``pool_id_to_kind``, ``pool_id_to_kind_string``, ``candidate_tokens``}``:
///     - ``edges``: ``list[(token0_id, token1_id, pool_id, pool_kind)]``
///       for `find_paths_rust`, with typed [`PoolKind`] values.
///     - ``v2v3_addresses``: ``{pool_id: checksum_address_str}``.
///     - ``v4_lookups``: ``{pool_id: (manager_address_str, pool_hash_hex)}``.
///     - ``pool_id_to_kind``: ``{pool_id: pool_kind}`` for the DFS.
///     - ``pool_id_to_kind_string``: ``{pool_id: kind_str}`` — the raw
///       single-table-inheritance polymorphic identity (e.g.
///       `"uniswap_v3"`); Python rebuilds the concrete `PathStep.type`
///       class via `pool_type.__mapper__.polymorphic_identity`.
///     - ``candidate_tokens``: ``set[int]`` of candidate token IDs (after the
///       whitelist intersection) for caller diagnostics.
///
/// # Errors
///
/// Returns `PyValueError` if the DB cannot be opened or the bulk read fails
/// (mirrors `db_err_to_py`'s `ValueError` mapping for the snapshot seam).
#[cfg(all(feature = "pathfinding", feature = "db"))]
#[pyfunction]
#[expect(clippy::implicit_hasher, clippy::needless_pass_by_value)]
#[pyo3(signature = (database_path, chain_id, pool_kinds, allowed_intermediate_token_ids=None))]
pub fn build_path_graph<'py>(
    py: Python<'py>,
    database_path: &str,
    chain_id: i64,
    pool_kinds: HashSet<PoolKind>,
    allowed_intermediate_token_ids: Option<HashSet<u64>>,
) -> PyResult<Bound<'py, PyDict>> {
    let kinds: Vec<CorePoolKind> = pool_kinds.into_iter().map(PoolKind::to_core).collect();

    let (
        edges,
        v2v3_addresses,
        v4_lookups,
        pool_id_to_kind,
        pool_id_to_kind_string,
        candidate_tokens,
    ) = py
        .detach(|| -> Result<_, degenbot_db::DbError> {
            let db = DegenbotDb::open(Path::new(database_path))?.0;
            fetch_graph_data(
                &db,
                chain_id,
                &kinds,
                allowed_intermediate_token_ids.as_ref(),
            )
        })
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    build_graph_dict(
        py,
        &edges,
        &v2v3_addresses,
        &v4_lookups,
        &pool_id_to_kind,
        &pool_id_to_kind_string,
        &candidate_tokens,
    )
}

/// The Rust-side result of `fetch_graph_data`: the filtered edge list + the
/// three address/kind maps + the candidate-token set, ready for
/// `build_graph_dict` to wrap into Python types.
///
/// The address maps hold PRE-COMPUTED checksum strings (not `Address`) so
/// `build_graph_dict` does no keccak/EIP-55 work under the GIL — the
/// `to_checksum(None)` calls run inside the `py.detach` span in
/// `build_path_graph`: the keccak loop over tens of thousands
/// of V2/V3 addresses previously held the GIL for ~24 s, starving every
/// tokio worker that needs `PyGILState_Ensure` and triggering the dispatch
/// circular deadlock during the rolling-start `build_paths` overlap).
#[cfg(all(feature = "pathfinding", feature = "db"))]
type GraphBuildResult = (
    Vec<(u64, u64, u64, PoolKind)>,
    hashbrown::HashMap<u64, String>,
    hashbrown::HashMap<u64, (String, String)>,
    hashbrown::HashMap<u64, CorePoolKind>,
    hashbrown::HashMap<u64, String>,
    hashbrown::HashSet<u64>,
);

/// Run the candidate-token fetch + bulk edge read + candidate-token edge
/// filter inside the GIL-released span (mirrors Python `_prepare_graph`'s
/// `_get_tokens_with_min_degree` + per-pool-type select + candidate filter).
#[cfg(all(feature = "pathfinding", feature = "db"))]
fn fetch_graph_data(
    db: &DegenbotDb,
    chain_id: i64,
    kinds: &[CorePoolKind],
    allowed_intermediate_token_ids: Option<&HashSet<u64>>,
) -> Result<GraphBuildResult, degenbot_db::DbError> {
    // Candidate tokens: those appearing in ≥ `degree` pools across the
    // requested kinds (mirrors Python `_get_tokens_with_min_degree`).
    // `degree=2` matches the Python callers (a cycle needs ≥ 2 pools).
    let mut candidate_tokens = db.fetch_tokens_with_min_degree(chain_id, 2, kinds)?;
    if let Some(allowed) = allowed_intermediate_token_ids {
        candidate_tokens.retain(|t| allowed.contains(t));
    }

    // Bulk edge + address read (ALL chain-filtered edges; the candidate-token
    // filter is applied below).
    let data = db.fetch_path_graph_edges(chain_id, kinds)?;

    // Filter edges to those where BOTH tokens are candidate tokens (mirrors
    // Python `_prepare_graph`'s `candidate_tokens` intersection).
    let edges: Vec<(u64, u64, u64, PoolKind)> = data
        .edges
        .into_iter()
        .filter(|(t0, t1, _, _)| candidate_tokens.contains(t0) && candidate_tokens.contains(t1))
        .map(|(t0, t1, pid, kind)| (t0, t1, pid, PoolKind::from_core(kind)))
        .collect();

    // Pre-compute EIP-55 checksum strings for every V2/V3 pool address +
    // every V4 manager address inside this GIL-released span .
    // `Address::to_checksum(None)` is pure Rust (a keccak256 over the
    // lowercase-hex address) and does NOT need the GIL; doing it here keeps
    // `build_graph_dict`'s dict-build loop GIL-light (only `set_item` calls).
    // Previously `build_graph_dict` called `to_checksum` PER pool while
    // holding the GIL, a ~24 s keccak loop that starved tokio workers.
    let v2v3_checksums: hashbrown::HashMap<u64, String> = data
        .v2v3_addresses
        .iter()
        .map(|(pid, addr)| (*pid, addr.to_checksum(None)))
        .collect();
    let v4_checksums: hashbrown::HashMap<u64, (String, String)> = data
        .v4_lookups
        .iter()
        .map(|(pid, (mgr, hash))| (*pid, (mgr.to_checksum(None), hash.clone())))
        .collect();

    Ok((
        edges,
        v2v3_checksums,
        v4_checksums,
        data.pool_id_to_kind,
        data.pool_id_to_kind_string,
        candidate_tokens,
    ))
}

/// Build the Python return dict — the address maps use checksum strings
/// (the `Address` ↔ Python str boundary the snapshot seam already uses).
#[cfg(all(feature = "pathfinding", feature = "db"))]
fn build_graph_dict<'py>(
    py: Python<'py>,
    edges: &[(u64, u64, u64, PoolKind)],
    v2v3_addresses: &hashbrown::HashMap<u64, String>,
    v4_lookups: &hashbrown::HashMap<u64, (String, String)>,
    pool_id_to_kind: &hashbrown::HashMap<u64, CorePoolKind>,
    pool_id_to_kind_string: &hashbrown::HashMap<u64, String>,
    candidate_tokens: &hashbrown::HashSet<u64>,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    let edges_vec: Vec<(u64, u64, u64, PoolKind)> = edges.to_vec();
    out.set_item("edges", PyList::new(py, edges_vec)?)?;

    let v2v3 = PyDict::new(py);
    for (pid, addr) in v2v3_addresses {
        v2v3.set_item(pid, addr.as_str())?;
    }
    out.set_item("v2v3_addresses", v2v3)?;

    let v4 = PyDict::new(py);
    for (pid, (mgr, hash)) in v4_lookups {
        v4.set_item(pid, (mgr.as_str(), hash.as_str()))?;
    }
    out.set_item("v4_lookups", v4)?;

    let kind_map = PyDict::new(py);
    for (pid, kind) in pool_id_to_kind {
        kind_map.set_item(pid, PoolKind::from_core(*kind))?;
    }
    out.set_item("pool_id_to_kind", kind_map)?;

    // The raw `kind` STRING per pool — the single-table-inheritance
    // polymorphic identity (e.g. `"uniswap_v3"`, `"sushiswap_v2"`). Python
    // rebuilds the concrete `PathStep.type` class from it via
    // `pool_type.__mapper__.polymorphic_identity` (AF7OEL strict parity).
    let kind_str_map = PyDict::new(py);
    for (pid, kind_str) in pool_id_to_kind_string {
        kind_str_map.set_item(pid, kind_str.clone())?;
    }
    out.set_item("pool_id_to_kind_string", kind_str_map)?;

    let cand = pyo3::types::PySet::empty(py)?;
    for t in candidate_tokens {
        cand.add(t)?;
    }
    out.set_item("candidate_tokens", cand)?;

    Ok(out)
}

/// A lazy Python iterator over arbitrage paths.
///
/// Yields ``list[tuple[int, int]]`` — each path is a list of
/// ``(pool_id, pool_kind_u8)`` tuples. The DFS runs incrementally: each call
/// to ``__next__`` advances the search until a complete path is found.
///
/// To amortize the per-path FFI cost, the iterator internally buffers up to
/// [`BATCH_SIZE`] paths per GIL-released span (the DFS advances while the
/// GIL is released, so other Python threads may run between batches). Paths
/// are held as **flat compact pool indices** (not `EdgeKey`s) in a single
/// growable buffer — this avoids the per-path `Vec<EdgeKey>` allocation
/// (~96k small allocs for a typical search) and only converts indices →
/// `(pool_id, kind_u8)` lazily when building each Python list.
#[pyclass(module = "degenbot._ffi")]
pub struct PathIterator {
    finder: OwnedPathFinder,
    /// Flat pool-index buffer for the current batch. Consume from the back:
    /// the last `len` indices form the current path (length in
    /// `batch_lens`), then the buffer is truncated by `len`.
    buffer: Vec<u32>,
    /// Path lengths within `buffer`, in insert order; the back entry is the
    /// next path to serve.
    batch_lens: Vec<usize>,
    /// Lazily built, per-pool `(pool_id, kind_u8)` tuples. Tuples are
    /// immutable, so sharing one object across every path that traverses the
    /// pool is semantics-preserving and turns the per-hop conversion from a
    /// fresh tuple allocation into a reference bump.
    pool_keys: Vec<Option<Py<PyTuple>>>,
}

/// Number of paths to fetch per GIL-released span. Tuned so a batch is large
/// enough to amortize the GIL release (~µs) yet small enough to bound memory
/// (`BATCH_SIZE` paths × max-depth × 4 B ≈ 100 KB per batch) and give other
/// threads periodic slices.
const BATCH_SIZE: usize = 8192;

#[pymethods]
impl PathIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&mut self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        if self.batch_lens.is_empty() {
            // Refill the flat buffer in one GIL-released span — the DFS
            // advances (appending compact pool indices, no per-path Vec
            // allocation) while other Python threads may run.
            let finder = &mut self.finder;
            let buffer = &mut self.buffer;
            let batch_lens = &mut self.batch_lens;
            py.detach(|| {
                buffer.clear();
                batch_lens.clear();
                while batch_lens.len() < BATCH_SIZE {
                    match finder.next_path_indices_into(buffer) {
                        Some(len) => batch_lens.push(len),
                        None => break,
                    }
                }
            });
        }

        // Serve one path from the back of the flat buffer.
        let Some(len) = self.batch_lens.pop() else {
            return Ok(None);
        };
        let start = self.buffer.len() - len;
        // Resolve indices → shared cached (pool_id, kind_u8) tuples.
        let list = PyList::empty(py);
        for i in start..self.buffer.len() {
            let idx = self.buffer[i];
            if (idx as usize) >= self.pool_keys.len() || self.pool_keys[idx as usize].is_none() {
                self.materialize_pool_key(py, idx)?;
            }
            if let Some(key) = &self.pool_keys[idx as usize] {
                list.append(key.bind(py))?;
            }
        }
        self.buffer.truncate(start);
        Ok(Some(list))
    }
}

impl PathIterator {
    /// Materialize the cached tuple for `idx`, filling any gap below it.
    /// Amortized: only pools that appear on yielded paths cost a tuple.
    fn materialize_pool_key(&mut self, py: Python<'_>, idx: u32) -> PyResult<()> {
        materialize_pool_keys(&self.finder, py, &mut self.pool_keys, idx)
    }
}

/// Fill the shared per-pool tuple cache up to `idx`: only pools that appear
/// on yielded paths ever allocate their tuple.
fn materialize_pool_keys(
    finder: &OwnedPathFinder,
    py: Python<'_>,
    cache: &mut Vec<Option<Py<PyTuple>>>,
    idx: u32,
) -> PyResult<()> {
    let new_len = (idx as usize) + 1;
    cache.resize_with(new_len, || None);
    for (i, slot) in cache.iter_mut().enumerate() {
        if slot.is_none() {
            let pool_idx = u32::try_from(i).unwrap_or(u32::MAX);
            let (pool_id, pool_kind) = finder.pool_edge_key(pool_idx);
            let tuple = PyTuple::new(
                py,
                [
                    pool_id.into_pyobject(py)?.into_any(),
                    PoolKind::from_core(pool_kind).into_pyobject(py)?.into_any(),
                ],
            )?;
            *slot = Some(tuple.unbind());
        }
    }
    Ok(())
}

/// Mutable state carried across `__anext__` calls.
struct AsyncPathState {
    finder: OwnedPathFinder,
    /// Flat pool-index buffer for the current batch. Consume from the back:
    /// the last `len` indices form one path (length in `batch_lens`), then the
    /// buffer is truncated by `len` (mirrors `PathIterator`).
    buffer: Vec<u32>,
    /// Path lengths within `buffer`, in insert order.
    batch_lens: Vec<usize>,
    /// Lazily built shared `(pool_id, kind_u8)` tuples (see `PathIterator`).
    pool_keys: Vec<Option<Py<PyTuple>>>,
}

/// A batched **async** iterator over the lazy DFS (4IOEVT).
///
/// `__anext__` returns `list[list[tuple[int, int]]]`: up to `batch_size`
/// paths per call, computed on the shared tokio runtime with the GIL released
/// (the DFS refill runs in the pyo3-async future body, which is polled without
/// the GIL; only the Python list construction re-acquires it). Exhaustion
/// raises `StopAsyncIteration`.
///
/// The mutable search state lives behind an `Arc<Mutex<Option<..>>>` and is
/// taken for the duration of one `__anext__` (mirroring `BlockStream`'s
/// receiver take/put-back), so a single shared iterator survives across awaits.
/// Dropping the Python object sets the cooperative `cancel` flag: a consumer
/// that abandons the sweep (`aclose()` / GC) stops a mid-grind DFS at its next
/// loop iteration instead of pinning a tokio worker until the search ends.
#[pyclass(name = "PathBatchIterator", module = "degenbot._ffi")]
pub struct PathBatchIterator {
    state: Arc<parking_lot::Mutex<Option<AsyncPathState>>>,
    cancel: Arc<AtomicBool>,
    batch_size: usize,
}

impl PathBatchIterator {
    fn new(finder: OwnedPathFinder, cancel: Arc<AtomicBool>, batch_size: usize) -> Self {
        Self {
            state: Arc::new(parking_lot::Mutex::new(Some(AsyncPathState {
                finder,
                buffer: Vec::new(),
                batch_lens: Vec::new(),
                pool_keys: Vec::new(),
            }))),
            cancel,
            batch_size: batch_size.max(1),
        }
    }
}

impl Drop for PathBatchIterator {
    fn drop(&mut self) {
        // Cooperative cancel: the in-flight `__anext__` future checks this
        // between DFS advances, so a mid-search abandonment releases the Rust
        // iterator promptly instead of grinding out the remaining batches.
        self.cancel.store(true, Ordering::Release);
    }
}

#[pymethods]
impl PathBatchIterator {
    /// Return self as the async iterator.
    #[expect(clippy::missing_const_for_fn)]
    fn __aiter__(slf: PyClassGuard<'_, Self>) -> PyClassGuard<'_, Self> {
        slf
    }

    /// Await the next batch of paths (`list[list[tuple[int, int]]]`).
    ///
    /// Raises `StopAsyncIteration` when the DFS is exhausted (or after the
    /// owning iterator was dropped/cancelled). A producer failure raises here
    /// at the consumer.
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        let batch_size = self.batch_size;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut taken = state
                .lock()
                .take()
                .ok_or_else(|| PyStopAsyncIteration::new_err("pathfinding iterator exhausted"))?;

            // Refill the shared flat buffer only when it is drained, using the
            // SAME chunk size as the sync `PathIterator`. Both serve paths from
            // the back of the buffer, so the async stream reproduces the sync
            // stream's order exactly; a delivery batch is just the next
            // `batch_size` pops.
            if taken.batch_lens.is_empty() {
                let finder = &mut taken.finder;
                let buffer = &mut taken.buffer;
                let batch_lens = &mut taken.batch_lens;
                buffer.clear();
                batch_lens.clear();
                while batch_lens.len() < BATCH_SIZE {
                    match finder.next_path_indices_into(buffer) {
                        Some(len) => batch_lens.push(len),
                        None => break,
                    }
                }
            }

            if taken.batch_lens.is_empty() {
                // Exhausted (or cancelled): drop the finder, never put it back.
                return Err(PyStopAsyncIteration::new_err(
                    "pathfinding iterator exhausted",
                ));
            }

            // Build the batch under the GIL (indices -> shared cached
            // `(pool_id, kind_u8)` tuples; tuples are immutable, so sharing
            // one per pool is semantics-preserving).
            let finder = &taken.finder;
            let buffer = &mut taken.buffer;
            let batch_lens = &mut taken.batch_lens;
            let take = batch_size.min(batch_lens.len());
            let batch = Python::attach(|py| -> PyResult<Py<PyList>> {
                let out = PyList::empty(py);
                for _ in 0..take {
                    let Some(len) = batch_lens.pop() else {
                        break;
                    };
                    let start = buffer.len() - len;
                    let path = PyList::empty(py);
                    for &idx in &buffer[start..] {
                        if (idx as usize) >= taken.pool_keys.len()
                            || taken.pool_keys[idx as usize].is_none()
                        {
                            materialize_pool_keys(finder, py, &mut taken.pool_keys, idx)?;
                        }
                        if let Some(key) = &taken.pool_keys[idx as usize] {
                            path.append(key.bind(py))?;
                        }
                    }
                    buffer.truncate(start);
                    out.append(path)?;
                }
                Ok(out.unbind())
            })?;

            *state.lock() = Some(taken);
            Ok(batch)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use degenbot_pathfinding::graph::PathGraph;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Dropping the async iterator must set its cooperative cancel flag so an
    /// in-flight `__anext__` DFS can stop at its next advance.
    #[test]
    fn drop_sets_cancel_flag() {
        let graph = PathGraph::from_edges(vec![
            (1u64, 2u64, 100u64, CorePoolKind::V2),
            (2u64, 1u64, 200u64, CorePoolKind::V2),
        ]);
        let cancel = Arc::new(AtomicBool::new(false));
        let finder = OwnedPathFinder::new(graph, 1, 1, 2, Some(2), false, None)
            .with_cancel(Arc::clone(&cancel));
        let iterator = PathBatchIterator::new(finder, Arc::clone(&cancel), 4);
        assert!(!cancel.load(Ordering::Acquire));
        drop(iterator);
        assert!(
            cancel.load(Ordering::Acquire),
            "dropping the iterator must set the cancel flag"
        );
    }
}
