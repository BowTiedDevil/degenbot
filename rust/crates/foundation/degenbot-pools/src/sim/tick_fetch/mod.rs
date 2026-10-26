//! The tick-word read seam for sparse tick-map reads (GLOSSARY
//! **Tick-word source**).

//! ONE trait serves both callsites (the two-trait stopgap is gone). The
//! read is addressed by a [`TickWordKey`] (the pool's read descriptor) plus
//! the word position; both call sites hold the descriptor — the registration
//! arm from the pool being assembled, the miss path from the registered
//! pool's own identity. See [`source`] for the trait's locking and
//! sync-bridging contract. The two callsites:

//! - the **registration-time Chain arm** of `assemble_{v3,v4}_tick_map` (the
//!   Store -> Db -> Chain precedence chain);
//! - the **live-pump miss path** during swap simulation
//!   (`v3_simulate_swap` / `v4_simulate_swap` -> `MissingTickWord` ->
//!   fetch -> merge -> retry).

pub mod source;

pub use source::{FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource};
