//! The tick-word capability: the read seam ([`source`]) plus the merge policy
//! ([`policy`]) every sparse tick-map reader runs (GLOSSARY **Tick-word
//! source**, GLOSSARY **Tick-word merge**).
//!
//! ONE trait serves both read call sites, addressed by a [`TickWordKey`] (the
//! pool's read descriptor) plus the word position:
//!
//! - the **registration-time Chain arm** of `assemble_{v3,v4}_tick_map` (the
//!   Store -> Db -> Chain precedence chain);
//! - the **live-pump miss path** during swap simulation
//!   (`v3_simulate_swap` / `v4_simulate_swap` -> `MissingTickWord` ->
//!   fetch -> merge -> retry).
//!
//! On top of that read sits the ONE merge policy: [`TickWordReader`] (the pool's
//! read handle), [`TickWordMergeTarget`] (where a fetched word lands), the
//! raced-fingerprint guard, and the checked-empty mapping. See [`policy`] for the
//! choreography and [`WordFetchOutcome`] for its verdict vocabulary.

pub mod policy;
pub mod source;

pub use policy::{
    checked_word, fetch_and_merge, install_staged_word, stage_word_fetch, word_of, CheckedWord,
    StagedWordFetch, TickWordFingerprint, TickWordMergeTarget, TickWordReader, TransientTickMap,
    WordFetchOutcome, WordFetchPolicy,
};
pub(crate) use policy::{overlay_fetched_ticks, tick_map_fingerprint};
pub use source::{FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource};
