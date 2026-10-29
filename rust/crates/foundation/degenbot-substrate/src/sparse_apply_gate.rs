//! The strict sparse-word apply gate's fetch choreography.
//!
//! The routing table's direct-apply arm refuses a tick mutation whose boundary
//! bitmap word the pool has not checked (`ApplyOutcome::MissingTickWord`,
//! armed by `pump.strict_sparse_apply`) instead of applying over an unknown
//! region — see `degenbot_pools::families::concentrated_liquidity::state_seeding::unknown_boundary_word`
//! for why applying corrupts the reorg journal's per-tick priors.
//!
//! The router cannot fetch the word itself: it is called under the state write
//! guard, and the fetch is a multi-second RPC. This module owns the half the
//! router cannot — the stage → fetch → install split of the tick-word merge
//! policy (GLOSSARY **Tick-word merge**), whose two short locks the router's
//! caller owns:
//!
//! 1. short write — clone the pool's read handle and capture its
//!    tick-mutation fingerprint (`stage_word_fetch_by_pool_id`),
//! 2. fetch — NO state lock held,
//! 3. short write — install, merging only if the pool was not mutated during
//!    the fetch (`install_word_fetch`),
//! 4. re-route the SAME event, which now applies.
//!
//! A `Raced` install (the pool advanced during the fetch window) retries the
//! stage+fetch rather than clobbering fresher writes; a fetch failure (no
//! stored fetcher, or the fetcher's error) leaves the router's typed refusal
//! standing. The event is then NOT applied — never applied over an unknown
//! region, and never silently dropped: the caller sees the refusal.

use degenbot_pools::WordFetchOutcome;

use crate::concentrated_liquidity_route::ApplyOutcome;
use crate::state_lock::LockSite;
use crate::state_lock::StateLock;
use crate::BotState;

/// Bounded attempts per word: a race requires a same-pool event to land inside
/// the (multi-second) fetch window, so three attempts exhaust only pathological
/// bursts.
const MAX_WORD_ATTEMPTS: u8 = 3;

/// Bounded attempts per event: two boundary words, plus one tolerant re-try for
/// a second-install race.
const MAX_EVENT_ATTEMPTS: u8 = 3;

/// Route one decoded event, satisfying the strict sparse-word gate by fetching
/// and installing the refused boundary word(s) between attempts.
///
/// `apply` performs one routing pass (the event's own `apply`); `fetch_block`
/// is the state context the word is fetched at — the event's block minus one,
/// the state the event applies OVER (the same context the retired Python gate
/// fetched at).
pub fn route_with_word_fetch(
    state: &StateLock<BotState>,
    fetch_block: u64,
    apply: impl Fn(&mut BotState) -> ApplyOutcome,
) -> ApplyOutcome {
    let mut outcome = apply(&mut state.write_at(LockSite::Core));
    for _ in 0..MAX_EVENT_ATTEMPTS {
        let ApplyOutcome::MissingTickWord { pool_id, word } = outcome else {
            return outcome;
        };
        if !backfill_word(state, pool_id, word, fetch_block) {
            // The refusal stands: the word could not be backfilled, so the
            // event is never applied over the unknown region.
            return outcome;
        }
        outcome = apply(&mut state.write_at(LockSite::Core));
    }
    outcome
}

/// Backfill one unknown bitmap word: stage (short write) → fetch (no lock) →
/// install (short write), retrying a lost install race. `false` when no fetcher
/// is stored or the fetch failed.
fn backfill_word(state: &StateLock<BotState>, pool_id: u64, word: i32, fetch_block: u64) -> bool {
    for attempt in 0..MAX_WORD_ATTEMPTS {
        let Some(staged) = state.write_at(LockSite::Core).stage_word_fetch_by_pool_id(
            pool_id,
            word,
            fetch_block,
            attempt > 0,
        ) else {
            return false;
        };
        // NO state lock held across the fetch.
        let Ok(fetched) = staged.fetch() else {
            return false;
        };
        match state
            .write_at(LockSite::Core)
            .install_word_fetch(&staged, &fetched)
        {
            WordFetchOutcome::Merged | WordFetchOutcome::KnownEmpty => return true,
            WordFetchOutcome::FetchFailed | WordFetchOutcome::NoSource => return false,
            WordFetchOutcome::Raced => {}
        }
    }
    false
}
