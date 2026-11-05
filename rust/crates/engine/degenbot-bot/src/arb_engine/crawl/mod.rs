//! The Rust-owned registration crawl.
//!
//! `admission` is Registration admission itself — the ONE implementation of the
//! stage order (memo → build → eligibility → verify → register), answered as one
//! `CrawlReceipt` or one loud `CrawlAbort`. `unit` is the CRAWL adapter over it:
//! one submitted candidate in, the admission's receipt out. `policy` is the
//! path-composition policy as data: the core evaluates a candidate's composition
//! without holding a Python predicate. `receipt` is a candidate's terminal record
//! (identity, answering stage, outcome) and `profile` the measurement folded from
//! those records — the stage profile and the summary counters the crawl is
//! reported by. `flights` is the single-flight build table that keeps concurrent
//! seats from paying for the same construction twice. `discovery` is where
//! candidates come from (the DB graph reads, the traversal plan, the lazy DFS
//! sweep) and `controller` the crawl's own machine: the windowed submission
//! loop, the FIFO fold, the two latches, and the operator surface.
//!
//! The data flow is one line: `discovery` yields a `DiscoveredPath`, the
//! controller wraps it in a `CrawlUnit`, a fleet seat runs `unit::run_crawl_unit`
//! (the admission's crawl adapter) and answers a `CrawlReceipt`, and the
//! controller folds that receipt into the `profile::CrawlSummary` it reports.

pub mod admission;
pub mod controller;
pub mod discovery;
pub mod flights;
pub mod policy;
pub mod profile;
pub mod receipt;
pub mod unit;
