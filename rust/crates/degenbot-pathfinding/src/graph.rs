//! The pathfinding multigraph and iterative depth-first search.
//!
//! This module is the pure-Rust core of the pathfinding algorithm. It
//! replaces the Python `networkx.MultiGraph` + recursive `_dfs` with a lean
//! adjacency-list graph and an iterative DFS using a `HashSet` visited-set
//! for O(1) cycle detection.
