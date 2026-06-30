//! `PyO3` bindings for the pathfinding graph + DFS.
//!
//! Thin translator over `degenbot_pathfinding::graph`. No business logic —
//! extract args (flat int tuples from Python) → build `OwnedPathFinder` →
//! yield paths lazily via the Python iterator protocol.

#![allow(clippy::doc_markdown)]

use crate::prelude::*;
use degenbot_pathfinding::graph::{OwnedPathFinder, PoolKind};
use pyo3::exceptions::PyValueError;
use pyo3::types::PyList;

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
#[pyclass]
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
        let len = match self.batch_lens.pop() {
            None => return Ok(None),
            Some(len) => len,
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
