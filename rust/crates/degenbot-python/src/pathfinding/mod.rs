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

    Ok(PathIterator { finder })
}

/// A lazy Python iterator over arbitrage paths.
///
/// Yields ``list[tuple[int, int]]`` — each path is a list of
/// ``(pool_id, pool_kind_u8)`` tuples. The DFS runs incrementally: each call
/// to ``__next__`` advances the search until a complete path is found.
#[pyclass]
pub struct PathIterator {
    finder: OwnedPathFinder,
}

#[pymethods]
impl PathIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&mut self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        // Run the next DFS step with the GIL released. Each step is cheap
        // (one edge exploration or one yield), so the GIL is held briefly.
        let result = py.detach(|| self.finder.next_path());

        match result {
            None => Ok(None),
            Some(path) => {
                let list = PyList::empty(py);
                for (pool_id, pool_kind) in &path {
                    let pool_id_py: Bound<'_, PyAny> =
                        pool_id.into_pyobject(py).unwrap().into_any();
                    let kind_py: Bound<'_, PyAny> =
                        pool_kind.as_u8().into_pyobject(py).unwrap().into_any();
                    list.append((pool_id_py, kind_py).into_pyobject(py)?)?;
                }
                Ok(Some(list))
            }
        }
    }
}
