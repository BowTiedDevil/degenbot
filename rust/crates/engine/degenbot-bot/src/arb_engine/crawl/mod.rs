//! The Rust-owned registration crawl.
//!
//! `policy` is the path-composition policy as data: the core evaluates a
//! candidate's composition without holding a Python predicate. `receipt` is a
//! unit's terminal record (identity, answering stage, outcome) and `profile`
//! the measurement folded from those records — the stage profile and the
//! summary counters the crawl is reported by.

pub mod policy;
pub mod profile;
pub mod receipt;
