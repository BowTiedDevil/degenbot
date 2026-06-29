//! Pure-Rust arbitrage pathfinding graph + depth-first search.
//!
//! This crate is a **zero-dependency leaf** — it has no `pyo3`, no `tokio`,
//! no `alloy`, no `degenbot-core`. The graph operates on plain `u64` token IDs
//! and pool IDs, making it independently testable without a Python
//! interpreter and reusable in non-Python Rust code. A standalone Rust
//! consumer can build a path graph and discover arbitrage cycles without
//! pulling the engine, the pump, or the RPC stack.
//!
//! # Design
//!
//! The graph is a multigraph: nodes are token IDs and edges are liquidity
//! pools, identified by `(pool_id, pool_kind)`. Parallel edges (multiple
//! pools connecting the same token pair) are naturally supported. The
//! [`PathGraph`] stores an adjacency list (`HashMap<u64, Vec<Edge>>`)
//! preserving edge insertion order for deterministic DFS traversal.
//!
//! The [`PathGraph::find_paths`] method performs an iterative depth-first
//! search for all valid cycles from a start token back to an end token,
//! honoring minimum/maximum depth bounds, per-depth pool-type filters, and
//! lookahead pruning via precomputed node valid-depth sets.
//!
//! # Relationship to the Python module
//!
//! The Python `degenbot.pathfinding` module handles database orchestration
//! (`SQLAlchemy` pool/token queries, address resolution) and calls into this
//! crate via the `PyO3` binding layer. Address resolution stays in Python
//! (where `SQLAlchemy` lives); the graph algorithm lives here.

pub mod graph;

pub use graph::{Edge, EdgeKey, OwnedPathFinder, PathFinder, PathGraph, PoolKind};
