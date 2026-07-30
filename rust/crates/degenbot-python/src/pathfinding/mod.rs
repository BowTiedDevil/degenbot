//! `PyO3` bindings for the pathfinding graph + DFS.
//!
//! Thin translator over `degenbot_pathfinding::graph`. No business logic —
//! extract args (flat int tuples from Python) → build `OwnedPathFinder` →
//! yield paths lazily via the Python iterator protocol.
//!
//! `build_path_graph` (AF7OEL) choreographs the DB read
//! (`degenbot_db::fetch_path_graph_edges` and `fetch_tokens_with_min_degree`),
//! the candidate-token edge filter, and `PathGraph::from_edges`/
//! `prune_dead_ends`; the Python `_prepare_graph` becomes a delegating shell.
//! The DFS half (`find_paths_rust`) is unchanged.

#![allow(clippy::doc_markdown)]

use crate::prelude::*;
#[cfg(all(feature = "pathfinding", feature = "db"))]
// Note: `alloy::primitives::Address` is no longer named in this module
// after the ergo-66H3KJ GIL fix — the address maps are pre-computed to
// checksum STRINGS inside the `py.detach` span in `build_path_graph`, so
// `build_graph_dict` holds no `Address` values. Re-add the import if a
// downstream helper here regains an `Address`-typed surface.
#[cfg(all(feature = "pathfinding", feature = "db"))]
use degenbot_db::DegenbotDb;
use degenbot_pathfinding::graph::{OwnedPathFinder, PoolKind};
use pyo3::exceptions::PyValueError;
#[cfg(all(feature = "pathfinding", feature = "db"))]
use pyo3::types::PyDict;
use pyo3::types::PyList;
#[cfg(all(feature = "pathfinding", feature = "db"))]
use std::collections::HashSet;
#[cfg(all(feature = "pathfinding", feature = "db"))]
use std::path::Path;

/// Find arbitrage paths (cycles) through a liquidity-pool graph.
///
/// This is the Rust-backed DFS that replaces the Python ``networkx``-based
/// ``_dfs``. Returns a **lazy iterator** — paths are yielded one at a time,
/// so memory usage is bounded even for graphs that produce millions of paths.
///
/// Args:
///     edges: A list of ``(token0_id, token1_id, pool_id, pool_kind)`` tuples.
///         ``pool_kind`` is ``0`` for V2/V3 pools (``LiquidityPoolTable``) or
///         ``1`` for V4 pools (``UniswapV4PoolTable``).
///     start_token_id: The token ID where the search begins.
///     end_token_id: The token ID the path must return to.
///     min_depth: Minimum number of hops in a completed path.
///     max_depth: Maximum number of hops, or ``None`` for no limit.
///     include_reverse: If ``True``, yield each found path again reversed.
///     pool_type_per_depth: Optional per-depth allowed pool kinds. A list where
///         each element is ``None`` (all kinds allowed) or a set of
///         ``pool_kind`` ints (``0`` = V2V3, ``1`` = V4). Implicitly caps
///         ``max_depth`` at its length.
///
/// Returns:
///     A ``PathIterator`` — iterate it (``for path in iter: ...``) to lazily
///     yield paths, each a list of ``(pool_id, pool_kind)`` tuples.
///
/// # Errors
///
/// Returns `PyValueError` if any pool-kind discriminant is not 0 or 1.
#[allow(clippy::too_many_arguments, clippy::implicit_hasher)]
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
    edges: Vec<(u64, u64, u64, u8)>,
    start_token_id: u64,
    end_token_id: u64,
    min_depth: usize,
    max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<Vec<Option<std::collections::HashSet<u8>>>>,
) -> PyResult<PathIterator> {
    let rust_edges: Vec<(u64, u64, u64, PoolKind)> = edges
        .into_iter()
        .map(|(t0, t1, pid, kind_u8)| {
            let kind = PoolKind::from_u8(kind_u8).ok_or_else(|| {
                PyValueError::new_err(format!(
                    "pool_kind must be 0 (V2), 1 (V3), or 2 (V4), got {kind_u8}"
                ))
            })?;
            Ok((t0, t1, pid, kind))
        })
        .collect::<PyResult<Vec<_>>>()?;

    let rust_filter: Option<Vec<Option<Vec<PoolKind>>>> = match pool_type_per_depth {
        Some(raw) => Some(
            raw.into_iter()
                .map(|opt| {
                    opt.map(|kinds| {
                        kinds
                            .into_iter()
                            .map(|k| {
                                PoolKind::from_u8(k).ok_or_else(|| {
                                    PyValueError::new_err(format!(
                                        "pool_kind must be 0 (V2), 1 (V3), or 2 (V4), got {k}"
                                    ))
                                })
                            })
                            .collect::<PyResult<Vec<_>>>()
                    })
                    .transpose()
                })
                .collect::<PyResult<Vec<_>>>()?,
        ),
        None => None,
    };

    // Build the graph + create a lazy iterator. The graph is pruned and
    // node-valid-depths are computed inside OwnedPathFinder::new.
    let mut graph = degenbot_pathfinding::graph::PathGraph::from_edges(rust_edges);
    graph.prune_dead_ends();

    let finder = OwnedPathFinder::new(
        graph,
        start_token_id,
        end_token_id,
        min_depth,
        max_depth,
        include_reverse,
        rust_filter,
    );

    Ok(PathIterator {
        finder,
        buffer: Vec::new(),
        batch_lens: Vec::new(),
    })
}

/// Build the pathfinding edge list + address lookups via the Rust DB core
/// (AF7OEL).
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
///     - ``edges``: ``list[(token0_id, token1_id, pool_id, pool_kind_u8)]``
///       for `find_paths_rust`.
///     - ``v2v3_addresses``: ``{pool_id: checksum_address_str}``.
///     - ``v4_lookups``: ``{pool_id: (manager_address_str, pool_hash_hex)}``.
///     - ``pool_id_to_kind``: ``{pool_id: pool_kind_u8}`` for the DFS.
///     - ``pool_id_to_kind_string``: ``{pool_id: kind_str}`` — the raw
///       single-table-inheritance polymorphic identity (e.g.
///       `"uniswap_v3"`); Python rebuilds the concrete `PathStep.type`
///       class via `pool_type.__mapper__.polymorphic_identity`.
///     - ``candidate_tokens``: ``set[int]`` of candidate token IDs (after the
///       whitelist intersection) for caller diagnostics.
///
/// # Errors
///
/// Returns `PyValueError` if any `pool_kind` is not 0/1/2, the DB cannot be
/// opened, or the bulk read fails (mirrors `db_err_to_py`'s `ValueError`
/// mapping for the snapshot seam).
#[cfg(all(feature = "pathfinding", feature = "db"))]
#[pyfunction]
#[allow(clippy::implicit_hasher, clippy::needless_pass_by_value)]
#[pyo3(signature = (database_path, chain_id, pool_kinds, allowed_intermediate_token_ids=None))]
pub fn build_path_graph<'py>(
    py: Python<'py>,
    database_path: &str,
    chain_id: i64,
    pool_kinds: HashSet<u8>,
    allowed_intermediate_token_ids: Option<HashSet<u64>>,
) -> PyResult<Bound<'py, PyDict>> {
    let kinds: Vec<PoolKind> = pool_kinds
        .iter()
        .map(|&k| {
            PoolKind::from_u8(k).ok_or_else(|| {
                PyValueError::new_err(format!(
                    "pool_kind must be 0 (V2), 1 (V3), or 2 (V4), got {k}"
                ))
            })
        })
        .collect::<PyResult<Vec<_>>>()?;

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
/// `build_path_graph` (ergo 66H3KJ: the keccak loop over tens of thousands
/// of V2/V3 addresses previously held the GIL for ~24 s, starving every
/// tokio worker that needs `PyGILState_Ensure` and triggering the dispatch
/// circular deadlock during the rolling-start `build_paths` overlap).
#[cfg(all(feature = "pathfinding", feature = "db"))]
type GraphBuildResult = (
    Vec<(u64, u64, u64, u8)>,
    std::collections::HashMap<u64, String>,
    std::collections::HashMap<u64, (String, String)>,
    std::collections::HashMap<u64, PoolKind>,
    std::collections::HashMap<u64, String>,
    HashSet<u64>,
);

/// Run the candidate-token fetch + bulk edge read + candidate-token edge
/// filter inside the GIL-released span (mirrors Python `_prepare_graph`'s
/// `_get_tokens_with_min_degree` + per-pool-type select + candidate filter).
#[cfg(all(feature = "pathfinding", feature = "db"))]
fn fetch_graph_data(
    db: &DegenbotDb,
    chain_id: i64,
    kinds: &[PoolKind],
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
    let edges: Vec<(u64, u64, u64, u8)> = data
        .edges
        .into_iter()
        .filter(|(t0, t1, _, _)| candidate_tokens.contains(t0) && candidate_tokens.contains(t1))
        .map(|(t0, t1, pid, kind)| (t0, t1, pid, kind.as_u8()))
        .collect();

    // Pre-compute EIP-55 checksum strings for every V2/V3 pool address +
    // every V4 manager address inside this GIL-released span (ergo 66H3KJ).
    // `Address::to_checksum(None)` is pure Rust (a keccak256 over the
    // lowercase-hex address) and does NOT need the GIL; doing it here keeps
    // `build_graph_dict`'s dict-build loop GIL-light (only `set_item` calls).
    // Previously `build_graph_dict` called `to_checksum` PER pool while
    // holding the GIL, a ~24 s keccak loop that starved tokio workers.
    let v2v3_checksums: std::collections::HashMap<u64, String> = data
        .v2v3_addresses
        .iter()
        .map(|(pid, addr)| (*pid, addr.to_checksum(None)))
        .collect();
    let v4_checksums: std::collections::HashMap<u64, (String, String)> = data
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
    edges: &[(u64, u64, u64, u8)],
    v2v3_addresses: &std::collections::HashMap<u64, String>,
    v4_lookups: &std::collections::HashMap<u64, (String, String)>,
    pool_id_to_kind: &std::collections::HashMap<u64, PoolKind>,
    pool_id_to_kind_string: &std::collections::HashMap<u64, String>,
    candidate_tokens: &HashSet<u64>,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    let edges_vec: Vec<(u64, u64, u64, u8)> = edges.to_vec();
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
        kind_map.set_item(pid, kind.as_u8())?;
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
        // Resolve indices → (pool_id, kind_u8) via the finder's pools table.
        let list = PyList::empty(py);
        for i in start..self.buffer.len() {
            let (pool_id, pool_kind) = self.finder.pool_edge_key(self.buffer[i]);
            list.append((pool_id, pool_kind.as_u8()))?;
        }
        self.buffer.truncate(start);
        Ok(Some(list))
    }
}
