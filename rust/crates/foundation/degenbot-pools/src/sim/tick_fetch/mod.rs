//! The tick-word read seam for sparse tick-map reads (GLOSSARY
//! **Tick-word source**).

//! ONE trait serves both callsites (the two-trait stopgap is gone):

//! - [`source::TickWordSource`] — the **registration-time Chain arm** of
//!   `assemble_{v3,v4}_tick_map` (the Store -> Db -> Chain precedence chain)
//!   AND the **live-pump miss path** during swap simulation
//!   (`v3_simulate_swap` / `v4_simulate_swap` -> `MissingTickWord`). The
//!   read is addressed by a [`source::TickWordKey`] (the pool's read
//!   descriptor) plus the word position; both call sites hold the
//!   descriptor (the registration arm from the pool being assembled, the
//!   miss path from the registered pool's own identity).

pub mod miss;

pub mod source;

pub use miss::{FetchTickWordError, TickWordFetcher};

pub use source::{FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource};
