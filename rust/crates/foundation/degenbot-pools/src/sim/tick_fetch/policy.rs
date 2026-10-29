//! The tick-word **merge** policy (GLOSSARY **Tick-word merge**) — the
//! miss -> fetch -> checked-empty -> merge choreography every sparse tick-map
//! reader runs, written once beside the read seam ([`super::source`]).
//!
//! The read seam answers one word. What a caller does with that answer was
//! spelled five times across three crates; the three facts the choreography
//! needs are owned here instead:
//!
//! - a **reader** ([`TickWordReader`]) — one pool's read descriptor
//!   ([`TickWordKey`]) paired with its stored [`TickWordSource`], minted by the
//!   crate (a registered pool handle via `PoolEntry::tick_word_reader`, or an
//!   identity plus a source) and read through the staged plan below;
//! - a **merge target** ([`TickWordMergeTarget`]) — where the fetched word
//!   lands: a registered pool's tick map (nonce, known-word set and tick-range
//!   cache bookkeeping included) or a caller-owned [`TransientTickMap`] (a
//!   hypothetical read's overlay, which owns none of that);
//! - the **raced guard** — the target's fingerprint captured when the fetch is
//!   staged and re-checked when it installs, so a word fetched across a
//!   concurrent pool write is retried instead of clobbering fresher ticks.
//!
//! The checked-empty rule rides along: a source's `Ok(None)` (an all-zero
//! bitmap) is a CHECKED word — now known, holding no ticks — so no caller
//! re-derives that mapping.
//!
//! # The two-phase split
//!
//! The read is a multi-second RPC and a caller must never hold its state lock
//! across it, so the choreography is [`stage_word_fetch`] (needs the target:
//! capture the fingerprint) -> [`StagedWordFetch::fetch`] (needs nothing — the
//! lock is released) -> [`install_staged_word`] (needs the target again:
//! re-check the fingerprint, then merge). [`fetch_and_merge`] runs the same
//! three steps for a caller that owns its target exclusively, so no split buys
//! it anything.
//!
//! # Verdicts
//!
//! [`WordFetchOutcome`] is the ONE verdict vocabulary:
//! [`WordFetchOutcome::Merged`] / [`WordFetchOutcome::KnownEmpty`] (the word is
//! known now), [`WordFetchOutcome::Raced`] (restage and retry),
//! [`WordFetchOutcome::FetchFailed`] / [`WordFetchOutcome::NoSource`] (the miss
//! stands). A caller maps these onto its own refusal type; it never re-derives
//! them.

use std::sync::Arc;

use degenbot_math::concentrated_liquidity::functions::tick_position;
use hashbrown::HashMap;

use super::source::{FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource};
use crate::TickInfo;

/// The bitmap word position holding `tick` for `tick_spacing` — the ONE
/// word-position derivation.
///
/// The sparse miss set, the registration seed read, the ingress read and a
/// staged fetch all name a word through this, so a word position cannot be
/// computed two ways.
#[must_use]
pub fn word_of(tick: i32, tick_spacing: i32) -> i32 {
    i32::from(tick_position(tick.div_euclid(tick_spacing)).0)
}

/// One tick map's mutation fingerprint: its price clock, its tick count, and
/// the sum of its ticks' stamps.
///
/// The sum term is what makes the fingerprint a guard rather than a heuristic: a
/// same-block churn that adds one tick and removes another leaves the count
/// alone and moves the stamp sum.
pub type TickWordFingerprint = (u64, usize, u64);

/// The fingerprint of one tick map (see [`TickWordFingerprint`]).
#[must_use]
pub(crate) fn tick_map_fingerprint(
    update_block: u64,
    tick_data: &HashMap<i32, TickInfo>,
) -> TickWordFingerprint {
    (
        update_block,
        tick_data.len(),
        tick_data.values().map(|tick| tick.block).sum(),
    )
}

/// Where one fetched tick word lands — the seam the merge policy merges through,
/// so the policy never learns which kind of target it holds.
///
/// The two implementations are genuinely different targets, not two spellings of
/// one: a registered pool's tick map carries the nonce / known-word /
/// tick-range-cache bookkeeping the state verbs own, while a
/// [`TransientTickMap`] is a caller-owned map that dies with the call.
pub trait TickWordMergeTarget {
    /// The target's tick-mutation fingerprint, or `None` when this target cannot
    /// receive a word (a retired registration, or a
    /// non-concentrated-liquidity entry).
    fn tick_word_fingerprint(&self) -> Option<TickWordFingerprint>;

    /// Merge `fetched` into the target: its initialized ticks overlay the
    /// resident rows (per [`overlay_fetched_ticks`]) and the word becomes known.
    fn apply_fetched_word(&mut self, fetched: &FetchedTickWord);
}

/// The two-stamp overlay a fetched word's ticks land through: a resident row
/// stamped LATER than the fetched one wins.
///
/// A fetch can predate an event the pump applied while the read was in flight
/// (a retried fetch re-reads at its context block), so a fetched tick with an
/// older stamp must never regress the resident value — the race guard re-runs
/// the FETCH for that case, and this rule keeps the overlap honest even inside
/// one merge.
pub(crate) fn overlay_fetched_ticks(
    resident: &mut HashMap<i32, TickInfo>,
    fetched: &FetchedTickWord,
) {
    for (tick, info) in &fetched.ticks {
        let resident_is_fresher = resident
            .get(tick)
            .is_some_and(|resident| resident.block > info.block);
        if !resident_is_fresher {
            resident.insert(*tick, info.clone());
        }
    }
}

/// The transient merge target: a caller-owned tick map with no registered pool
/// behind it (a hypothetical / override read).
///
/// It owns none of a registered target's bookkeeping — no nonce to bump, no
/// known-word set to extend, no cached tick ranges to invalidate — because
/// nothing reads those off a map that dies with the call.
pub struct TransientTickMap<'a> {
    tick_data: &'a mut HashMap<i32, TickInfo>,
}

impl<'a> TransientTickMap<'a> {
    /// Address one caller-owned tick map as a merge target.
    #[must_use]
    pub fn new(tick_data: &'a mut HashMap<i32, TickInfo>) -> Self {
        Self { tick_data }
    }
}

impl TickWordMergeTarget for TransientTickMap<'_> {
    fn tick_word_fingerprint(&self) -> Option<TickWordFingerprint> {
        // A caller-owned map has no concurrent writer, so the guard only has to
        // be stable across one stage/install pair of the SAME call.
        Some(tick_map_fingerprint(0, self.tick_data))
    }

    fn apply_fetched_word(&mut self, fetched: &FetchedTickWord) {
        overlay_fetched_ticks(self.tick_data, fetched);
    }
}

/// One pool's tick-word read handle: its read descriptor plus the stored source,
/// cloned off the pool before a fetch so the read never borrows the state it is
/// fetching for.
#[derive(Clone, Debug)]
pub struct TickWordReader {
    key: TickWordKey,
    source: Arc<dyn TickWordSource>,
}

impl TickWordReader {
    /// Pair a pool's read descriptor with its stored source.
    #[must_use]
    pub(crate) fn new(key: TickWordKey, source: Arc<dyn TickWordSource>) -> Self {
        Self { key, source }
    }

    /// One word read with the checked-empty mapping applied.
    ///
    /// # Errors
    ///
    /// Propagates the source's [`TickWordFetchError`].
    pub(crate) fn fetch_word(
        &self,
        word: i32,
        block: u64,
    ) -> Result<CheckedWord, TickWordFetchError> {
        self.source
            .fetch_tick_word(&self.key, word, block)
            .map(|hit| checked_word(word, hit))
    }
}

/// One miss-path word read, with the checked-empty mapping applied.
#[derive(Debug, Clone)]
pub struct CheckedWord {
    /// The word's data; a checked-empty read carries an empty tick map.
    pub word: FetchedTickWord,
    /// The source answered `Ok(None)` — an all-zero bitmap, so the word is
    /// known-empty rather than merely tick-free.
    pub known_empty: bool,
}

/// The checked-empty mapping: a miss-path read's `Ok(None)` becomes a word that
/// is KNOWN and holds no ticks.
#[must_use]
pub fn checked_word(word: i32, hit: Option<FetchedTickWord>) -> CheckedWord {
    match hit {
        Some(fetched) => CheckedWord {
            word: fetched,
            known_empty: false,
        },
        None => CheckedWord {
            word: FetchedTickWord::new(word, HashMap::new()),
            known_empty: true,
        },
    }
}

/// The verdict of one fetch-and-merge attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordFetchOutcome {
    /// The fetched word's ticks merged into the target.
    Merged,
    /// The source's bitmap was all-zero: the word is known and holds no ticks.
    KnownEmpty,
    /// The target was mutated while the fetch ran — nothing merged, restage.
    Raced,
    /// The fetch failed (RPC, malformed return, word out of range).
    FetchFailed,
    /// No recovery source: no stored fetcher, or no merge target.
    NoSource,
}

impl WordFetchOutcome {
    /// Did the word become known (merged, with or without ticks)?
    #[must_use]
    pub fn known(self) -> bool {
        matches!(self, Self::Merged | Self::KnownEmpty)
    }
}

/// Which recovery posture a caller runs — the ONE arm/disarm vocabulary, so a
/// call site names its posture without carrying a source of its own.
#[derive(Clone, Copy)]
pub enum WordFetchPolicy<'a> {
    /// No fetch: the miss IS the caller's answer.
    Disarmed,
    /// Fetch the missing word through `reader`.
    Armed(&'a TickWordReader),
}

/// A staged fetch: the reader, the word, the fetch block, and the target
/// fingerprint captured while the caller still held the target.
///
/// [`Self::fetch`] needs no target at all — that is the point of staging, since
/// the read is a multi-second RPC and the caller's lock must not be held across
/// it.
#[derive(Clone, Debug)]
pub struct StagedWordFetch {
    reader: TickWordReader,
    word: i32,
    block: u64,
    fingerprint: TickWordFingerprint,
}

impl StagedWordFetch {
    /// The word this fetch was staged for.
    #[must_use]
    pub fn word(&self) -> i32 {
        self.word
    }

    /// The block this fetch reads at.
    #[must_use]
    pub fn block(&self) -> u64 {
        self.block
    }

    /// Run the fetch with NO target access, so the caller can release its lock
    /// first.
    ///
    /// # Errors
    ///
    /// Propagates the source's [`TickWordFetchError`].
    pub fn fetch(&self) -> Result<CheckedWord, TickWordFetchError> {
        self.reader.fetch_word(self.word, self.block)
    }
}

/// Stage one word's fetch: capture the target's fingerprint and pair the reader
/// with the word and block.
///
/// `None` when the policy carries no reader (nothing to fetch) or the target
/// cannot receive a word — the caller answers with its own refusal.
#[must_use]
pub fn stage_word_fetch<T: TickWordMergeTarget>(
    target: &T,
    word: i32,
    block: u64,
    policy: WordFetchPolicy<'_>,
) -> Option<StagedWordFetch> {
    let WordFetchPolicy::Armed(reader) = policy else {
        return None;
    };
    let fingerprint = target.tick_word_fingerprint()?;
    Some(StagedWordFetch {
        reader: reader.clone(),
        word,
        block,
        fingerprint,
    })
}

/// Install a staged fetch: re-check the target's fingerprint, then merge.
///
/// A fingerprint mismatch means the pool was written while the read was in
/// flight — the fetched overlay would clobber fresher ticks, so nothing merges
/// and the caller restages ([`WordFetchOutcome::Raced`]).
pub fn install_staged_word<T: TickWordMergeTarget>(
    target: &mut T,
    staged: &StagedWordFetch,
    fetched: &CheckedWord,
) -> WordFetchOutcome {
    match target.tick_word_fingerprint() {
        None => WordFetchOutcome::NoSource,
        Some(fingerprint) if fingerprint != staged.fingerprint => WordFetchOutcome::Raced,
        Some(_) => {
            target.apply_fetched_word(&fetched.word);
            if fetched.known_empty {
                WordFetchOutcome::KnownEmpty
            } else {
                WordFetchOutcome::Merged
            }
        }
    }
}

/// The whole choreography in one call: stage -> fetch -> install.
///
/// For a caller that owns its target exclusively (nothing can write it between
/// the three steps), so [`WordFetchOutcome::Raced`] is unreachable there. A
/// caller whose fetch releases a lock uses the split ([`stage_word_fetch`] /
/// [`StagedWordFetch::fetch`] / [`install_staged_word`]) instead.
pub fn fetch_and_merge<T: TickWordMergeTarget>(
    target: &mut T,
    word: i32,
    block: u64,
    policy: WordFetchPolicy<'_>,
) -> WordFetchOutcome {
    let Some(staged) = stage_word_fetch(target, word, block, policy) else {
        return WordFetchOutcome::NoSource;
    };
    match staged.fetch() {
        Ok(fetched) => install_staged_word(target, &staged, &fetched),
        Err(_) => WordFetchOutcome::FetchFailed,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used)]

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use alloy::primitives::{Address, U128};

    use super::*;

    /// A scripted merge target: an in-memory tick map plus the words it was
    /// handed, so a test can assert BOTH that a merge landed and that a refused
    /// install merged nothing.
    #[derive(Debug)]
    struct ScriptedTarget {
        clock: u64,
        tick_data: HashMap<i32, TickInfo>,
        words: Vec<i32>,
    }

    impl ScriptedTarget {
        fn new() -> Self {
            Self {
                clock: 0,
                tick_data: HashMap::new(),
                words: Vec::new(),
            }
        }
    }

    impl TickWordMergeTarget for ScriptedTarget {
        fn tick_word_fingerprint(&self) -> Option<TickWordFingerprint> {
            Some(tick_map_fingerprint(self.clock, &self.tick_data))
        }

        fn apply_fetched_word(&mut self, fetched: &FetchedTickWord) {
            self.words.push(fetched.word);
            overlay_fetched_ticks(&mut self.tick_data, fetched);
        }
    }

    /// One scripted source reply.
    #[derive(Debug)]
    enum Reply {
        /// The source answers this word.
        Word(FetchedTickWord),
        /// The source's bitmap was all-zero (`Ok(None)`).
        Miss,
        /// The read itself failed.
        Fail,
    }

    /// A scripted tick-word source: one queued reply per call, every call
    /// counted.
    #[derive(Debug)]
    struct ScriptedSource {
        replies: Mutex<VecDeque<Reply>>,
        calls: AtomicUsize,
    }

    fn source(replies: Vec<Reply>) -> Arc<dyn TickWordSource> {
        Arc::new(ScriptedSource {
            replies: Mutex::new(VecDeque::from(replies)),
            calls: AtomicUsize::new(0),
        })
    }

    impl TickWordSource for ScriptedSource {
        fn fetch_tick_word(
            &self,
            _key: &TickWordKey,
            word: i32,
            _block: u64,
        ) -> Result<Option<FetchedTickWord>, TickWordFetchError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let reply = self
                .replies
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front();
            match reply {
                Some(Reply::Word(mut fetched)) => {
                    fetched.word = word;
                    Ok(Some(fetched))
                }
                Some(Reply::Miss) => Ok(None),
                Some(Reply::Fail) | None => Err(TickWordFetchError::FetchFailed),
            }
        }
    }

    fn key() -> TickWordKey {
        TickWordKey::V3 {
            address: Address::new([0x11; 20]),
            tick_spacing: 10,
        }
    }

    fn reader(replies: Vec<Reply>) -> TickWordReader {
        TickWordReader::new(key(), source(replies))
    }

    fn tick(block: u64) -> TickInfo {
        TickInfo {
            liquidity_gross: U128::from(100u128),
            liquidity_net: 100i128,
            block,
        }
    }

    /// A fetched word carrying `ticks`, every row stamped `block`.
    fn fetched_at(ticks: &[i32], block: u64) -> FetchedTickWord {
        FetchedTickWord {
            word: 0,
            ticks: ticks.iter().map(|&t| (t, tick(block))).collect(),
            bitmap: None,
        }
    }

    #[test]
    fn word_of_is_the_one_word_position_derivation() {
        // What: the word half of the math package's bit-position helper, for
        // both signs and several spacings. The miss set, the seed read and a
        // staged fetch all name a word through `word_of`, so a divergent
        // derivation would make one of them fetch the wrong region.
        for (tick, spacing) in [
            (0, 1),
            (1, 1),
            (256, 1),
            (-1, 1),
            (-230, 10),
            (-60, 60),
            (120, 60),
            (15_360, 60),
            (887_272, 200),
        ] {
            assert_eq!(
                word_of(tick, spacing),
                degenbot_math::concentrated_liquidity::liquidity_mapping::get_tick_word_and_bit_position(
                    tick, spacing
                )
                .0,
                "tick {tick} spacing {spacing}"
            );
        }
    }

    #[test]
    fn fetch_and_merge_lands_the_word_and_its_ticks() {
        let mut target = ScriptedTarget::new();
        let reader = reader(vec![Reply::Word(fetched_at(&[60, 120], 99))]);

        let outcome = fetch_and_merge(&mut target, 0, 17, WordFetchPolicy::Armed(&reader));

        assert_eq!(outcome, WordFetchOutcome::Merged);
        assert_eq!(target.words, vec![0], "the word merged exactly once");
        let mut ticks: Vec<i32> = target.tick_data.keys().copied().collect();
        ticks.sort_unstable();
        assert_eq!(ticks, vec![60, 120], "every initialized tick landed");
    }

    #[test]
    fn checked_empty_word_becomes_known_with_no_ticks() {
        // What: a source `Ok(None)` is a CHECKED word, not a failed read — the
        // word merges as known and contributes no ticks, so the next read does
        // not ask for it again.
        let mut target = ScriptedTarget::new();
        let reader = reader(vec![Reply::Miss]);

        let outcome = fetch_and_merge(&mut target, 3, 99, WordFetchPolicy::Armed(&reader));

        assert_eq!(outcome, WordFetchOutcome::KnownEmpty);
        assert!(outcome.known());
        assert_eq!(target.words, vec![3]);
        assert!(target.tick_data.is_empty());
    }

    #[test]
    fn raced_install_merges_nothing() {
        // What: the raced guard. A pool written while the fetch was in flight
        // moves the fingerprint, so the install refuses rather than overlaying a
        // word fetched over superseded state.
        let mut target = ScriptedTarget::new();
        let reader = reader(vec![Reply::Word(fetched_at(&[60], 99))]);
        let staged =
            stage_word_fetch(&target, 0, 99, WordFetchPolicy::Armed(&reader)).expect("armed stage");

        // The pump applies an event for this pool during the fetch window.
        target.clock = 100;
        target.tick_data.insert(60, tick(100));

        let fetched = staged.fetch().expect("scripted fetch");
        let outcome = install_staged_word(&mut target, &staged, &fetched);

        assert_eq!(outcome, WordFetchOutcome::Raced);
        assert!(target.words.is_empty(), "a raced install merges nothing");
        assert_eq!(
            target.tick_data.get(&60).map(|t| t.block),
            Some(100),
            "the event-applied row is never regressed by the stale overlay"
        );
    }

    #[test]
    fn a_failed_fetch_merges_nothing() {
        let mut target = ScriptedTarget::new();
        let reader = reader(vec![Reply::Fail]);

        let outcome = fetch_and_merge(&mut target, 5, 99, WordFetchPolicy::Armed(&reader));

        assert_eq!(outcome, WordFetchOutcome::FetchFailed);
        assert!(!outcome.known());
        assert!(target.words.is_empty());
        assert!(target.tick_data.is_empty());
    }

    #[test]
    fn a_disarmed_policy_reports_no_source() {
        let mut target = ScriptedTarget::new();

        let outcome = fetch_and_merge(&mut target, 5, 99, WordFetchPolicy::Disarmed);

        assert_eq!(outcome, WordFetchOutcome::NoSource);
        assert!(!outcome.known());
        assert!(target.words.is_empty());
    }

    #[test]
    fn a_vanished_target_reports_no_source_and_installs_nothing() {
        // What: the target's fingerprint answers `None` once the registration is
        // gone, so an install staged before it vanished refuses instead of
        // writing into a retired pool.
        #[derive(Debug)]
        struct Vanished;

        impl TickWordMergeTarget for Vanished {
            fn tick_word_fingerprint(&self) -> Option<TickWordFingerprint> {
                None
            }

            fn apply_fetched_word(&mut self, _fetched: &FetchedTickWord) {
                unreachable!("a vanished target never receives a word")
            }
        }

        let target = ScriptedTarget::new();
        let reader = reader(vec![Reply::Word(fetched_at(&[60], 99))]);
        let staged =
            stage_word_fetch(&target, 0, 99, WordFetchPolicy::Armed(&reader)).expect("armed stage");
        let fetched = staged.fetch().expect("scripted fetch");

        assert_eq!(
            install_staged_word(&mut Vanished, &staged, &fetched),
            WordFetchOutcome::NoSource
        );
    }

    #[test]
    fn a_transient_target_overlays_the_two_stamp_rule() {
        // What: the transient merge target carries the same stamp discipline as
        // a registered one — a fetched row older than the resident row never
        // regresses it, and a row the transient does not hold lands.
        let mut tick_data: HashMap<i32, TickInfo> = HashMap::from_iter([(60, tick(200))]);
        let fetched = fetched_at(&[60, 120], 99);
        {
            let mut target = TransientTickMap::new(&mut tick_data);
            assert_eq!(target.tick_word_fingerprint(), Some((0, 1, 200)));
            target.apply_fetched_word(&fetched);
        }

        assert_eq!(
            tick_data.get(&60).map(|t| t.block),
            Some(200),
            "the fresher resident row wins"
        );
        assert_eq!(
            tick_data.get(&120).map(|t| t.block),
            Some(99),
            "a row the transient did not hold lands"
        );
    }
}
